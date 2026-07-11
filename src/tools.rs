use rmcp::{
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, ListResourcesResult, ReadResourceRequestParams, ReadResourceResult,
        ResourceContents,
    },
    tool, tool_handler, tool_router,
};
use serde_json::{Value, json};
use std::{env, fs, path::PathBuf, sync::Arc, time::Duration};
use tokio::{process::Command, sync::Mutex, time::timeout};

use crate::backend::{
    ipc::{IpcPaths, advance_safe_internal, checkpoint_internal, execute_policy_action},
    policy::{SAFE_TRANSITION_ACTIONS, build_policy_state},
    replay::ReplayDB,
    runtime::{self, balatro_processes, observation_age},
};
use crate::guide::{GUIDE_TOPICS, guide};
use crate::models::*;
use crate::protocol::{
    compact_observation, envelope, sanitize, tool as to_tool_result, value_state,
};

use std::sync::LazyLock;
static EMPTY_BRIDGE: LazyLock<serde_json::Map<String, Value>> = LazyLock::new(serde_json::Map::new);

#[derive(Clone)]
pub struct Server {
    pub root: PathBuf,
    pub runtime_root: PathBuf,
    pub ipc: IpcPaths,
    pub mutations: Arc<Mutex<()>>,
    pub replay_db: Arc<Mutex<ReplayDB>>,
    pub tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

impl Server {
    pub fn new(root: PathBuf) -> Result<Self, String> {
        let runtime_root = env::var_os("BALATRO_RUNTIME_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.clone());
        let ipc = IpcPaths::new(&runtime_root);
        let replay_db = Arc::new(Mutex::new(ReplayDB::new(&runtime_root)));
        Ok(Self {
            root,
            runtime_root,
            ipc,
            mutations: Arc::new(Mutex::new(())),
            replay_db,
            tool_router: Self::tool_router(),
        })
    }

    /// Read the observation JSON from the Lua bridge.
    pub fn read_observation(&self) -> Result<Value, String> {
        self.ipc.read_observation()
    }

    async fn policy(&self, limit: u32) -> Result<Value, String> {
        let observation = self.read_observation()?;
        let state = build_policy_state(&observation, limit as usize, 40, 60);
        Ok(state)
    }

    async fn status(&self) -> Result<Value, String> {
        let processes = balatro_processes().unwrap_or_default();
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
        Ok(json!({
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
        }))
    }

    pub async fn ensure_runtime_impl(&self) -> Result<Value, String> {
        let status = self.status().await?;
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
        match self.status().await {
            Ok(status) => {
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
                Ok(status)
            }
            Err(error) => Err(envelope(false, Value::Null, "status_failed", &error)),
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
        match self.status().await {
            Ok(data) => {
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
            Err(e) => to_tool_result(envelope(false, Value::Null, "status_failed", &e)),
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
        let settle = params.settle_timeout.clamp(1.0, 30.0);
        match execute_policy_action(
            &self.ipc,
            &params.action_id,
            &params.decision_id,
            30,
            15,
            60,
            settle,
        ) {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(error) => {
                let current = self.policy(40).await.unwrap_or(Value::Null);
                let code = if error.contains("stale") {
                    "stale_decision"
                } else if error.contains("timeout") {
                    "timeout"
                } else {
                    "action_failed"
                };
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
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
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
        description = "Read a capped latest Lovely log tail and bridge health. Never returns arbitrary files."
    )]
    async fn runtime_diagnostics(
        &self,
        Parameters(params): Parameters<RuntimeParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let mut data = self.diagnostic(params.lines);
        if let Ok(status) = self.status().await {
            data["status"] = sanitize(status);
        }
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
    fn list_resources(
        &self,
        _: Option<rmcp::model::PaginatedRequestParams>,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, rmcp::ErrorData>> + Send + '_
    {
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
        std::future::ready(Ok(ListResourcesResult::with_all_items(resources)))
    }
    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResult, rmcp::ErrorData>> + Send + '_
    {
        let topic = request
            .uri
            .strip_prefix("balatro://guide/")
            .unwrap_or("core");
        let result = guide(topic)
            .map(|text| ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)]))
            .ok_or_else(|| rmcp::ErrorData::invalid_params("unknown guide topic", None));
        std::future::ready(result)
    }
}

#[cfg(test)]
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
    }
}
