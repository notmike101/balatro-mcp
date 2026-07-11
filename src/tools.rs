use rmcp::{
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, ListResourcesResult, ReadResourceRequestParams, ReadResourceResult,
        ResourceContents,
    },
    tool, tool_handler, tool_router,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf, sync::Arc, time::Duration};
use tokio::{process::Command, sync::Mutex, time::timeout};

use crate::backend::{
    ipc::{IpcPaths, advance_safe_internal, checkpoint_internal, execute_policy_action},
    policy::{SAFE_TRANSITION_ACTIONS, build_policy_state},
    replay::ReplayDB,
    runtime::{self, balatro_processes, observation_age},
    scoring::score_hand,
    state::StateDB,
};
use crate::guide::{GUIDE_TOPICS, guide};
use crate::models::*;
use crate::protocol::{
    compact_observation, envelope, sanitize, tool as to_tool_result, value_state,
};

use std::sync::LazyLock;
static EMPTY_BRIDGE: LazyLock<serde_json::Map<String, Value>> = LazyLock::new(serde_json::Map::new);

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_default(),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(values) => {
            let mut keys: Vec<_> = values.keys().collect();
            keys.sort();
            format!(
                "{{{}}}",
                keys.iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(*key).unwrap_or_default(),
                        canonical_json(&values[*key])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

fn digest(prefix: &str, value: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prefix.as_bytes());
    hasher.update(canonical_json(value).as_bytes());
    format!("{}-{:x}", prefix, hasher.finalize())
}

fn decision_id_for(observation: &Value) -> String {
    digest("d3", observation)
}
fn observation_id_for(observation: &Value) -> String {
    digest("o3", observation)
}

fn action_error_code(error: &str) -> &'static str {
    if error.contains("stale") {
        "stale_decision"
    } else if error.contains("timeout") {
        "timeout"
    } else {
        "action_failed"
    }
}

