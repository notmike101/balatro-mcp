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
use std::{collections::HashSet, env, fs, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Mutex;

use crate::backend::{
    ipc::{IpcPaths, advance_safe_internal, execute_policy_action, start_new_run},
    policy::{SAFE_TRANSITION_ACTIONS, build_policy_state},
    replay::ReplayDB,
    runtime::{self, balatro_processes, observation_age},
    scoring::score_hand,
    state::StateDB,
};
use crate::crash;
use crate::guide::{GUIDE_TOPICS, guide};
use crate::models::*;
use crate::protocol::{compact_observation, envelope, sanitize, tool as to_tool_result};
use crate::rules::{LookupOptions, RulesDb, default_db_path};

use std::sync::LazyLock;
static EMPTY_BRIDGE: LazyLock<serde_json::Map<String, Value>> = LazyLock::new(serde_json::Map::new);

const CANONICAL_PLAY_LIMIT: usize = 40;
const CANONICAL_DISCARD_LIMIT: usize = 40;
const CANONICAL_TARGET_LIMIT: usize = 60;
const MAX_ACTION_PAGE: usize = 256;

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
    let mut basis = observation.clone();
    if let Some(object) = basis.as_object_mut() {
        // Bridge sequence/timestamp/last-response fields change on every poll
        // but do not change the legal action set.
        object.remove("bridge");
        object.remove("observed_at_ms");
    }
    digest("d3", &basis)
}

fn semantic_decision_id_for(policy: &Value) -> String {
    let basis = json!({
        "game": policy.get("game"),
        "legal_actions": policy.get("legal_actions"),
        "decision_checks": policy.get("decision_checks"),
        "economy": policy.get("economy"),
        "slots": policy.get("slots"),
        "run_phase": policy.get("run_phase"),
        "score_pressure": policy.get("score_pressure"),
    });
    digest("d4", &basis)
}

fn canonical_policy_decision_id(observation: &Value) -> String {
    let state = build_policy_state(
        observation,
        CANONICAL_PLAY_LIMIT,
        CANONICAL_DISCARD_LIMIT,
        CANONICAL_TARGET_LIMIT,
    );
    semantic_decision_id_for(&state)
}

fn page_legal_actions(
    data: &mut Value,
    action_type: &str,
    requested_offset: u32,
    requested_limit: u32,
) {
    let Some(object) = data.as_object_mut() else {
        return;
    };
    let actions = object
        .remove("legal_actions")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let filtered: Vec<Value> = actions
        .into_iter()
        .filter(|action| {
            action_type.is_empty()
                || action.get("action").and_then(Value::as_str) == Some(action_type)
        })
        .collect();
    let total = filtered.len();
    let offset = (requested_offset as usize).min(total);
    let requested_limit = if requested_limit == 0 {
        MAX_ACTION_PAGE
    } else {
        requested_limit as usize
    };
    let page_limit = requested_limit.min(MAX_ACTION_PAGE);
    let end = offset.saturating_add(page_limit).min(total);
    let page = filtered[offset..end].to_vec();

    object.insert("legal_actions".into(), Value::Array(page));
    object.insert("legal_actions_total".into(), json!(total));
    object.insert("legal_actions_offset".into(), json!(offset));
    object.insert("legal_actions_limit".into(), json!(page_limit));
    object.insert("legal_actions_truncated".into(), json!(end < total));
    object.insert(
        "legal_actions_next_offset".into(),
        if end < total { json!(end) } else { Value::Null },
    );
}

fn resolve_hand_selection_action(
    policy: &Value,
    template: &Value,
    card_indices: &[usize],
) -> Result<Value, String> {
    let action = template
        .get("action")
        .and_then(Value::as_str)
        .ok_or("hand selection action has no action type")?;
    if !matches!(action, "play" | "discard") {
        return Err("card_indices can only be used with play_selected or discard_selected".into());
    }
    if card_indices.is_empty() {
        return Err("card_indices must contain at least one 1-based hand position".into());
    }

    let hand = policy
        .get("hand")
        .and_then(Value::as_array)
        .ok_or("current policy has no hand")?;
    let max_cards = template
        .get("max_cards")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(hand.len());
    if card_indices.len() > max_cards {
        return Err(format!(
            "too many cards for {action}; maximum is {max_cards}"
        ));
    }

    let mut seen = HashSet::with_capacity(card_indices.len());
    let mut card_ids = Vec::with_capacity(card_indices.len());
    for index in card_indices {
        if *index == 0 || *index > hand.len() {
            return Err(format!(
                "hand card index {index} is not available; indices are 1-based"
            ));
        }
        if !seen.insert(*index) {
            return Err(format!(
                "hand card index {index} was supplied more than once"
            ));
        }
        let card = &hand[*index - 1];
        let id = card
            .get("instance_id")
            .or_else(|| card.get("id"))
            .and_then(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| (!value.is_null()).then(|| value.to_string()))
            })
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("hand card index {index} has no stable card id"))?;
        card_ids.push(id);
    }

    let mut resolved = template.clone();
    resolved["cards"] = json!(card_indices);
    resolved["card_indices"] = json!(card_indices);
    resolved["card_ids"] = json!(card_ids);
    Ok(resolved)
}

