use rand::distr::SampleString;

use rmcp::{
    model::{CallToolResult, ListResourcesResult, ReadResourceRequestParams, ReadResourceResult, ResourceContents},
    handler::server::wrapper::Parameters,
    tool, tool_handler, tool_router,
};
use serde_json::{Value, json};
use std::{env, fs, path::PathBuf, sync::Arc, time::Duration};
use tokio::{process::Command, sync::Mutex, time::timeout};

use crate::models::*;
use crate::protocol::{ compact_observation, envelope, sanitize, tool as to_tool_result, value_state};
use crate::guide::{GUIDE_TOPICS, guide};

#[derive(Clone)]
pub struct Server {
    pub root: PathBuf,
    pub runtime_root: PathBuf,
    pub capability_file: Arc<PathBuf>,
    pub capability: Arc<String>,
    pub mutations: Arc<Mutex<()>>,
    pub tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

impl Server {
    pub fn new(root: PathBuf) -> Result<Self, String> {
        let token = rand::distr::Alphanumeric.sample_string(&mut rand::rng(), 48);
        let file =
            env::temp_dir().join(format!("balatro-mcp-{}-{}.cap", std::process::id(), token));
        fs::write(&file, &token).map_err(|e| format!("cannot create MCP capability: {e}"))?;
        let runtime_root = env::var_os("BALATRO_RUNTIME_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.clone());
        Ok(Self {
            root,
            runtime_root,
            capability_file: Arc::new(file),
            capability: Arc::new(token),
            mutations: Arc::new(Mutex::new(())),
            tool_router: Self::tool_router(),
        })
    }

    pub async fn run_json(
        &self,
        program: &str,
        args: &[String],
        seconds: f64,
    ) -> Result<Value, String> {
        let mut child = Command::new(program);
        child
            .args(args)
            .current_dir(&self.root)
            .env("BALATRO_MCP_CAPABILITY", self.capability.as_str())
            .env(
                "BALATRO_MCP_CAPABILITY_FILE",
                self.capability_file.as_os_str(),
            )
            .env("BALATRO_RUNTIME_ROOT", self.runtime_root.as_os_str())
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
        serde_json::from_str(&out).map_err(|_| {
            format!(
                "backend returned non-JSON output: {}",
                out.chars().take(300).collect::<String>()
            )
        })
    }

    pub async fn controller(&self, args: &[&str], seconds: f64) -> Result<Value, String> {
        let mut all = vec!["balatroctl.py".to_string()];
        all.extend(args.iter().map(|x| (*x).into()));
        self.run_json("python", &all, seconds).await
    }

    pub async fn policy(&self, limit: u32) -> Result<Value, String> {
        self.controller(
            &[
                "policy-state",
                "--json",
                "--play-limit",
                &limit.min(80).to_string(),
                "--discard-limit",
                "40",
                "--target-limit",
                "60",
            ],
            12.0,
        )
        .await
    }

    pub async fn status(&self) -> Result<Value, String> {
        self.controller(&["status", "--json"], 10.0).await
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
        Command::new(self.runtime_root.join("Balatro.exe"))
            .current_dir(&self.runtime_root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot launch fixed Balatro.exe: {e}"))?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let current = self.status().await?;
            let current_count = current
                .get("processes")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            if current_count == 1 {
                return Ok(current);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("Balatro.exe did not become responsive within 15 seconds".into());
            }
        }
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
        self.run_json("node", &all, 15.0).await
    }

    pub async fn replay(&self, args: &[String]) -> Result<Value, String> {
        let mut all = vec![self.root.join("agent/replays.py").display().to_string()];
        all.extend(args.iter().cloned());
        self.run_json("python", &all, 15.0).await
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

impl Drop for Server {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.capability_file.as_ref());
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
        let settle = params.settle_timeout.clamp(1.0, 30.0).to_string();
        match self
            .controller(
                &[
                    "policy-step",
                    &params.action_id,
                    "--decision-id",
                    &params.decision_id,
                    "--json",
                    "--play-limit",
                    "30",
                    "--discard-limit",
                    "15",
                    "--target-limit",
                    "60",
                    "--settle-timeout",
                    &settle,
                ],
                40.0,
            )
            .await
        {
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
        let steps = params.max_steps.clamp(1, 20).to_string();
        match self
            .controller(&["advance-safe", "--steps", &steps], 45.0)
            .await
        {
            Ok(_) => match self.policy(40).await {
                Ok(data) => to_tool_result(envelope(true, data, "", "")),
                Err(e) => to_tool_result(envelope(false, Value::Null, "advance_failed", &e)),
            },
            Err(e) => to_tool_result(envelope(false, Value::Null, "advance_failed", &e)),
        }
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
        match self
            .controller(&["checkpoint", "--kind", &params.kind], 15.0)
            .await
        {
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
        let marker = if params.outcome == "best" {
            "@best"
        } else if params.outcome == "fail" {
            "@fail"
        } else {
            "@clear"
        };
        let args = vec![
            format!("@seed:{SEED}"),
            format!("@ante:{}", params.ante),
            format!("@stake:{}", params.stake),
            format!("@blind:{}", params.blind),
            marker.into(),
            "--json".into(),
        ];
        match self.replay(&args).await {
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
        let mut args = vec![
            params.outcome.clone(),
            SEED.into(),
            params.ante.to_string(),
            params.stake.to_string(),
            params.blind_key,
        ];
        if params.outcome == "clear" {
            if !params.jokers.is_empty() {
                args.push(format!("jokers:{}", params.jokers.join(",")));
            }
            if !params.steps.is_empty() {
                args.push(format!("steps:{}", params.steps.join(";")));
            }
            if let Some(value) = params.dollars_start {
                args.push(format!("dollars_start:{value}"));
            }
            if let Some(value) = params.dollars_end {
                args.push(format!("dollars_end:{value}"));
            }
            if !params.notes.is_empty() {
                args.push(format!("notes:{}", params.notes));
            }
        }
        match self.replay(&args).await {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(e) => to_tool_result(envelope(false, Value::Null, "replay_log_failed", &e)),
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