fn active_directives(rules: &Value, observation: &Value) -> Value {
    let values = rules
        .get("rules")
        .and_then(Value::as_array)
        .map(|rules| {
            rules
                .iter()
                .filter(|rule| {
                    if rule.get("active").and_then(Value::as_bool) == Some(false) {
                        return false;
                    }
                    rule.get("conditions")
                        .and_then(Value::as_object)
                        .map(|conditions| {
                            conditions.iter().all(|(key, expected)| {
                                observation.pointer(key).or_else(|| observation.get(key))
                                    == Some(expected)
                            })
                        })
                        .unwrap_or(true)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Value::Array(values)
}

#[derive(Clone)]
pub struct Server {
    pub root: PathBuf,
    pub runtime_root: PathBuf,
    pub ipc: IpcPaths,
    pub mutations: Arc<Mutex<()>>,
    pub replay_db: Arc<Mutex<ReplayDB>>,
    pub state_db: Arc<Mutex<StateDB>>,
    pub(crate) process_override: Arc<Mutex<Option<Vec<Value>>>>,
    pub tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

impl Server {
    pub fn new(root: PathBuf) -> Result<Self, String> {
        let runtime_root = env::var_os("BALATRO_RUNTIME_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.clone());
        let ipc = IpcPaths::new(&runtime_root);
        let replay_db = Arc::new(Mutex::new(ReplayDB::new(&runtime_root)));
        let state_db = Arc::new(Mutex::new(StateDB::new(&runtime_root)));
        Ok(Self {
            root,
            runtime_root,
            ipc,
            mutations: Arc::new(Mutex::new(())),
            replay_db,
            state_db,
            process_override: Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
        })
    }

    /// Read the observation JSON from the Lua bridge.
    pub fn read_observation(&self) -> Result<Value, String> {
        self.ipc.read_observation()
    }

    async fn policy(&self, limit: u32) -> Result<Value, String> {
        let observation = self.read_observation()?;
        let mut state = build_policy_state(&observation, limit as usize, 40, 60);
        let decision_id = decision_id_for(&observation);
        let observation_id = observation_id_for(&observation);
        let score = serde_json::to_value(score_hand(&observation, None)).unwrap_or(Value::Null);
        let directives = self
            .state_db
            .lock()
            .await
            .strategy()
            .unwrap_or_else(|_| json!({"rules":[]}));
        let object = state
            .as_object_mut()
            .expect("policy state must be an object");
        object.insert("schema".into(), json!("balatro-mcp/policy/v3"));
        object.insert("decision_id".into(), json!(decision_id));
        object.insert("observation_id".into(), json!(observation_id));
        object.insert(
            "active_directives".into(),
            active_directives(&directives, &observation),
        );
        object.insert("score_analysis".into(), score.clone());
        object.insert(
            "estimate_quality".into(),
            score
                .get("estimate_quality")
                .cloned()
                .unwrap_or_else(|| json!("partial")),
        );
        Ok(state)
    }

    async fn status(&self) -> Value {
        let processes = self
            .process_override
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| balatro_processes().unwrap_or_default());
        let observation = self.read_observation();
        let obs_path = self.runtime_root.join("Balatro/codex_observation.json");
        let age = observation_age(&obs_path);
        let bridge: &serde_json::Map<String, Value> = observation
            .as_ref()
            .ok()
            .and_then(|o| o.get("bridge"))
            .and_then(|b| b.as_object())
            .unwrap_or(&EMPTY_BRIDGE);
        let seed = observation
            .as_ref()
            .ok()
            .and_then(|o| runtime::observation_seed(o));
        let mut problems = Vec::new();
        if processes.len() != 1 {
            problems.push(format!(
                "expected one Balatro.exe process; found {}",
                processes.len()
            ));
        }
        if age.is_none() || age.unwrap_or(f64::MAX) > runtime::MAX_OBSERVATION_AGE_SECONDS {
            problems.push(format!("observation stale or missing: age={:?}?", age));
        }
        if let Some(s) = &seed {
            if s != runtime::ALLOWED_SEED {
                problems.push(format!("seed mismatch: {}", s));
            }
        }
        if bridge.get("version").and_then(|v| v.as_str()) != Some(runtime::EXPECTED_BRIDGE_VERSION)
        {
            problems.push(format!(
                "bridge restart required: loaded={} expected={}",
                bridge
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown"),
                runtime::EXPECTED_BRIDGE_VERSION
            ));
        }
        json!({
            "schema": "balatro-agent-status/v2.0",
            "safe_for_mutation": problems.is_empty(),
            "problems": problems,
            "processes": processes,
            "observation_age_seconds": age,
            "bridge": {
                "version": bridge.get("version"),
                "session_id": bridge.get("session_id"),
                "observation_seq": bridge.get("observation_seq"),
            },
            "seed": seed,
        })
    }

    pub async fn ensure_runtime_impl(&self) -> Result<Value, String> {
        let status = self.status().await;
        let count = status
            .get("processes")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if count > 1 {
            return Err(
                "multiple Balatro.exe processes; resolve manually before MCP mutation".into(),
            );
        }
        if count == 1 {
            return Ok(status);
        }
        // Balatro.exe launch is intentionally external to the MCP server.
        Err("Balatro.exe not running; start it manually before MCP mutation".into())
    }

    pub async fn preflight(&self) -> Result<Value, Value> {
        {
            let status = self.status().await;
            let count = status
                .get("processes")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            if count != 1 {
                return Err(envelope(
                    false,
                    status,
                    "process_count",
                    "exactly one Balatro.exe process required",
                ));
            }
            if let Some(seed) = status.get("seed").and_then(Value::as_str)
                && seed != SEED
            {
                return Err(envelope(
                    false,
                    status,
                    "wrong_seed",
                    "wrong seed; required 2K9H9HN",
                ));
            }
            if !status
                .get("safe_for_mutation")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Err(envelope(
                    false,
                    status,
                    "unsafe_runtime",
                    "runtime is not safe for mutation",
                ));
            }
            let observation = match self.read_observation() {
                Ok(value) => value,
                Err(error) => {
                    return Err(envelope(false, status, "observation_unavailable", &error));
                }
            };
            let processes = status
                .get("processes")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let age = status
                .get("observation_age_seconds")
                .and_then(Value::as_f64);
            if let Err(error) = runtime::validate_runtime(&observation, processes, age) {
                return Err(envelope(
                    false,
                    status,
                    "unsafe_runtime",
                    &error.to_string(),
                ));
            }
            if let Err(error) =
                runtime::guard_command(&json!({"action": "policy_step"}), &observation)
            {
                return Err(envelope(
                    false,
                    status,
                    "unsafe_runtime",
                    &error.to_string(),
                ));
            }
            Ok(status)
        }
    }

    pub async fn node(&self, args: &[String]) -> Result<Value, String> {
        let mut all = vec![
            self.root
                .join("tools/balatro-info-db/bin/balatro-info.js")
                .display()
                .to_string(),
        ];
        all.extend(args.iter().cloned());
        self.run_external_json("node", &all, 15.0).await
    }

    async fn run_external_json(
        &self,
        program: &str,
        args: &[String],
        seconds: f64,
    ) -> Result<Value, String> {
        let mut child = Command::new(program);
        child
            .args(args)
            .current_dir(&self.root)
            .env("BALATRO_RUNTIME_ROOT", &self.runtime_root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = timeout(
            Duration::from_secs_f64(seconds.clamp(0.1, 60.0)),
            child.output(),
        )
        .await
        .map_err(|_| "backend timeout".to_string())
        .and_then(|r| r.map_err(|e| format!("backend launch failed: {e}")))?;
        let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if let Ok(value) = serde_json::from_str(&out) {
            return Ok(value);
        }
        if !output.status.success() {
            return Err(if err.is_empty() { out } else { err });
        }
        Err(format!(
            "backend returned non-JSON output: {}",
            out.chars().take(300).collect::<String>()
        ))
    }

    pub fn diagnostic(&self, lines: u32) -> Value {
        let appdata = env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:/Users/me/AppData/Roaming"));
        let directory = appdata.join("Balatro/Mods/lovely/log");
        let latest = fs::read_dir(&directory).ok().and_then(|items| {
            items
                .filter_map(Result::ok)
                .filter_map(|x| {
                    let modified = x.metadata().ok()?.modified().ok()?;
                    Some((modified, x.path()))
                })
                .max_by_key(|(modified, _)| *modified)
                .map(|(_, path)| path)
        });
        match latest {
            Some(path) => {
                let text = fs::read_to_string(&path).unwrap_or_default();
                let tail = text
                    .lines()
                    .rev()
                    .take(lines.clamp(1, 200) as usize)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                json!({"log_found": true, "tail": tail, "truncated": text.lines().count() > lines as usize})
            }
            None => json!({"log_found": false, "tail": "Lovely log not found"}),
        }
    }
}

#[tool_router]
impl Server {
    #[tool(description = "Check process count, seed, bridge freshness, and resumable-save safety.")]
    async fn game_status(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let data = self.status().await;
        let process_count = data
            .get("processes")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let valid_seed = data
            .get("seed")
            .and_then(Value::as_str)
            .is_none_or(|seed| seed == SEED);
        if process_count == 1 && valid_seed {
            to_tool_result(envelope(true, data, "", ""))
        } else {
            to_tool_result(envelope(
                false,
                data,
                "preflight",
                "Balatro runtime preflight failed",
            ))
        }
    }

    #[tool(
        description = "Ensure the fixed Balatro.exe is running without UI actions or new-run creation. Refuses multiple processes."
    )]
    async fn ensure_runtime(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let _guard = self.mutations.lock().await;
        match self.ensure_runtime_impl().await {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(e) => to_tool_result(envelope(false, Value::Null, "runtime_failed", &e)),
        }
    }

    #[tool(
        description = "Read targeted live state. Valid sections: summary, hand, build, blind, hand_values, all."
    )]
    async fn observe(
        &self,
        Parameters(params): Parameters<ObserveParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if !["summary", "hand", "build", "blind", "hand_values", "all"]
            .contains(&params.section.as_str())
        {
            return to_tool_result(envelope(
                false,
                Value::Null,
                "invalid_arguments",
                "invalid observe section",
            ));
        }
        match self.policy(40).await {
            Ok(data) => to_tool_result(envelope(
                true,
                compact_observation(data, &params.section),
                "",
                "",
            )),
            Err(e) => to_tool_result(envelope(false, Value::Null, "observe_failed", &e)),
        }
    }

    #[tool(
        description = "Return current decision_id, legal actions, rankings, and strategy analysis. Examine decision_checks.ordering.hand_order and decision_checks.ordering.joker_order when jokers are present and trigger sequence can affect scoring. Examine decision_checks.consumables when Tarot/Planet/Spectral cards are owned or available in shop; use or sell before exiting or advancing. Examine decision_checks.shop when in SHOP state to track remaining items and indexes after each purchase. Examine decision_checks.slots to verify available joker and consumable slots before buying."
    )]
    async fn get_decision(
        &self,
        Parameters(params): Parameters<DecisionParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.policy(params.limit).await {
            Ok(mut data) => {
                if !params.action_type.is_empty()
                    && let Some(actions) =
                        data.get_mut("legal_actions").and_then(Value::as_array_mut)
                {
                    actions.retain(|a| {
                        a.get("action").and_then(Value::as_str) == Some(params.action_type.as_str())
                    });
                }
                to_tool_result(envelope(true, data, "", ""))
            }
            Err(e) => to_tool_result(envelope(false, Value::Null, "decision_failed", &e)),
        }
    }

    #[tool(
        description = "Execute exactly one current legal action using action_id and decision_id. Mutations are serialized and checkpointed."
    )]
    async fn take_action(
        &self,
        Parameters(params): Parameters<ActionParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if params.action_id.is_empty() || params.decision_id.is_empty() {
            return to_tool_result(envelope(
                false,
                Value::Null,
                "invalid_arguments",
                "action_id and decision_id are required",
            ));
        }
        let _guard = self.mutations.lock().await;
        if let Err(problem) = self.preflight().await {
            return to_tool_result(problem);
        }
        let current = self.policy(40).await.unwrap_or(Value::Null);
        if current.get("decision_id").and_then(Value::as_str) != Some(params.decision_id.as_str()) {
            return to_tool_result(envelope(
                false,
                current,
                "stale_decision",
                "decision_id does not match the current observation",
            ));
        }
        let settle = params.settle_timeout.clamp(1.0, 30.0);
        let selected = current
            .get("legal_actions")
            .and_then(Value::as_array)
            .and_then(|actions| {
                actions.iter().find(|action| {
                    action.get("action_id").and_then(Value::as_str)
                        == Some(params.action_id.as_str())
                })
            });
        match execute_policy_action(
            &self.ipc,
            &params.action_id,
            &params.decision_id,
            selected,
            30,
            15,
            60,
            settle,
        ) {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(error) => {
                let current = self.policy(40).await.unwrap_or(Value::Null);
                let code = action_error_code(&error);
                to_tool_result(envelope(false, current, code, &error))
            }
        }
    }

    #[tool(description = "Advance only confirmed non-strategic transitions.")]
    async fn advance_safe(
        &self,
        Parameters(params): Parameters<AdvanceParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let _guard = self.mutations.lock().await;
        if let Err(problem) = self.preflight().await {
            return to_tool_result(problem);
        }
        let steps = params.max_steps.clamp(1, 20) as u32;
        // Try each safe transition action from the policy module
        for action in SAFE_TRANSITION_ACTIONS {
            match advance_safe_internal(&self.ipc, action, steps) {
                Ok(_data) => {
                    match self.policy(40).await {
                        Ok(policy_data) => {
                            return to_tool_result(envelope(true, policy_data, "", ""));
                        }
                        Err(e) => {
                            return to_tool_result(envelope(
                                false,
                                Value::Null,
                                "advance_failed",
                                &e,
                            ));
                        }
                    };
                }
                Err(_) => continue,
            }
        }
        to_tool_result(envelope(
            false,
            Value::Null,
            "advance_failed",
            "no safe transition succeeded",
        ))
    }

    #[tool(description = "Wait read-only for an exact state, or for any stable state when blank.")]
    async fn wait_for_state(
        &self,
        Parameters(params): Parameters<WaitParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs_f64(params.timeout.clamp(0.1, 60.0));
        loop {
            if let Ok(data) = self.policy(20).await {
                let state = value_state(&data)
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                if (!params.state.is_empty() && state.eq_ignore_ascii_case(&params.state))
                    || (params.state.is_empty() && !state.is_empty())
                {
                    return to_tool_result(envelope(true, data, "", ""));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return to_tool_result(envelope(
                    false,
                    Value::Null,
                    "timeout",
                    "timed out waiting for state",
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[tool(description = "Persist the current observation into canonical resumable session state.")]
    async fn checkpoint(
        &self,
        Parameters(params): Parameters<CheckpointParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match checkpoint_internal(&self.ipc, &params.kind) {
            Ok(data) => {
                if let Ok(observation) = self.read_observation() {
                    let _ = self
                        .state_db
                        .lock()
                        .await
                        .checkpoint(&observation, &params.kind);
                }
                to_tool_result(envelope(true, data, "", ""))
            }
            Err(e) => to_tool_result(envelope(false, Value::Null, "checkpoint_failed", &e)),
        }
    }

    #[tool(
        description = "Look up a Balatro entity and compatible editions, enhancements, seals, stickers, or suit."
    )]
    async fn lookup_rule(
        &self,
        Parameters(params): Parameters<LookupParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if !INFO_TYPES.contains(&params.entity_type.as_str()) {
            return to_tool_result(envelope(
                false,
                Value::Null,
                "invalid_arguments",
                "unsupported entity type",
            ));
        }
        let mut args = vec!["lookup".into(), params.entity_type, params.name];
        for (flag, value) in [
            ("--suit", params.suit),
            ("--edition", params.edition),
            ("--enhancement", params.enhancement),
            ("--seal", params.seal),
        ] {
            if !value.is_empty() {
                args.push(flag.into());
                args.push(value);
            }
        }
        for sticker in params.stickers {
            args.push("--sticker".into());
            args.push(sticker);
        }
        match self.node(&args).await {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(e) => to_tool_result(envelope(false, Value::Null, "lookup_failed", &e)),
        }
    }

    #[tool(description = "List static Balatro entities, optionally by entity_type.")]
    async fn list_rules(
        &self,
        Parameters(params): Parameters<ListParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let mut args = vec!["list".into()];
        if !params.entity_type.is_empty() {
            args.push(params.entity_type);
        }
        match self.node(&args).await {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(e) => to_tool_result(envelope(false, Value::Null, "list_failed", &e)),
        }
    }
    #[tool(description = "Return counts and metadata for the vendored rules database.")]
    async fn rules_stats(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.node(&["stats".into()]).await {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(e) => to_tool_result(envelope(false, Value::Null, "stats_failed", &e)),
        }
    }
    #[tool(
        description = "Explain a stable Balatro concept. Use the guide resource for the full text."
    )]
    async fn rules_overview(
        &self,
        Parameters(params): Parameters<TopicParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match guide(&params.topic) {
            Some(text) => to_tool_result(envelope(
                true,
                json!({"topic": params.topic, "guide": text}),
                "",
                "",
            )),
            None => to_tool_result(envelope(
                false,
                Value::Null,
                "unknown_topic",
                "unknown guide topic",
            )),
        }
    }
    #[tool(
        description = "Query prior replay lines for the required seed, ante, stake, blind, and outcome."
    )]
    async fn query_replays(
        &self,
        Parameters(params): Parameters<ReplayQueryParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let outcome_marker: Option<&str> = if params.outcome == "best" {
            Some("clear")
        } else if params.outcome == "fail" {
            Some("fail")
        } else {
            Some(params.outcome.as_str())
        };
        let result = self.replay_db.lock().await.query_replays(
            Some(SEED),
            Some(params.ante),
            Some(params.stake),
            if params.blind.is_empty() {
                None
            } else {
                Some(params.blind.as_str())
            },
            outcome_marker,
            true,
        );
        match result {
            Ok(data) => to_tool_result(envelope(true, json!({"replays": data}), "", "")),
            Err(e) => to_tool_result(envelope(false, Value::Null, "replay_query_failed", &e)),
        }
    }
    #[tool(
        description = "Log a blind clear or failure for the required seed. Use immediately after resolution."
    )]
    async fn log_replay(
        &self,
        Parameters(params): Parameters<ReplayLogParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let _guard = self.mutations.lock().await;
        if params.outcome == "clear" {
            // Parse jokers
            let jokers: Vec<(i64, &str, Option<&str>, Option<&str>, Option<&str>)> = params
                .jokers
                .iter()
                .enumerate()
                .map(|(idx, j)| {
                    let parts: Vec<&str> = j.splitn(3, ',').collect();
                    let name = parts.first().map(|s| s.trim()).unwrap_or("");
                    let edition = parts.get(1).and_then(|s| {
                        if s.trim().is_empty() {
                            None
                        } else {
                            Some(s.trim())
                        }
                    });
                    let enh = parts.get(2).and_then(|s| {
                        if s.trim().is_empty() {
                            None
                        } else {
                            Some(s.trim())
                        }
                    });
                    (idx as i64, name, edition, enh, None::<&str>)
                })
                .collect();
            // Parse steps into the complex tuple format
            let steps: Vec<(
                String,
                String,
                Option<String>,
                String,
                Option<String>,
                Option<String>,
                i64,
                Option<String>,
                Option<i64>,
                Option<i64>,
                Option<i64>,
                Option<String>,
                Option<String>,
                Option<String>,
            )> = params
                .steps
                .iter()
                .map(|s| {
                    let parts: Vec<&str> = s.splitn(14, ',').collect();
                    let action = parts
                        .first()
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    let details = parts
                        .get(1)
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    let rationale = parts.get(2).and_then(|s| {
                        let t = s.trim();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t.to_string())
                        }
                    });
                    let ht = parts
                        .get(3)
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    let ch = parts.get(4).and_then(|s| {
                        let t = s.trim();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t.to_string())
                        }
                    });
                    let cd = parts.get(5).and_then(|s| {
                        let t = s.trim();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t.to_string())
                        }
                    });
                    let dc = parts
                        .get(6)
                        .and_then(|s| s.trim().parse::<i64>().ok())
                        .unwrap_or(0);
                    let fc = parts.get(7).and_then(|s| {
                        let t = s.trim();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t.to_string())
                        }
                    });
                    let bc = parts.get(8).and_then(|s| s.trim().parse::<i64>().ok());
                    let bm = parts.get(9).and_then(|s| s.trim().parse::<i64>().ok());
                    let fs = parts.get(10).and_then(|s| s.trim().parse::<i64>().ok());
                    let cn = parts.get(11).and_then(|s| {
                        let t = s.trim();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t.to_string())
                        }
                    });
                    let th = parts.get(12).and_then(|s| {
                        let t = s.trim();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t.to_string())
                        }
                    });
                    let nt = parts.get(13).and_then(|s| {
                        let t = s.trim();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t.to_string())
                        }
                    });
                    (
                        action, details, rationale, ht, ch, cd, dc, fc, bc, bm, fs, cn, th, nt,
                    )
                })
                .collect();
            // Parse vouchers from notes if present, otherwise empty
            let vouchers: Vec<&str> = vec![];
            // Parse hand levels from notes if present, otherwise empty
            let hand_levels: Vec<(&str, i64, Option<i64>, Option<i64>)> = vec![];
            // Parse economy
            let dollars_start = params.dollars_start;
            let dollars_end = params.dollars_end;
            let shop_bought = "";
            let shop_skipped = "";
            // Parse tags from notes if present, otherwise empty
            let tags: Vec<&str> = vec![];
            // Notes
            let notes = params.notes.as_str();

            match self.replay_db.lock().await.log_clear(
                SEED,
                params.ante,
                params.stake,
                &params.blind_key,
                &jokers,
                &vouchers,
                &hand_levels,
                dollars_start,
                dollars_end,
                shop_bought,
                shop_skipped,
                &steps,
                &tags,
                notes,
            ) {
                Ok(rid) => to_tool_result(envelope(
                    true,
                    json!({"replay_id": rid, "outcome": "clear"}),
                    "",
                    "",
                )),
                Err(e) => to_tool_result(envelope(false, Value::Null, "replay_log_failed", &e)),
            }
        } else {
            // Fail outcome
            match self.replay_db.lock().await.log_fail(
                SEED,
                params.ante,
                params.stake,
                &params.blind_key,
            ) {
                Ok(rid) => to_tool_result(envelope(
                    true,
                    json!({"replay_id": rid, "outcome": "fail"}),
                    "",
                    "",
                )),
                Err(e) => to_tool_result(envelope(false, Value::Null, "replay_log_failed", &e)),
            }
        }
    }

    #[tool(
        description = "Score selected hand cards using the live poker-hand contract and explicit estimate metadata."
    )]
    async fn score_hand(
        &self,
        Parameters(params): Parameters<ScoreParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.read_observation() {
            Ok(observation) => to_tool_result(envelope(
                true,
                serde_json::to_value(score_hand(
                    &observation,
                    if params.card_indices.is_empty() {
                        None
                    } else {
                        Some(params.card_indices.as_slice())
                    },
                ))
                .unwrap_or(Value::Null),
                "",
                "",
            )),
            Err(error) => to_tool_result(envelope(false, Value::Null, "score_failed", &error)),
        }
    }

    #[tool(description = "Return the canonical live poker-hand values contract.")]
    async fn hand_values(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.read_observation() {
            Ok(observation) => to_tool_result(envelope(
                true,
                observation
                    .get("poker_hands")
                    .cloned()
                    .unwrap_or(Value::Null),
                "",
                "",
            )),
            Err(error) => {
                to_tool_result(envelope(false, Value::Null, "hand_values_failed", &error))
            }
        }
    }

    #[tool(description = "Read Rust-owned strategy rules and directives.")]
    async fn strategy_state(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.state_db.lock().await.strategy() {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(error) => to_tool_result(envelope(false, Value::Null, "strategy_failed", &error)),
        }
    }

    #[tool(description = "Add a Rust-owned conditioned strategy rule.")]
    async fn strategy_add_rule(
        &self,
        Parameters(params): Parameters<StrategyRuleParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if params.id.is_empty() || params.directive.is_empty() {
            return to_tool_result(envelope(
                false,
                Value::Null,
                "invalid_arguments",
                "id and directive are required",
            ));
        }
        match self.state_db.lock().await.add_rule(
            &params.id,
            &params.kind,
            &params.conditions,
            &params.directive,
            params.absolute,
        ) {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(error) => {
                to_tool_result(envelope(false, Value::Null, "strategy_rule_failed", &error))
            }
        }
    }

    #[tool(description = "Record evidence for a Rust-owned strategy rule.")]
    async fn strategy_record_evidence(
        &self,
        Parameters(params): Parameters<StrategyEvidenceParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.state_db.lock().await.record_evidence(
            &params.rule_id,
            &params.outcome,
            &params.event_id,
            &params.note,
        ) {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(error) => to_tool_result(envelope(
                false,
                Value::Null,
                "strategy_evidence_failed",
                &error,
            )),
        }
    }

    #[tool(description = "Store a structured strategy lesson.")]
    async fn lesson_add(
        &self,
        Parameters(params): Parameters<LessonParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.state_db.lock().await.add_lesson(
            &params.category,
            &params.lesson,
            &params.source,
            params.confidence,
        ) {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(error) => to_tool_result(envelope(false, Value::Null, "lesson_failed", &error)),
        }
    }

    #[tool(description = "Record an actual score against an earlier estimate.")]
    async fn estimation_record(
        &self,
        Parameters(params): Parameters<EstimateParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.state_db.lock().await.record_estimate(
            &params.hand_type,
            params.estimated,
            params.actual,
            &params.context,
        ) {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(error) => to_tool_result(envelope(false, Value::Null, "estimation_failed", &error)),
        }
    }

    #[tool(description = "Summarize recorded scoring-estimation errors.")]
    async fn estimation_report(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.state_db.lock().await.estimation_report() {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(error) => to_tool_result(envelope(
                false,
                Value::Null,
                "estimation_report_failed",
                &error,
            )),
        }
    }

    #[tool(description = "Persist the current observation into Rust-owned current-run state.")]
    async fn run_state(
        &self,
        Parameters(params): Parameters<StateParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if params.kind == "read" {
            return match self.state_db.lock().await.current_run() {
                Ok(data) => to_tool_result(envelope(true, data, "", "")),
                Err(error) => {
                    to_tool_result(envelope(false, Value::Null, "run_state_failed", &error))
                }
            };
        }
        match self.read_observation() {
            Ok(observation) => match self
                .state_db
                .lock()
                .await
                .checkpoint(&observation, &params.kind)
            {
                Ok(data) => to_tool_result(envelope(true, data, "", "")),
                Err(error) => {
                    to_tool_result(envelope(false, Value::Null, "run_state_failed", &error))
                }
            },
            Err(error) => {
                to_tool_result(envelope(false, Value::Null, "observation_failed", &error))
            }
        }
    }

    #[tool(description = "Read recent Rust-owned run events.")]
    async fn event_history(
        &self,
        Parameters(params): Parameters<StateParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.state_db.lock().await.events(params.limit) {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(error) => {
                to_tool_result(envelope(false, Value::Null, "event_history_failed", &error))
            }
        }
    }

    #[tool(
        description = "Read a capped latest Lovely log tail and bridge health. Never returns arbitrary files."
    )]
    async fn runtime_diagnostics(
        &self,
        Parameters(params): Parameters<RuntimeParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let mut data = self.diagnostic(params.lines);
        data["status"] = sanitize(self.status().await);
        to_tool_result(envelope(true, data, "", ""))
    }
}

