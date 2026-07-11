use rand::distr::{Alphanumeric, SampleString};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Implementation, ListResourcesResult, ReadResourceRequestParams,
        ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo,
    },
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{env, fs, path::PathBuf, sync::Arc, time::Duration};
use tokio::{process::Command, sync::Mutex, time::timeout};

const SEED: &str = "2K9H9HN";
const INFO_TYPES: &[&str] = &[
    "joker",
    "tarot",
    "spectral",
    "planet",
    "voucher",
    "deck",
    "blind",
    "tag",
    "seal",
    "edition",
    "enhancement",
    "sticker",
    "stake",
    "card",
    "playing-card",
];

#[derive(Clone)]
struct Server {
    root: PathBuf,
    capability_file: Arc<PathBuf>,
    capability: Arc<String>,
    mutations: Arc<Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, JsonSchema)]
struct ObserveParams {
    #[serde(default = "summary")]
    section: String,
}
fn summary() -> String {
    "summary".into()
}

#[derive(Deserialize, JsonSchema)]
struct DecisionParams {
    #[serde(default)]
    action_type: String,
    #[serde(default = "decision_limit")]
    limit: u32,
}
fn decision_limit() -> u32 {
    40
}

#[derive(Deserialize, JsonSchema)]
struct ActionParams {
    action_id: String,
    decision_id: String,
    #[serde(default = "settle_timeout")]
    settle_timeout: f64,
}
fn settle_timeout() -> f64 {
    12.0
}

#[derive(Deserialize, JsonSchema)]
struct AdvanceParams {
    #[serde(default = "advance_steps")]
    max_steps: u32,
}
fn advance_steps() -> u32 {
    8
}

#[derive(Deserialize, JsonSchema)]
struct WaitParams {
    #[serde(default)]
    state: String,
    #[serde(default = "wait_timeout")]
    timeout: f64,
}
fn wait_timeout() -> f64 {
    10.0
}

#[derive(Deserialize, JsonSchema)]
struct CheckpointParams {
    #[serde(default = "checkpoint_kind")]
    kind: String,
}
fn checkpoint_kind() -> String {
    "mcp".into()
}

#[derive(Deserialize, JsonSchema)]
struct LookupParams {
    #[serde(rename = "type")]
    entity_type: String,
    name: String,
    #[serde(default)]
    suit: String,
    #[serde(default)]
    edition: String,
    #[serde(default)]
    enhancement: String,
    #[serde(default)]
    seal: String,
    #[serde(default)]
    stickers: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ListParams {
    #[serde(default)]
    entity_type: String,
}
#[derive(Deserialize, JsonSchema)]
struct TopicParams {
    #[serde(default = "core")]
    topic: String,
}
fn core() -> String {
    "core".into()
}
#[derive(Deserialize, JsonSchema)]
struct ReplayQueryParams {
    ante: i64,
    stake: i64,
    blind: String,
    #[serde(default = "best")]
    outcome: String,
}
fn best() -> String {
    "best".into()
}
#[derive(Deserialize, JsonSchema)]
struct ReplayLogParams {
    outcome: String,
    ante: i64,
    stake: i64,
    blind_key: String,
    #[serde(default)]
    jokers: Vec<String>,
    #[serde(default)]
    steps: Vec<String>,
    #[serde(default)]
    dollars_start: Option<i64>,
    #[serde(default)]
    dollars_end: Option<i64>,
    #[serde(default)]
    notes: String,
}
#[derive(Deserialize, JsonSchema)]
struct RuntimeParams {
    #[serde(default = "log_lines")]
    lines: u32,
}
fn log_lines() -> u32 {
    120
}

impl Server {
    fn new(root: PathBuf) -> Result<Self, String> {
        let token = Alphanumeric.sample_string(&mut rand::rng(), 48);
        let file =
            env::temp_dir().join(format!("balatro-mcp-{}-{}.cap", std::process::id(), token));
        fs::write(&file, &token).map_err(|e| format!("cannot create MCP capability: {e}"))?;
        Ok(Self {
            root,
            capability_file: Arc::new(file),
            capability: Arc::new(token),
            mutations: Arc::new(Mutex::new(())),
            tool_router: Self::tool_router(),
        })
    }