fn resolve_encoded_selection_action(policy: &Value, action_id: &str) -> Option<Value> {
    let (action, encoded_ids) = action_id.split_once('_')?;
    if !matches!(action, "play" | "discard") || encoded_ids.is_empty() {
        return None;
    }
    let hand = policy.get("hand")?.as_array()?;
    let mut indices = Vec::new();
    let mut card_ids = Vec::new();
    for requested_id in encoded_ids.split('_') {
        let (index, card) = hand.iter().enumerate().find(|(_, card)| {
            card.get("instance_id")
                .or_else(|| card.get("id"))
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string())
                })
                .as_deref()
                == Some(requested_id)
        })?;
        if indices.contains(&(index + 1)) {
            return None;
        }
        indices.push(index + 1);
        card_ids.push(card.get("instance_id").or_else(|| card.get("id"))?.clone());
    }
    Some(json!({
        "action_id": action_id,
        "action": action,
        "cards": indices,
        "card_indices": indices,
        "card_ids": card_ids,
    }))
}

fn replay_outcome_filter(outcome: &str) -> Option<String> {
    let normalized = outcome.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "all" => None,
        "best" | "clear" => Some("clear".into()),
        "fail" => Some("fail".into()),
        _ => Some(normalized),
    }
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

fn normalize_info_type(entity_type: &str) -> String {
    let normalized = entity_type.to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "back" | "decks" => "deck".into(),
        "playingcards" => "playing-card".into(),
        _ => normalized,
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
    pub rules_db: Arc<RulesDb>,
    pub(crate) process_override: Arc<Mutex<Option<Vec<Value>>>>,
    pub(crate) status_override: Arc<Mutex<Option<Value>>>,
    pub(crate) policy_failure_override: Arc<Mutex<bool>>,
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
            root: root.clone(),
            runtime_root,
            ipc,
            mutations: Arc::new(Mutex::new(())),
            replay_db,
            state_db,
            rules_db: Arc::new(RulesDb::new(default_db_path(&root))),
            process_override: Arc::new(Mutex::new(None)),
            status_override: Arc::new(Mutex::new(None)),
            policy_failure_override: Arc::new(Mutex::new(false)),
            tool_router: Self::tool_router(),
        })
    }

    /// Read the observation JSON from the Lua bridge.
    pub fn read_observation(&self) -> Result<Value, String> {
        self.ipc.read_observation()
    }

    async fn policy(&self, limit: u32) -> Result<Value, String> {
        if *self.policy_failure_override.lock().await {
            return Err("deterministic policy failure".into());
        }
        let observation = self.read_observation()?;
        let mut state = build_policy_state(
            &observation,
            (limit as usize).min(CANONICAL_PLAY_LIMIT),
            CANONICAL_DISCARD_LIMIT,
            CANONICAL_TARGET_LIMIT,
        );
        let decision_id = canonical_policy_decision_id(&observation);
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
        object.insert("estimate_quality".into(), score["estimate_quality"].clone());
        Ok(state)
    }

    async fn status(&self) -> Value {
        if let Some(status) = self.status_override.lock().await.clone() {
            return status;
        }
        let processes = self
            .process_override
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| balatro_processes().unwrap_or_default());
        let observation = self.read_observation();
        let obs_path = self.ipc.observation_path.clone();
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
        let state = observation
            .as_ref()
            .ok()
            .and_then(|o| {
                o.pointer("/game/state_name")
                    .or_else(|| o.pointer("/game/state"))
            })
            .cloned()
            .unwrap_or(Value::Null);
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
        } else {
            problems.push("seed missing from the live observation".into());
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
            "state": state,
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
            Ok(status)
        }
    }

    async fn preflight_new_run(&self, confirm_override: bool) -> Result<Value, Value> {
        let status = self.status().await;
        let process_count = status
            .get("processes")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if process_count != 1 {
            return Err(envelope(
                false,
                status,
                "process_count",
                "exactly one Balatro.exe process required",
            ));
        }
        let observation = match self.read_observation() {
            Ok(value) => value,
            Err(error) => {
                return Err(envelope(false, status, "observation_unavailable", &error));
            }
        };
        let age = status
            .get("observation_age_seconds")
            .and_then(Value::as_f64);
        let processes = status
            .get("processes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if let Err(error) = runtime::validate_runtime(&observation, processes, age) {
            return Err(envelope(
                false,
                status,
                "unsafe_runtime",
                &error.to_string(),
            ));
        }
        let state = observation
            .pointer("/game/state_name")
            .and_then(Value::as_str)
            .or_else(|| observation.pointer("/game/state").and_then(Value::as_str))
            .or_else(|| observation.get("state").and_then(Value::as_str))
            .unwrap_or_default();
        let saved_present = observation
            .get("ready")
            .and_then(|ready| ready.get("saved_game_present"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || observation
                .get("ready")
                .and_then(|ready| ready.get("saved_game_seed"))
                .and_then(Value::as_str)
                .is_some();
        if !state.eq_ignore_ascii_case("MENU")
            && !state.eq_ignore_ascii_case("MAIN_MENU")
            && !state.eq_ignore_ascii_case("GAME_OVER")
            && !(confirm_override && saved_present)
        {
            return Err(envelope(
                false,
                status,
                "invalid_state",
                "start_new_run requires the Balatro main menu",
            ));
        }
        let saved_seed = observation
            .get("ready")
            .and_then(|ready| ready.get("saved_game_seed"))
            .and_then(Value::as_str);
        let saved_present = observation
            .get("ready")
            .and_then(|ready| ready.get("saved_game_present"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || saved_seed.is_some();
        if saved_present && !confirm_override {
            return Err(envelope(
                false,
                status,
                "confirmation_required",
                "an existing saved run will be replaced; call start_new_run with confirm_override=true",
            ));
        }
        Ok(status)
    }

    pub fn diagnostic(&self, lines: u32) -> Value {
        diagnostic_for_appdata(env::var_os("APPDATA"), lines)
    }
}

fn diagnostic_for_appdata(appdata: Option<std::ffi::OsString>, lines: u32) -> Value {
    let appdata = appdata
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:/Users/me/AppData/Roaming"));
    let directory = appdata.join("Balatro/Mods/lovely/log");
    diagnostic_from_directory(&directory, lines)
}

fn diagnostic_from_directory(directory: &std::path::Path, lines: u32) -> Value {
    let latest = fs::read_dir(&directory).ok().and_then(|items| {
        items
            .filter_map(Result::ok)
            .filter_map(|x| {
                let modified = x
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
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

fn wait_state_matches(data: &Value, requested: &str) -> bool {
    let state = data
        .pointer("/game/state_name")
        .or_else(|| data.pointer("/game/state"))
        .or_else(|| data.get("state"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if requested.is_empty() {
        !state.is_empty()
    } else {
        state.eq_ignore_ascii_case(requested)
    }
}

fn observation_state(data: &Value) -> &str {
    data.pointer("/game/state_name")
        .or_else(|| data.pointer("/game/state"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn synchronize_bridge_response_state(mut response: Value, post_state: &Value) -> Value {
    let state = observation_state(post_state);
    if !state.is_empty() {
        if let Some(object) = response.as_object_mut() {
            object.insert("state".into(), json!(state));
        }
    }
    response
}

fn wait_state_summary(data: Value) -> Value {
    let legal_actions_available = data
        .get("legal_actions")
        .and_then(Value::as_array)
        .is_some_and(|actions| !actions.is_empty());
    let mut summary = compact_observation(data, "summary");
    if let Some(object) = summary.as_object_mut() {
        object.insert("read_only".into(), json!(true));
        object.insert(
            "legal_actions_available".into(),
            json!(legal_actions_available),
        );
        object.insert("next_step".into(), json!("get_decision"));
    }
    summary
}

fn state_result(result: Result<Value, String>) -> Result<CallToolResult, rmcp::ErrorData> {
    match result {
        Ok(data) => to_tool_result(envelope(true, data, "", "")),
        Err(error) => to_tool_result(envelope(false, Value::Null, "run_state_failed", &error)),
    }
}

fn observation_error_result(error: &str) -> Result<CallToolResult, rmcp::ErrorData> {
    to_tool_result(envelope(false, Value::Null, "observation_failed", error))
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
        description = "Verify that the fixed Balatro.exe is running and report bridge/seed safety. This tool never launches Balatro, performs UI actions, or creates a new run; it refuses multiple processes."
    )]
    async fn ensure_runtime(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let _guard = match self.mutations.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                return to_tool_result(envelope(
                    false,
                    Value::Null,
                    "mutation_busy",
                    "another mutation is already in progress",
                ));
            }
        };
        match self.ensure_runtime_impl().await {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(e) => to_tool_result(envelope(false, Value::Null, "runtime_failed", &e)),
        }
    }

    #[tool(
        description = "Always start a new run with exactly seed 2K9H9HN. If a saved run exists, confirm replacement with confirm_override=true. Requires one Balatro process, a fresh loaded bridge, and the main menu; never accepts or creates another seed."
    )]
    async fn start_new_run(
        &self,
        Parameters(params): Parameters<StartNewRunParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let _guard = match self.mutations.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                return to_tool_result(envelope(
                    false,
                    Value::Null,
                    "mutation_busy",
                    "another mutation is already in progress",
                ));
            }
        };
        if let Err(problem) = self.preflight_new_run(params.confirm_override).await {
            return to_tool_result(problem);
        }
        let ipc = self.ipc.clone();
        match tokio::task::spawn_blocking(move || start_new_run(&ipc, params.confirm_override))
            .await
        {
            Ok(Ok(data)) => {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
                let mut next = Value::Null;
                loop {
                    if let Ok(policy) = self.policy(40).await {
                        let state = policy
                            .pointer("/game/state")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        next = policy;
                        if !state.eq_ignore_ascii_case("MENU")
                            && !state.eq_ignore_ascii_case("SPLASH")
                        {
                            break;
                        }
                    }
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                if next.is_null() {
                    next = json!({"legal_actions": []});
                }
                let bridge_response = synchronize_bridge_response_state(data, &next);
                if let Some(object) = next.as_object_mut() {
                    object.insert("bridge_response".into(), bridge_response);
                }
                to_tool_result(envelope(true, next, "", ""))
            }
            Ok(Err(error)) => {
                to_tool_result(envelope(false, Value::Null, "new_run_failed", &error))
            }
            Err(error) => to_tool_result(envelope(
                false,
                Value::Null,
                "new_run_failed",
                &format!("new run worker failed: {error}"),
            )),
        }
    }

    #[tool(
        description = "Read targeted live state. This is read-only and does not return a decision_id or legal_actions; call get_decision for actionable state. Valid sections: summary, hand, build, blind, hand_values, all."
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
        if params.section == "all" {
            return match self.read_observation() {
                Ok(data) => to_tool_result(envelope(true, data, "", "")),
                Err(e) => to_tool_result(envelope(false, Value::Null, "observe_failed", &e)),
            };
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
        description = "Return the current decision_id, a compact legal-action list with caller-supplied play_selected/discard_selected templates for arbitrary positions, rankings, and strategy analysis. When legal_actions_truncated is true, call get_decision again with action_offset set to legal_actions_next_offset and an explicit action_limit; repeat until legal_actions_next_offset is null. The same pagination contract applies after action_type filtering. In GAME_OVER, from_game_over is a game-specific ui_click and return_to_menu is also available as a safe_transition. Examine decision_checks.ordering.hand_order and decision_checks.ordering.joker_order when jokers are present and trigger sequence can affect scoring. Examine decision_checks.consumables when Tarot/Planet/Spectral cards are owned or available in shop; use or sell before exiting or advancing. Examine decision_checks.shop and decision_checks.slots during SHOP state to track remaining items and indexes after each purchase."
    )]
    async fn get_decision(
        &self,
        Parameters(params): Parameters<DecisionParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.policy(params.limit).await {
            Ok(mut data) => {
                page_legal_actions(
                    &mut data,
                    &params.action_type,
                    params.action_offset,
                    params.action_limit,
                );
                to_tool_result(envelope(true, data, "", ""))
            }
            Err(e) => to_tool_result(envelope(false, Value::Null, "decision_failed", &e)),
        }
    }

    #[tool(
        description = "Execute exactly one current legal action using action_id and a non-empty decision_id. For arbitrary play/discard selections, use the legal play_selected or discard_selected action with 1-based card_indices. If the live state changes and stale_decision is returned, use the returned current decision_id and legal_actions, then retry the action. The current legal action set is authoritative because bridge polling can refresh snapshots between tool calls; mutations are serialized and checkpointed."
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
        let _guard = match self.mutations.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                return to_tool_result(envelope(
                    false,
                    Value::Null,
                    "mutation_busy",
                    "another mutation is already in progress",
                ));
            }
        };
        if let Err(problem) = self.preflight().await {
            return to_tool_result(problem);
        }
        let current = self.policy(40).await.unwrap_or(Value::Null);
        let current_decision_id = current
            .get("decision_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if current_decision_id.is_empty() {
            return to_tool_result(envelope(
                false,
                current,
                "decision_failed",
                "current observation has no decision_id",
            ));
        }
        if params.decision_id != current_decision_id {
            return to_tool_result(envelope(
                false,
                current,
                "stale_decision",
                "decision_id is stale because the live state changed; use the returned current decision_id and legal_actions, and do not retry the previous action",
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
            })
            .cloned();
        let action = match selected {
            None => match resolve_encoded_selection_action(&current, &params.action_id) {
                Some(action) => action,
                None => {
                    return to_tool_result(envelope(
                        false,
                        current,
                        "action_not_found",
                        "action_id is not in the current legal action set",
                    ));
                }
            },
            Some(selected) => {
                let caller_supplies_selection =
                    selected.get("selection").and_then(Value::as_str) == Some("caller_supplied");
                if caller_supplies_selection {
                    if params.card_indices.is_empty() {
                        return to_tool_result(envelope(
                            false,
                            current,
                            "invalid_arguments",
                            "card_indices is required for play_selected/discard_selected",
                        ));
                    }
                    match resolve_hand_selection_action(&current, &selected, &params.card_indices) {
                        Ok(action) => action,
                        Err(error) => {
                            return to_tool_result(envelope(
                                false,
                                current,
                                "invalid_arguments",
                                &error,
                            ));
                        }
                    }
                } else {
                    // Concrete play/discard action IDs already contain their card
                    // selection. Ignore redundant card_indices for compatibility.
                    selected
                }
            }
        };
        let ipc = self.ipc.clone();
        let action_id = params.action_id.clone();
        let decision_id = current_decision_id.clone();
        let before_decision_id = current_decision_id.clone();
        let before_state = current
            .pointer("/game/state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        match tokio::task::spawn_blocking(move || {
            execute_policy_action(
                &ipc,
                &action_id,
                &decision_id,
                Some(&action),
                30,
                15,
                60,
                settle,
            )
        })
        .await
        {
            Ok(Ok(data)) => {
                if data.get("ok").and_then(Value::as_bool) == Some(false) {
                    let message = data
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("bridge rejected the policy action");
                    let current = self.policy(40).await.unwrap_or(Value::Null);
                    return to_tool_result(envelope(
                        false,
                        current,
                        action_error_code(message),
                        message,
                    ));
                }
                let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(settle);
                let mut changed = false;
                loop {
                    if let Ok(observation) = self.read_observation() {
                        let semantic_state = build_policy_state(&observation, 40, 40, 60);
                        let decision_id = semantic_decision_id_for(&semantic_state);
                        changed = decision_id != before_decision_id
                            || observation_state(&observation) != before_state;
                        if changed {
                            break;
                        }
                    }
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                let mut next = if changed {
                    self.policy(40)
                        .await
                        .unwrap_or_else(|_| json!({"legal_actions": []}))
                } else {
                    json!({"legal_actions": []})
                };
                let bridge_response = synchronize_bridge_response_state(data, &next);
                if let Some(object) = next.as_object_mut() {
                    object.insert("bridge_response".into(), bridge_response);
                }
                if !changed {
                    return to_tool_result(envelope(
                        false,
                        next,
                        "action_failed",
                        "bridge acknowledged the action but the live decision did not change",
                    ));
                }
                to_tool_result(envelope(true, next, "", ""))
            }
            Ok(Err(error)) => {
                let current = self.policy(40).await.unwrap_or(Value::Null);
                let code = action_error_code(&error);
                to_tool_result(envelope(false, current, code, &error))
            }
            Err(error) => to_tool_result(envelope(
                false,
                Value::Null,
                "action_failed",
                &format!("action worker failed: {error}"),
            )),
        }
    }

    #[tool(description = "Advance only confirmed non-strategic transitions.")]
    async fn advance_safe(
        &self,
        Parameters(params): Parameters<AdvanceParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let _guard = match self.mutations.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                return to_tool_result(envelope(
                    false,
                    Value::Null,
                    "mutation_busy",
                    "another mutation is already in progress",
                ));
            }
        };
        if let Err(problem) = self.preflight().await {
            return to_tool_result(problem);
        }
        let steps = params.max_steps.clamp(1, 20) as u32;
        let before_policy = match self.policy(20).await {
            Ok(value) => value,
            Err(error) => {
                return to_tool_result(envelope(false, Value::Null, "advance_failed", &error));
            }
        };
        let before_state = before_policy
            .pointer("/game/state")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let available: Vec<String> = before_policy
            .get("legal_actions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|action| {
                action.get("action").and_then(Value::as_str) == Some("safe_transition")
            })
            .filter_map(|action| {
                action
                    .get("action_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        if available.is_empty() {
            return to_tool_result(envelope(
                false,
                before_policy,
                "advance_failed",
                "no safe transition is legal in the current state",
            ));
        }
        // Try each safe transition action from the policy module.
        for action in SAFE_TRANSITION_ACTIONS {
            if !available.iter().any(|candidate| candidate == action) {
                continue;
            }
            match advance_safe_internal(&self.ipc, action, steps) {
                Ok(bridge_data) => {
                    match self.policy(40).await {
                        Ok(policy_data) => {
                            let after_state =
                                policy_data.pointer("/game/state").and_then(Value::as_str);
                            if before_state.as_deref() != after_state {
                                let mut result = policy_data;
                                if let Some(object) = result.as_object_mut() {
                                    object.insert("bridge_response".into(), bridge_data);
                                }
                                return to_tool_result(envelope(true, result, "", ""));
                            }
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

    #[tool(
        description = "Wait read-only for an exact state, or for any stable state when blank. The result confirms state only and does not include legal_actions; call get_decision after the wait to obtain a decision_id and actions."
    )]
    async fn wait_for_state(
        &self,
        Parameters(params): Parameters<WaitParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let timeout = params.timeout.clamp(0.1, 60.0);
        let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(timeout);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return to_tool_result(envelope(
                    false,
                    Value::Null,
                    "timeout",
                    "timed out waiting for state",
                ));
            }
            if let Ok(Ok(Ok(observation))) = tokio::time::timeout(
                remaining,
                tokio::task::spawn_blocking({
                    let ipc = self.ipc.clone();
                    move || ipc.read_observation()
                }),
            )
            .await
            {
                if wait_state_matches(&observation, &params.state) {
                    return match self.policy(20).await {
                        Ok(data) => {
                            to_tool_result(envelope(true, wait_state_summary(data), "", ""))
                        }
                        Err(error) => {
                            to_tool_result(envelope(false, Value::Null, "wait_failed", &error))
                        }
                    };
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
        let _guard = match self.mutations.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                return to_tool_result(envelope(
                    false,
                    Value::Null,
                    "mutation_busy",
                    "another mutation is already in progress",
                ));
            }
        };
        if let Err(problem) = self.preflight().await {
            return to_tool_result(problem);
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
                    to_tool_result(envelope(false, Value::Null, "checkpoint_failed", &error))
                }
            },
            Err(error) => to_tool_result(envelope(false, Value::Null, "checkpoint_failed", &error)),
        }
    }

    #[tool(
        description = "Look up a Balatro entity and compatible editions, enhancements, seals, stickers, or suit. Entity type is case-insensitive (for example, Joker or joker)."
    )]
    async fn lookup_rule(
        &self,
        Parameters(params): Parameters<LookupParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let entity_type = normalize_info_type(&params.entity_type);
        if !INFO_TYPES.contains(&entity_type.as_str()) {
            return to_tool_result(envelope(
                false,
                Value::Null,
                "invalid_arguments",
                "unsupported entity type",
            ));
        }
        let options = LookupOptions {
            suit: (!params.suit.is_empty()).then_some(params.suit),
            edition: (!params.edition.is_empty()).then_some(params.edition),
            enhancement: (!params.enhancement.is_empty()).then_some(params.enhancement),
            seal: (!params.seal.is_empty()).then_some(params.seal),
            stickers: params.stickers,
        };
        match self.rules_db.lookup(&entity_type, &params.name, &options) {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(e) => to_tool_result(envelope(false, Value::Null, "lookup_failed", &e)),
        }
    }

    #[tool(description = "List static Balatro entities, optionally by entity_type.")]
    async fn list_rules(
        &self,
        Parameters(params): Parameters<ListParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let entity_type = normalize_info_type(&params.entity_type);
        match self
            .rules_db
            .list((!entity_type.is_empty()).then_some(entity_type.as_str()))
        {
            Ok(data) => to_tool_result(envelope(true, data, "", "")),
            Err(e) => to_tool_result(envelope(false, Value::Null, "list_failed", &e)),
        }
    }
    #[tool(description = "Return counts and metadata for the vendored rules database.")]
    async fn rules_stats(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.rules_db.stats() {
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
        let outcome_marker = replay_outcome_filter(&params.outcome);
        let result = self.replay_db.lock().await.query_replays(
            Some(SEED),
            Some(params.ante),
            Some(params.stake),
            if params.blind.is_empty() {
                None
            } else {
                Some(params.blind.as_str())
            },
            outcome_marker.as_deref(),
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
        description = "Score selected hand cards using the live poker-hand contract and explicit estimate metadata. card_indices uses 1-based hand positions, matching play/discard actions. When omitted, the live highlighted hand selection is used when present; otherwise the best five-card subset is scored for hands larger than five cards."
    )]
    async fn score_hand(
        &self,
        Parameters(params): Parameters<ScoreParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let selected_indices = if params.card_indices.is_empty() {
            None
        } else {
            let mut indices = Vec::with_capacity(params.card_indices.len());
            for index in params.card_indices {
                if index == 0 {
                    return to_tool_result(envelope(
                        false,
                        Value::Null,
                        "invalid_arguments",
                        "score_hand card_indices are 1-based and must be non-zero",
                    ));
                }
                indices.push(index - 1);
            }
            Some(indices)
        };
        match self.read_observation() {
            Ok(observation) => to_tool_result(envelope(
                true,
                serde_json::to_value(score_hand(&observation, selected_indices.as_deref()))
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
            return state_result(self.state_db.lock().await.current_run());
        }
        match self.read_observation() {
            Ok(observation) => state_result(
                self.state_db
                    .lock()
                    .await
                    .checkpoint(&observation, &params.kind),
            ),
            Err(error) => observation_error_result(&error),
        }
    }

    #[tool(
        description = "Read recent Rust-owned run events, newest checkpoint first. This is explicit checkpoint history, not an automatic live-observation stream; call run_state with a checkpoint kind to record the current observation."
    )]
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
        data["mcp_crash_log"] = crash::tail(
            &self.runtime_root.join("agent").join("mcp_crash.log"),
            params.lines,
        );
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
            "Use only these MCP tools for Balatro. Start with game_status; if the main menu has a non-target saved seed, use start_new_run to recover to 2K9H9HN; then get_decision. Examine decision_checks.ordering when jokers are present; examine decision_checks.consumables for owned or shop Tarot/Planet/Spectral; examine decision_checks.shop and decision_checks.slots during SHOP state; execute only a current legal action_id with its decision_id, refreshing and retrying on stale_decision. In ROUND_EVAL use proceed_round, then next_round in SHOP. For arbitrary hand selections, use play_selected or discard_selected with 1-based card_indices. score_hand also uses 1-based indices and hand_values returns the live contract. observe is read-only and wait_for_state returns next_step=get_decision; page get_decision from legal_actions_next_offset until it is null, including after action_type filtering. Use run_state(kind=checkpoint) before event_history when current live history is needed.",
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

    #[test]
    fn bridge_response_state_uses_post_action_state() {
        let response = synchronize_bridge_response_state(
            json!({"state": "SELECTING_HAND", "message": "queued"}),
            &json!({"game": {"state": "HAND_PLAYED"}}),
        );
        assert_eq!(response["state"], "HAND_PLAYED");
        assert_eq!(response["message"], "queued");
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
                    settle_timeout: 12.0,
                    card_indices: vec![]
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
        assert_eq!(normalize_info_type("Joker"), "joker");
        assert_eq!(normalize_info_type("playing_card"), "playing-card");
        assert_eq!(normalize_info_type("back"), "deck");
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
    async fn mutation_routes_fail_fast_without_runtime() {
        let (_dir, server) = server();
        let started = std::time::Instant::now();
        assert!(
            server
                .take_action(Parameters(ActionParams {
                    action_id: "play:1".into(),
                    decision_id: "d3-test".into(),
                    settle_timeout: 12.0,
                    card_indices: vec![],
                }))
                .await
                .is_ok()
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));

        let started = std::time::Instant::now();
        assert!(
            server
                .checkpoint(Parameters(CheckpointParams {
                    kind: "test".into()
                }))
                .await
                .is_ok()
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));

        let started = std::time::Instant::now();
        assert!(
            server
                .wait_for_state(Parameters(WaitParams {
                    state: "PLAY".into(),
                    timeout: 0.1,
                }))
                .await
                .is_ok()
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
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
        assert!(
            server
                .query_replays(Parameters(ReplayQueryParams {
                    ante: 1,
                    stake: 1,
                    blind: "Small".into(),
                    outcome: "fail".into(),
                }))
                .await
                .is_ok()
        );
        let all = server
            .query_replays(Parameters(ReplayQueryParams {
                ante: 1,
                stake: 1,
                blind: "Small".into(),
                outcome: "all".into(),
            }))
            .await
            .unwrap();
        let all = all.structured_content.unwrap();
        assert_eq!(all["ok"], true);
        assert_eq!(all["data"]["replays"].as_array().unwrap().len(), 2);
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
                    limit: 1,
                    action_limit: 80,
                    action_offset: 0
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .get_decision(Parameters(DecisionParams {
                    action_type: "play".into(),
                    limit: 10,
                    action_limit: 80,
                    action_offset: 0
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
                    card_indices: vec![1]
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
            "game": {"state": "SELECTING_HAND"},
            "round": {"seed": SEED},
            "run": {"hands_left": 4, "discards_left": 3},
            "ready": {},
            "bridge": {"loaded": true, "version": "0.6.0", "session_id": "test"},
            "areas": {"hand": [
                {"instance_id":"a", "base": {"rank": "A", "suit": "H"}, "suits": [{"key": "H"}]},
                {"instance_id":"b", "base": {"rank": "K", "suit": "D"}, "suits": [{"key": "D"}]}
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
        let observed = server
            .observe(Parameters(ObserveParams {
                section: "all".into(),
            }))
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert!(observed["data"]["areas"].is_object());
        assert!(observed["data"].get("legal_actions").is_none());
        let decision = server
            .get_decision(Parameters(DecisionParams {
                action_type: String::new(),
                limit: 10,
                action_limit: 2,
                action_offset: 0,
            }))
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(decision["data"]["legal_actions_offset"], 0);
        assert_eq!(decision["data"]["legal_actions_limit"], 2);
        assert_eq!(decision["data"]["legal_actions_truncated"], true);
        assert_eq!(decision["data"]["legal_actions_next_offset"], 2);
        assert!(
            decision["data"]["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["action_id"] == "discard_selected")
        );
        assert!(
            server
                .get_decision(Parameters(DecisionParams {
                    action_type: "play".into(),
                    limit: 10,
                    action_limit: 80,
                    action_offset: 0
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
        assert!(
            server
                .score_hand(Parameters(ScoreParams {
                    card_indices: vec![1]
                }))
                .await
                .is_ok()
        );
        assert!(server.hand_values().await.is_ok());
        let waited = server
            .wait_for_state(Parameters(WaitParams {
                state: "SELECTING_HAND".into(),
                timeout: 0.1,
            }))
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(waited["state"], "SELECTING_HAND");
        assert_eq!(waited["data"]["state"], "SELECTING_HAND");
        assert!(waited["data"].get("legal_actions").is_none());
        assert_eq!(waited["data"]["read_only"], true);
        assert_eq!(waited["data"]["legal_actions_available"], true);
        assert_eq!(waited["data"]["next_step"], "get_decision");
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
                .wait_for_state(Parameters(WaitParams {
                    state: "SHOP".into(),
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

        std::fs::remove_file(&server.ipc.observation_path).unwrap();
        std::fs::write(&server.ipc.response_path, r#"{"_decode_error":true}"#).unwrap();
        assert!(
            server
                .checkpoint(Parameters(CheckpointParams {
                    kind: "missing-observation".into()
                }))
                .await
                .is_ok()
        );
        write_fixture(&server);

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
                    action_id: "play_a".into(),
                    decision_id: "stale".into(),
                    settle_timeout: 1.0,
                    card_indices: vec![]
                }))
                .await
                .is_ok()
        );
        assert!(
            server
                .take_action(Parameters(ActionParams {
                    action_id: "play_a".into(),
                    decision_id: decision.clone(),
                    settle_timeout: 1.0,
                    card_indices: vec![]
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
        *server.policy_failure_override.lock().await = true;
        std::fs::write(&server.ipc.response_path, r#"{"_decode_error":true}"#).unwrap();
        assert!(
            server
                .advance_safe(Parameters(AdvanceParams { max_steps: 1 }))
                .await
                .is_ok()
        );
        *server.policy_failure_override.lock().await = false;
        let mut broken = server.clone();
        broken.ipc.command_path = std::path::PathBuf::from("Z:\\missing\\command.lua");
        assert!(
            broken
                .take_action(Parameters(ActionParams {
                    action_id: "play_a".into(),
                    decision_id: decision,
                    settle_timeout: 1.0,
                    card_indices: vec![]
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
    async fn state_and_replay_route_failures_are_structured() {
        let (dir, server) = server();
        write_fixture(&server);
        let bad_state_root = dir.path().join("bad-state");
        std::fs::create_dir_all(bad_state_root.join("agent/rust_state.db")).unwrap();
        *server.state_db.lock().await = StateDB::new(&bad_state_root);
        assert!(server.policy(1).await.is_ok());
        let saved_observation = std::fs::read(&server.ipc.observation_path).unwrap();
        std::fs::remove_file(&server.ipc.observation_path).unwrap();
        assert!(
            server
                .run_state(Parameters(StateParams {
                    kind: "checkpoint".into(),
                    limit: 1
                }))
                .await
                .is_ok()
        );
        std::fs::write(&server.ipc.observation_path, saved_observation).unwrap();
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
                .run_state(Parameters(StateParams {
                    kind: "checkpoint".into(),
                    limit: 1
                }))
                .await
                .is_ok()
        );
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
        let mismatch = active_directives(
            &json!({"rules":[{"conditions":{"/missing":"value"}}]}),
            &json!({"state":"PLAY"}),
        );
        assert!(mismatch.as_array().unwrap().is_empty());
        assert!(server.diagnostic(0).get("log_found").is_some());
        let empty_log_dir = tempdir().unwrap();
        assert_eq!(
            diagnostic_from_directory(empty_log_dir.path(), 5)["log_found"],
            false
        );
        assert!(diagnostic_for_appdata(None, 5).get("log_found").is_some());
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
        let bridge_refresh_1 = json!({
            "game":{"state":"SELECTING_HAND"},
            "bridge":{"observation_seq":1,"observed_at_ms":100,"last_response":{"id":"1"}}
        });
        let bridge_refresh_2 = json!({
            "game":{"state":"SELECTING_HAND"},
            "bridge":{"observation_seq":2,"observed_at_ms":200,"last_response":{"id":"2"}}
        });
        assert_eq!(
            decision_id_for(&bridge_refresh_1),
            decision_id_for(&bridge_refresh_2)
        );
        let state_data = json!({"game":{"state":"PLAY"}});
        assert!(wait_state_matches(&state_data, "play"));
        assert!(wait_state_matches(&state_data, ""));
        assert!(!wait_state_matches(&state_data, "SHOP"));
        assert!(matches!(state_result(Ok(json!({}))), Ok(_)));
        assert!(matches!(state_result(Err("broken".into())), Ok(_)));
        assert!(matches!(observation_error_result("missing"), Ok(_)));
    }

    #[test]
    fn decision_pages_and_hand_selection_are_deterministic() {
        let observation = json!({
            "game": {"state": "SELECTING_HAND"},
            "run": {"hands_left": 2, "discards_left": 1, "blind": {"chips_required": 100}},
            "areas": {"hand": [
                {"instance_id": "a", "base": {"rank": "A"}},
                {"instance_id": "b", "base": {"rank": "K"}},
                {"instance_id": "c", "base": {"rank": "Q"}},
                {"instance_id": "d", "base": {"rank": "J"}},
                {"instance_id": "e", "base": {"rank": "10"}}
            ]}
        });
        let canonical = build_policy_state(&observation, 40, 40, 60);
        let limited = build_policy_state(&observation, 1, 40, 60);
        assert_eq!(
            canonical_policy_decision_id(&observation),
            semantic_decision_id_for(&canonical)
        );
        assert_ne!(
            semantic_decision_id_for(&limited),
            semantic_decision_id_for(&canonical)
        );

        let template = canonical["legal_actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|action| action["action_id"] == "play_selected")
            .unwrap();
        let resolved = resolve_hand_selection_action(&canonical, template, &[4, 5]).unwrap();
        assert_eq!(resolved["card_indices"], json!([4, 5]));
        assert_eq!(resolved["card_ids"], json!(["d", "e"]));
        let encoded = resolve_encoded_selection_action(&canonical, "play_d_e").unwrap();
        assert_eq!(encoded["card_indices"], json!([4, 5]));
        assert_eq!(encoded["card_ids"], json!(["d", "e"]));
        assert!(resolve_hand_selection_action(&canonical, template, &[0, 5]).is_err());
        assert!(resolve_hand_selection_action(&canonical, template, &[4, 4]).is_err());

        let mut page = json!({
            "legal_actions": [
                {"action": "play", "action_id": "p1"},
                {"action": "move_card", "action_id": "m1"},
                {"action": "move_card", "action_id": "m2"}
            ]
        });
        page_legal_actions(&mut page, "move_card", 1, 1);
        assert_eq!(page["legal_actions_total"], 2);
        assert_eq!(page["legal_actions"][0]["action_id"], "m2");
        assert_eq!(page["legal_actions_truncated"], false);
        assert_eq!(page["legal_actions_next_offset"], Value::Null);
        assert_eq!(replay_outcome_filter("all"), None);
        assert_eq!(replay_outcome_filter("BEST").as_deref(), Some("clear"));
        assert_eq!(replay_outcome_filter("fail").as_deref(), Some("fail"));
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
                    settle_timeout: 1.0,
                    card_indices: vec![]
                }))
                .await
                .is_ok()
        );
        assert!(server.preflight().await.is_err());

        *server.process_override.lock().await = Some(vec![json!({"pid": 1}), json!({"pid": 2})]);
        assert!(server.ensure_runtime().await.is_ok());

        write_fixture(&server);
        let saved_observation = std::fs::read(&server.ipc.observation_path).unwrap();
        *server.status_override.lock().await = Some(json!({
            "processes": [{"pid": 1}],
            "seed": SEED,
            "safe_for_mutation": true,
            "observation_age_seconds": 0.0
        }));
        std::fs::remove_file(&server.ipc.observation_path).unwrap();
        let unavailable = server.preflight().await.unwrap_err();
        assert_eq!(unavailable["error"]["code"], "observation_unavailable");
        std::fs::write(&server.ipc.observation_path, saved_observation).unwrap();
        *server.status_override.lock().await = None;
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
                    settle_timeout: 1.0,
                    card_indices: vec![]
                }))
                .await
                .is_ok()
        );

        wrong["round"]["seed"] = json!(SEED);
        wrong["bridge"] = json!({"loaded": true});
        assert!(server.game_status().await.is_ok());
        wrong["bridge"] = json!({"loaded": true, "version": 7});
        std::fs::write(
            &server.ipc.observation_path,
            serde_json::to_vec(&wrong).unwrap(),
        )
        .unwrap();
        assert!(server.game_status().await.is_ok());
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
                    settle_timeout: 1.0,
                    card_indices: vec![]
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