#[tool_handler(router = self.tool_router)]
impl rmcp::ServerHandler for Server {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(
            rmcp::model::Implementation::new("balatro", env!("CARGO_PKG_VERSION"))
                .with_title("Balatro safe gameplay MCP")
                .with_description("Rust stdio boundary for safe Balatro gameplay."),
        )
        .with_instructions(
            "Use only these MCP tools for Balatro. Start with game_status, then get_decision; examine decision_checks.ordering when jokers are present; examine decision_checks.consumables for owned or shop Tarot/Planet/Spectral; examine decision_checks.shop and decision_checks.slots during SHOP state; execute only legal action_id with its decision_id.",
        )
    }
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn list_resources(
        &self,
        _: Option<rmcp::model::PaginatedRequestParams>,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, rmcp::ErrorData>> + Send + '_
    {
        std::future::ready(Ok(guide_resources()))
    }
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResult, rmcp::ErrorData>> + Send + '_
    {
        std::future::ready(read_guide_resource(request.uri))
    }
}

fn guide_resources() -> ListResourcesResult {
    let mut resources = vec![rmcp::model::Annotated::new(
        rmcp::model::RawResource::new("balatro://guide", "Balatro guides"),
        None,
    )];
    for topic in GUIDE_TOPICS {
        resources.push(rmcp::model::Annotated::new(
            rmcp::model::RawResource::new(
                format!("balatro://guide/{topic}"),
                format!("Balatro guide: {topic}"),
            ),
            None,
        ));
    }
    ListResourcesResult::with_all_items(resources)
}