    async fn run_json(
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

    async fn controller(&self, args: &[&str], seconds: f64) -> Result<Value, String> {
        let mut all = vec!["balatroctl.py".to_string()];
        all.extend(args.iter().map(|x| (*x).into()));
        self.run_json("python", &all, seconds).await
    }

    async fn policy(&self, limit: u32) -> Result<Value, String> {
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

    async fn status(&self) -> Result<Value, String> {
        self.controller(&["status", "--json"], 10.0).await
    }

    async fn ensure_runtime_impl(&self) -> Result<Value, String> {
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
        Command::new(self.root.join("Balatro.exe"))
            .current_dir(&self.root)
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

    async fn preflight(&self) -> Result<Value, Value> {
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

    async fn node(&self, args: &[String]) -> Result<Value, String> {
        let mut all = vec![
            self.root
                .join("tools/balatro-info-db/bin/balatro-info.js")
                .display()
                .to_string(),
        ];
        all.extend(args.iter().cloned());
        self.run_json("node", &all, 15.0).await
    }

    async fn replay(&self, args: &[String]) -> Result<Value, String> {
        let mut all = vec![self.root.join("agent/replays.py").display().to_string()];
        all.extend(args.iter().cloned());
        self.run_json("python", &all, 15.0).await
    }

    fn diagnostic(&self, lines: u32) -> Value {
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

fn value_state(data: &Value) -> Option<Value> {
    data.pointer("/game/state")
        .cloned()
        .or_else(|| data.get("state").cloned())
}
fn sanitize(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(sanitize).collect()),
        Value::Object(mut map) => {
            map.remove("command");
            map.remove("raw_command");
            let hidden = matches!(map.get("face_down"), Some(Value::Bool(true)))
                || matches!(map.get("facing"), Some(Value::String(x)) if x == "back");
            if hidden {
                return json!({"index": map.get("index"), "instance_id": map.get("instance_id"), "hidden": true});
            }
            Value::Object(
                map.into_iter()
                    .map(|(key, value)| (key, sanitize(value)))
                    .collect(),
            )
        }
        other => other,
    }
}
fn envelope(ok: bool, data: Value, code: &str, message: &str) -> Value {
    let data = sanitize(data);
    let state = value_state(&data).unwrap_or(Value::Null);
    let decision_id = data.get("decision_id").cloned().unwrap_or(Value::Null);
    let legal = data
        .get("legal_actions")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let mut answer = Map::new();
    answer.insert("ok".into(), Value::Bool(ok));
    answer.insert("state".into(), state);
    answer.insert("decision_id".into(), decision_id);
    answer.insert("legal_actions".into(), legal);
    answer.insert("data".into(), data);
    if !ok {
        answer.insert("error".into(), json!({"code": code, "message": message}));
    }
    Value::Object(answer)
}
fn tool(value: Value) -> Result<CallToolResult, rmcp::ErrorData> {
    let error = value.get("ok") == Some(&Value::Bool(false));
    Ok(if error {
        CallToolResult::structured_error(value)
    } else {
        CallToolResult::structured(value)
    })
}

#[tool_router]
impl Server {
    #[tool(description = "Check process count, seed, bridge freshness, and resumable-save safety.")]
    async fn game_status(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.status().await {
            Ok(data) => tool(envelope(true, data, "", "")),
            Err(e) => tool(envelope(false, Value::Null, "status_failed", &e)),
        }
    }

    #[tool(
        description = "Ensure the fixed Balatro.exe is running without UI actions or new-run creation. Refuses multiple processes."
    )]
    async fn ensure_runtime(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let _guard = self.mutations.lock().await;
        match self.ensure_runtime_impl().await {
            Ok(data) => tool(envelope(true, data, "", "")),
            Err(e) => tool(envelope(false, Value::Null, "runtime_failed", &e)),
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
            return tool(envelope(
                false,
                Value::Null,
                "invalid_arguments",
                "invalid observe section",
            ));
        }
        match self.policy(40).await {
            Ok(data) => tool(envelope(
                true,
                compact_observation(data, &params.section),
                "",
                "",
            )),
            Err(e) => tool(envelope(false, Value::Null, "observe_failed", &e)),
        }
    }

    #[tool(
        description = "Return current decision_id, legal actions, rankings, and strategy analysis."
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
                tool(envelope(true, data, "", ""))
            }
            Err(e) => tool(envelope(false, Value::Null, "decision_failed", &e)),
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
            return tool(envelope(
                false,
                Value::Null,
                "invalid_arguments",
                "action_id and decision_id are required",
            ));
        }
        let _guard = self.mutations.lock().await;
        if let Err(problem) = self.preflight().await {
            return tool(problem);
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
            Ok(data) => tool(envelope(true, data, "", "")),
            Err(error) => {
                let current = self.policy(40).await.unwrap_or(Value::Null);
                let code = if error.contains("stale") {
                    "stale_decision"
                } else if error.contains("timeout") {
                    "timeout"
                } else {
                    "action_failed"
                };
                tool(envelope(false, current, code, &error))
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
            return tool(problem);
        }
        let steps = params.max_steps.clamp(1, 20).to_string();
        match self
            .controller(&["advance-safe", "--steps", &steps], 45.0)
            .await
        {
            Ok(_) => match self.policy(40).await {
                Ok(data) => tool(envelope(true, data, "", "")),
                Err(e) => tool(envelope(false, Value::Null, "advance_failed", &e)),
            },
            Err(e) => tool(envelope(false, Value::Null, "advance_failed", &e)),
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
                    return tool(envelope(true, data, "", ""));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return tool(envelope(
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
            Ok(data) => tool(envelope(true, data, "", "")),
            Err(e) => tool(envelope(false, Value::Null, "checkpoint_failed", &e)),
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
            return tool(envelope(
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
            Ok(data) => tool(envelope(true, data, "", "")),
            Err(e) => tool(envelope(false, Value::Null, "lookup_failed", &e)),
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
            Ok(data) => tool(envelope(true, data, "", "")),
            Err(e) => tool(envelope(false, Value::Null, "list_failed", &e)),
        }
    }
    #[tool(description = "Return counts and metadata for the vendored rules database.")]
    async fn rules_stats(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.node(&["stats".into()]).await {
            Ok(data) => tool(envelope(true, data, "", "")),
            Err(e) => tool(envelope(false, Value::Null, "stats_failed", &e)),
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
            Some(text) => tool(envelope(
                true,
                json!({"topic": params.topic, "guide": text}),
                "",
                "",
            )),
            None => tool(envelope(
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
            Ok(data) => tool(envelope(true, json!({"replays": data}), "", "")),
            Err(e) => tool(envelope(false, Value::Null, "replay_query_failed", &e)),
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
            Ok(data) => tool(envelope(true, data, "", "")),
            Err(e) => tool(envelope(false, Value::Null, "replay_log_failed", &e)),
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
        tool(envelope(true, data, "", ""))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo { capabilities: ServerCapabilities::builder().enable_tools().enable_resources().build(), server_info: Implementation { name: "balatro".into(), title: Some("Balatro safe gameplay MCP".into()), version: env!("CARGO_PKG_VERSION").into(), description: Some("Rust stdio boundary for safe Balatro gameplay.".into()), icons: None, website_url: None }, instructions: Some("Use only these MCP tools for Balatro. Start with game_status, then get_decision; execute only legal action_id with its decision_id.".into()), ..Default::default() }
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
            .map(|text| ReadResourceResult {
                contents: vec![ResourceContents::text(text, request.uri)],
            })
            .ok_or_else(|| rmcp::ErrorData::invalid_params("unknown guide topic", None));
        std::future::ready(result)
    }
}

fn compact_observation(data: Value, section: &str) -> Value {
    if section == "all" {
        return data;
    }
    let get = |key: &str| data.get(key).cloned().unwrap_or(Value::Null);
    match section {
        "hand" => {
            json!({"game": get("game"), "run": get("run"), "hand": data.pointer("/areas/hand").cloned(), "decision_id": get("decision_id"), "legal_actions": get("legal_actions")})
        }
        "build" => {
            json!({"game": get("game"), "run": get("run"), "jokers": data.pointer("/areas/jokers").cloned(), "consumables": data.pointer("/areas/consumeables").cloned(), "decision_id": get("decision_id"), "legal_actions": get("legal_actions")})
        }
        "blind" => {
            json!({"game": get("game"), "run": get("run"), "blind": get("blind"), "active_directives": get("active_directives"), "decision_id": get("decision_id"), "legal_actions": get("legal_actions")})
        }
        "hand_values" => {
            json!({"game": get("game"), "run": get("run"), "poker_hand_values": get("poker_hand_values"), "decision_id": get("decision_id"), "legal_actions": get("legal_actions")})
        }
        _ => {
            json!({"game": get("game"), "run": get("run"), "blind": get("blind"), "ready": get("ready"), "estimate_quality": get("estimate_quality"), "decision_id": get("decision_id"), "legal_actions": get("legal_actions")})
        }
    }
}
const GUIDE_TOPICS: &[&str] = &[
    "core",
    "hands",
    "actions",
    "economy",
    "blinds",
    "jokers",
    "cards",
    "consumables",
];

fn guide(topic: &str) -> Option<&'static str> {
    match topic.to_ascii_lowercase().as_str() {
        "core" | "rules" | "ante8" => Some(
            "Goal: clear Small, Big, and Boss blinds through Ante 8. Agent loop: game_status; query matching replays before each blind; get_decision; lookup unknown effects; take one legal action with current decision_id; observe the new state. Never infer face-down cards.",
        ),
        "hands" | "scoring" => Some(
            "Poker hands score Chips multiplied by Mult. Planet consumables level a named hand. Use live hand_values; controller scores are estimates unless explicitly exact.",
        ),
        "actions" | "discards" => Some(
            "Hands score; discards redraw without scoring. Consider all legal discard sizes and a specific draw goal. Use only legal actions.",
        ),
        "economy" | "shops" => Some(
            "Evaluate Joker slots, consumable slots, price, interest, and the next blind. Buy one item at a time, then reread decision state.",
        ),
        "blinds" | "bosses" | "debuffs" => Some(
            "Boss blinds impose special rules and debuffs. Read live blind state and lookup_rule before committing to a plan.",
        ),
        "jokers" | "editions" => Some(
            "Jokers affect Chips, Mult, economy, and rules. Foil adds Chips, Holographic adds Mult, Polychrome adds X Mult, Negative adds a Joker slot. Stickers are separate constraints.",
        ),
        "cards" | "enhancements" | "seals" => Some(
            "Playing cards may have one enhancement, edition, and seal. Face-down card identity is always unknown.",
        ),
        "consumables" | "vouchers" | "stakes" | "decks" | "tags" | "progression" => Some(
            "Decks, Stakes, Vouchers, Tags, and consumables change run rules and resources. Look up unfamiliar effects before acting.",
        ),
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = env::current_dir()?;
    let server = Server::new(root).map_err(std::io::Error::other)?;
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_commands_and_hidden_identity() {
        let clean = sanitize(json!({
            "legal_actions": [{"id": "play:1", "command": {"action": "play"}}],
            "areas": {"hand": [{"index": 2, "instance_id": 91, "face_down": true, "rank": "Ace", "suit": "Spades"}]}
        }));
        assert!(clean.pointer("/legal_actions/0/command").is_none());
        assert_eq!(
            clean.pointer("/areas/hand/0/hidden"),
            Some(&Value::Bool(true))
        );
        assert!(clean.pointer("/areas/hand/0/rank").is_none());
    }

    #[test]
    fn stable_error_envelope_contains_recovery_fields() {
        let result = envelope(
            false,
            json!({"decision_id": "new", "legal_actions": [{"id": "play:new"}]}),
            "stale_decision",
            "stale",
        );
        assert_eq!(result["ok"], false);
        assert_eq!(result["decision_id"], "new");
        assert_eq!(result["legal_actions"][0]["id"], "play:new");
        assert_eq!(result["error"]["code"], "stale_decision");
    }
}