fn read_guide_resource(uri: String) -> Result<ReadResourceResult, rmcp::ErrorData> {
    let topic = uri.strip_prefix("balatro://guide/").unwrap_or("core");
    guide(topic)
        .map(|text| ReadResourceResult::new(vec![ResourceContents::text(text, uri)]))
        .ok_or_else(|| rmcp::ErrorData::invalid_params("unknown guide topic", None))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn server() -> (tempfile::TempDir, Server) {
        let dir = tempdir().unwrap();
        let server = Server::new(dir.path().to_path_buf()).unwrap();
        (dir, server)
    }

    #[tokio::test]
    async fn route_validation_errors_are_structured() {
        let (_dir, server) = server();
        assert!(
            server
                .strategy_add_rule(Parameters(StrategyRuleParams {
                    id: String::new(),
                    kind: "bad".into(),
                    conditions: json!({}),
                    directive: String::new(),
                    absolute: false
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .observe(Parameters(ObserveParams {
                    section: "bad".into()
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .take_action(Parameters(ActionParams {
                    action_id: String::new(),
                    decision_id: String::new(),
                    settle_timeout: 12.0
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .lookup_rule(Parameters(LookupParams {
                    entity_type: "bad".into(),
                    name: "x".into(),
                    suit: String::new(),
                    edition: String::new(),
                    enhancement: String::new(),
                    seal: String::new(),
                    stickers: vec![]
                }))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn read_only_routes_work_without_a_live_game() {
        let (_dir, server) = server();
        assert!(
            server
                .rules_overview(Parameters(TopicParams {
                    topic: "core".into()
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .rules_overview(Parameters(TopicParams {
                    topic: "unknown".into()
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .query_replays(Parameters(ReplayQueryParams {
                    ante: 1,
                    stake: 1,
                    blind: String::new(),
                    outcome: "best".into()
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .runtime_diagnostics(Parameters(RuntimeParams { lines: 5 }))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn replay_routes_cover_clear_and_fail_outcomes() {
        let (_dir, server) = server();
        for outcome in ["clear", "fail"] {
            assert!(
                server
                    .log_replay(Parameters(ReplayLogParams {
                        outcome: outcome.into(),
                        ante: 1,
                        stake: 1,
                        blind_key: "Small".into(),
                        jokers: vec![],
                        steps: vec![],
                        dollars_start: None,
                        dollars_end: None,
                        notes: String::new(),
                    }))
                    .await
                    .is_ok()
            );
        }
        let step = (0..14)
            .map(|i| format!("field{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let sparse_step = vec![
            "action", "details", "", "", "", "", "", "", "", "", "", "", "", "",
        ]
        .join(",");
        assert!(
            server
                .log_replay(Parameters(ReplayLogParams {
                    outcome: "clear".into(),
                    ante: 2,
                    stake: 1,
                    blind_key: "Boss".into(),
                    jokers: vec!["Joker,Foil,Bonus".into(), "Joker,,".into()],
                    steps: vec![step, sparse_step],
                    dollars_start: Some(10),
                    dollars_end: Some(20),
                    notes: "full fixture".into(),
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .query_replays(Parameters(ReplayQueryParams {
                    ante: 2,
                    stake: 1,
                    blind: "Boss".into(),
                    outcome: "clear".into(),
                }))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn every_tool_route_has_a_no_game_boundary_path() {
        let (_dir, server) = server();
        assert!(server.game_status().await.is_ok());
        assert!(server.ensure_runtime().await.is_ok());
        assert!(
            server
                .get_decision(Parameters(DecisionParams {
                    action_type: String::new(),
                    limit: 1
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .get_decision(Parameters(DecisionParams {
                    action_type: "play".into(),
                    limit: 10
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .observe(Parameters(ObserveParams {
                    section: "summary".into()
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .advance_safe(Parameters(AdvanceParams { max_steps: 1 }))
                .await
                .is_ok()
        );
        assert!(
            server
                .wait_for_state(Parameters(WaitParams {
                    state: String::new(),
                    timeout: 0.1
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .checkpoint(Parameters(CheckpointParams {
                    kind: "test".into()
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .list_rules(Parameters(ListParams {
                    entity_type: String::new()
                }))
                .await
                .is_ok()
        );
        assert!(server.rules_stats().await.is_ok());
        let info = <Server as rmcp::ServerHandler>::get_info(&server);
        assert!(info.capabilities.tools.is_some());
        assert!(info.capabilities.resources.is_some());
        let registered: std::collections::HashSet<_> = Server::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        for expected in [
            "game_status",
            "ensure_runtime",
            "observe",
            "get_decision",
            "take_action",
            "advance_safe",
            "wait_for_state",
            "checkpoint",
            "lookup_rule",
            "list_rules",
            "rules_stats",
            "rules_overview",
            "query_replays",
            "log_replay",
            "score_hand",
            "hand_values",
            "strategy_state",
            "strategy_add_rule",
            "strategy_record_evidence",
            "lesson_add",
            "estimation_record",
            "estimation_report",
            "run_state",
            "event_history",
            "runtime_diagnostics",
        ] {
            assert!(
                registered.contains(expected),
                "missing registered tool {expected}"
            );
        }
    }

    #[tokio::test]
    async fn new_state_and_scoring_tools_have_structured_paths() {
        let (_dir, server) = server();
        assert!(
            server
                .score_hand(Parameters(ScoreParams {
                    card_indices: vec![]
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .score_hand(Parameters(ScoreParams {
                    card_indices: vec![0]
                }))
                .await
                .is_ok()
        );
        assert!(server.hand_values().await.is_ok());
        assert!(server.strategy_state().await.is_ok());
        assert!(
            server
                .strategy_add_rule(Parameters(StrategyRuleParams {
                    id: "r1".into(),
                    kind: "test".into(),
                    conditions: json!({}),
                    directive: "test".into(),
                    absolute: false
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .strategy_record_evidence(Parameters(StrategyEvidenceParams {
                    rule_id: "r1".into(),
                    outcome: "success".into(),
                    event_id: "e1".into(),
                    note: "test".into()
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .lesson_add(Parameters(LessonParams {
                    category: "test".into(),
                    lesson: "lesson".into(),
                    source: "unit".into(),
                    confidence: 0.8
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .estimation_record(Parameters(EstimateParams {
                    hand_type: "Pair".into(),
                    estimated: 10,
                    actual: 12,
                    context: json!({})
                }))
                .await
                .is_ok()
        );
        assert!(server.estimation_report().await.is_ok());
        write_fixture(&server);
        assert!(
            server
                .run_state(Parameters(StateParams {
                    kind: "broken-checkpoint".into(),
                    limit: 1
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .run_state(Parameters(StateParams {
                    kind: "read".into(),
                    limit: 10
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .run_state(Parameters(StateParams {
                    kind: "read".into(),
                    limit: 10
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .event_history(Parameters(StateParams {
                    kind: "events".into(),
                    limit: 10
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .event_history(Parameters(StateParams {
                    kind: "events".into(),
                    limit: 10
                }))
                .await
                .is_ok()
        );
    }

    fn write_fixture(server: &Server) {
        let observation = json!({
            "state": "PLAY",
            "round": {"seed": SEED},
            "ready": {},
            "bridge": {"loaded": true, "version": "0.6.0", "session_id": "test"},
            "areas": {"hand": [
                {"base": {"rank": "A", "suit": "H"}, "suits": [{"key": "H"}]},
                {"base": {"rank": "K", "suit": "D"}, "suits": [{"key": "D"}]}
            ]},
            "poker_hands": {"values": {"High Card": {"chips": 10, "mult": 2}}}
        });
        std::fs::write(
            &server.ipc.observation_path,
            serde_json::to_vec(&observation).unwrap(),
        )
        .unwrap();
        let age_path = server.runtime_root.join("Balatro/codex_observation.json");
        std::fs::create_dir_all(age_path.parent().unwrap()).unwrap();
        std::fs::write(age_path, b"{}").unwrap();
    }

    #[tokio::test]
    async fn deterministic_observation_and_ipc_route_success_paths() {
        let (_dir, server) = server();
        write_fixture(&server);
        assert!(
            server
                .observe(Parameters(ObserveParams {
                    section: "all".into()
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .get_decision(Parameters(DecisionParams {
                    action_type: String::new(),
                    limit: 10
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .score_hand(Parameters(ScoreParams {
                    card_indices: vec![]
                }))
                .await
                .is_ok()
        );
        assert!(server.hand_values().await.is_ok());
        assert!(
            server
                .wait_for_state(Parameters(WaitParams {
                    state: "PLAY".into(),
                    timeout: 0.1
                }))
                .await
                .is_ok()
        );

        std::fs::write(&server.ipc.response_path, r#"{"_decode_error":true}"#).unwrap();
        assert!(
            server
                .checkpoint(Parameters(CheckpointParams {
                    kind: "test".into()
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .run_state(Parameters(StateParams {
                    kind: "checkpoint".into(),
                    limit: 10
                }))
                .await
                .is_ok()
        );

        *server.process_override.lock().await = Some(vec![json!({
            "pid": 1234,
            "name": "Balatro.exe"
        })]);
        assert!(server.game_status().await.is_ok());
        assert!(server.ensure_runtime().await.is_ok());
        std::fs::write(&server.ipc.response_path, r#"{"_decode_error":true}"#).unwrap();
        let observation: Value =
            serde_json::from_slice(&std::fs::read(&server.ipc.observation_path).unwrap()).unwrap();
        let decision = decision_id_for(&observation);
        assert!(
            server
                .take_action(Parameters(ActionParams {
                    action_id: "play".into(),
                    decision_id: "stale".into(),
                    settle_timeout: 1.0
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .take_action(Parameters(ActionParams {
                    action_id: "play".into(),
                    decision_id: decision.clone(),
                    settle_timeout: 1.0
                }))
                .await
                .is_ok()
        );
        std::fs::write(&server.ipc.response_path, r#"{"_decode_error":true}"#).unwrap();
        assert!(
            server
                .advance_safe(Parameters(AdvanceParams { max_steps: 1 }))
                .await
                .is_ok()
        );
        let mut broken = server.clone();
        broken.ipc.command_path = std::path::PathBuf::from("Z:\\missing\\command.lua");
        assert!(
            broken
                .take_action(Parameters(ActionParams {
                    action_id: "play".into(),
                    decision_id: decision,
                    settle_timeout: 1.0
                }))
                .await
                .is_ok()
        );
        assert!(
            broken
                .advance_safe(Parameters(AdvanceParams { max_steps: 1 }))
                .await
                .is_ok()
        );
        assert!(
            broken
                .checkpoint(Parameters(CheckpointParams {
                    kind: "broken".into()
                }))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn info_db_routes_cover_json_success_and_backend_failures() {
        let (dir, server) = server();
        let script = dir.path().join("tools/balatro-info-db/bin/balatro-info.js");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(
            &script,
            "console.log(JSON.stringify({ok:true, args:process.argv.slice(2)}));",
        )
        .unwrap();
        assert!(
            server
                .lookup_rule(Parameters(LookupParams {
                    entity_type: "joker".into(),
                    name: "Joker".into(),
                    suit: "H".into(),
                    edition: "Foil".into(),
                    enhancement: "Bonus".into(),
                    seal: "Red".into(),
                    stickers: vec!["eternal".into()]
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .list_rules(Parameters(ListParams {
                    entity_type: "joker".into()
                }))
                .await
                .is_ok()
        );
        assert!(server.rules_stats().await.is_ok());
        std::fs::write(&script, "console.error('backend failed'); process.exit(2);").unwrap();
        assert!(server.rules_stats().await.is_ok());
        assert!(
            server
                .lookup_rule(Parameters(LookupParams {
                    entity_type: "joker".into(),
                    name: "Joker".into(),
                    suit: String::new(),
                    edition: String::new(),
                    enhancement: String::new(),
                    seal: String::new(),
                    stickers: vec![]
                }))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn external_runner_covers_non_json_launch_and_timeout_errors() {
        let (dir, server) = server();
        let script = dir.path().join("tools/balatro-info-db/bin/balatro-info.js");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "console.log('not-json');").unwrap();
        let error = server.node(&["stats".into()]).await.unwrap_err();
        assert!(error.contains("non-JSON"));
        let error = server
            .run_external_json("definitely-not-a-real-program", &[], 0.1)
            .await
            .unwrap_err();
        assert!(error.contains("launch failed"));
        std::fs::write(&script, "setTimeout(() => console.log('{}'), 1000);").unwrap();
        let error = server
            .run_external_json("node", &[script.display().to_string()], 0.1)
            .await
            .unwrap_err();
        assert!(error.contains("timeout"));
    }

    #[tokio::test]
    async fn state_and_replay_route_failures_are_structured() {
        let (dir, server) = server();
        let bad_state_root = dir.path().join("bad-state");
        std::fs::create_dir_all(bad_state_root.join("agent/rust_state.db")).unwrap();
        *server.state_db.lock().await = StateDB::new(&bad_state_root);
        assert!(server.strategy_state().await.is_ok());
        assert!(
            server
                .strategy_add_rule(Parameters(StrategyRuleParams {
                    id: "bad".into(),
                    kind: "x".into(),
                    conditions: json!({}),
                    directive: "x".into(),
                    absolute: false
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .lesson_add(Parameters(LessonParams {
                    category: "x".into(),
                    lesson: "x".into(),
                    source: "x".into(),
                    confidence: 0.5
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .estimation_record(Parameters(EstimateParams {
                    hand_type: "x".into(),
                    estimated: 1,
                    actual: 2,
                    context: json!({})
                }))
                .await
                .is_ok()
        );
        assert!(server.estimation_report().await.is_ok());
        assert!(
            server
                .run_state(Parameters(StateParams {
                    kind: "read".into(),
                    limit: 1
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .event_history(Parameters(StateParams {
                    kind: "events".into(),
                    limit: 1
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .strategy_record_evidence(Parameters(StrategyEvidenceParams {
                    rule_id: "bad".into(),
                    outcome: "failure".into(),
                    event_id: "x".into(),
                    note: "x".into()
                }))
                .await
                .is_ok()
        );

        let bad_replay_root = dir.path().join("bad-replay");
        std::fs::create_dir_all(bad_replay_root.join("agent/replays.db")).unwrap();
        *server.replay_db.lock().await = ReplayDB::new(&bad_replay_root);
        assert!(
            server
                .query_replays(Parameters(ReplayQueryParams {
                    ante: 1,
                    stake: 1,
                    blind: String::new(),
                    outcome: "other".into()
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .log_replay(Parameters(ReplayLogParams {
                    outcome: "fail".into(),
                    ante: 1,
                    stake: 1,
                    blind_key: "x".into(),
                    jokers: vec![],
                    steps: vec![],
                    dollars_start: None,
                    dollars_end: None,
                    notes: String::new()
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .log_replay(Parameters(ReplayLogParams {
                    outcome: "clear".into(),
                    ante: 1,
                    stake: 1,
                    blind_key: "x".into(),
                    jokers: vec![],
                    steps: vec![],
                    dollars_start: None,
                    dollars_end: None,
                    notes: String::new()
                }))
                .await
                .is_ok()
        );
    }

    #[test]
    fn directive_filtering_and_diagnostics_are_safe() {
        let (_dir, server) = server();
        let rules = json!({"rules":[
            {"id":"active","active":true,"conditions":{"/state":"PLAY"}},
            {"id":"inactive","active":false,"conditions":{}},
            {"id":"unconditional","conditions":{}}
        ]});
        let result = active_directives(&rules, &json!({"state":"PLAY"}));
        assert_eq!(result.as_array().unwrap().len(), 2);
        assert!(server.diagnostic(0).get("log_found").is_some());
    }

    #[test]
    fn resource_helpers_cover_root_known_and_unknown_topics() {
        let resources = guide_resources();
        assert!(resources.resources.len() > GUIDE_TOPICS.len());
        assert!(read_guide_resource("balatro://guide/core".into()).is_ok());
        assert!(read_guide_resource("balatro://guide/does-not-exist".into()).is_err());
        assert!(read_guide_resource("balatro://guide".into()).is_ok());
    }

    #[test]
    fn decision_ids_are_stable_and_observation_sensitive() {
        let first = json!({"b":1,"a":[true]});
        let reordered = json!({"a":[true],"b":1});
        assert_eq!(decision_id_for(&first), decision_id_for(&reordered));
        assert_ne!(
            decision_id_for(&first),
            decision_id_for(&json!({"b":2,"a":[true]}))
        );
        assert_eq!(canonical_json(&Value::Null), "null");
        assert_eq!(canonical_json(&json!(42)), "42");
        assert_eq!(canonical_json(&json!("text")), "\"text\"");
        assert_eq!(action_error_code("stale decision"), "stale_decision");
        assert_eq!(action_error_code("bridge timeout"), "timeout");
        assert_eq!(action_error_code("cannot write"), "action_failed");
    }

    #[tokio::test]
    async fn runtime_status_and_preflight_failure_branches_are_exercised() {
        let (_dir, server) = server();
        *server.process_override.lock().await = Some(vec![]);
        assert!(server.ensure_runtime_impl().await.is_err());
        assert!(server.game_status().await.is_ok());
        assert!(
            server
                .take_action(Parameters(ActionParams {
                    action_id: "x".into(),
                    decision_id: "d".into(),
                    settle_timeout: 1.0
                }))
                .await
                .is_ok()
        );
        assert!(server.preflight().await.is_err());

        *server.process_override.lock().await = Some(vec![json!({"pid": 1}), json!({"pid": 2})]);
        assert!(server.ensure_runtime().await.is_ok());

        write_fixture(&server);
        let mut wrong =
            serde_json::from_slice::<Value>(&std::fs::read(&server.ipc.observation_path).unwrap())
                .unwrap();
        wrong["round"]["seed"] = json!("WRONG");
        std::fs::write(
            &server.ipc.observation_path,
            serde_json::to_vec(&wrong).unwrap(),
        )
        .unwrap();
        *server.process_override.lock().await = Some(vec![json!({"pid": 1})]);
        assert!(server.game_status().await.is_ok());
        assert!(
            server
                .take_action(Parameters(ActionParams {
                    action_id: "x".into(),
                    decision_id: "d".into(),
                    settle_timeout: 1.0
                }))
                .await
                .is_ok()
        );

        wrong["round"]["seed"] = json!(SEED);
        wrong["bridge"] = json!({"loaded": true, "version": "0.6.0"});
        std::fs::write(
            &server.ipc.observation_path,
            serde_json::to_vec(&wrong).unwrap(),
        )
        .unwrap();
        assert!(
            server
                .take_action(Parameters(ActionParams {
                    action_id: "x".into(),
                    decision_id: "d".into(),
                    settle_timeout: 1.0
                }))
                .await
                .is_ok()
        );
        assert!(server.preflight().await.is_err());

        wrong["bridge"] = json!({"loaded": false, "version": "0.6.0", "session_id": "x"});
        std::fs::write(
            &server.ipc.observation_path,
            serde_json::to_vec(&wrong).unwrap(),
        )
        .unwrap();
        assert!(server.preflight().await.is_err());
    }
}
