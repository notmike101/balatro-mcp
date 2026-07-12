use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// File-based IPC paths for communicating with the Balatro Lovely bridge.
/// The Lua bridge reads COMMAND_PATH and writes OBSERVATION_PATH / RESPONSE_PATH.
#[derive(Clone)]
pub struct IpcPaths {
    pub command_path: PathBuf,
    pub observation_path: PathBuf,
    pub response_path: PathBuf,
}

impl IpcPaths {
    /// Build paths from the runtime root (APPDATA/Balatro directory).
    pub fn new(runtime_root: &Path) -> Self {
        let balatro_dir = runtime_root;
        Self {
            command_path: balatro_dir.join("codex_command.lua"),
            observation_path: balatro_dir.join("codex_observation.json"),
            response_path: balatro_dir.join("codex_response.json"),
        }
    }

    /// Read the current observation JSON from the bridge.
    pub fn read_observation(&self) -> Result<Value, String> {
        let text = std::fs::read_to_string(&self.observation_path)
            .map_err(|e| format!("cannot read observation: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("invalid observation JSON: {e}"))
    }

    /// Write a command as a Lua return expression for the bridge to pick up.
    pub fn write_command(&self, command: &Value) -> Result<Value, String> {
        let lua = to_lua_command(command);
        let tmp = self.command_path.with_extension("lua.tmp");
        std::fs::write(&tmp, format!("return {lua}\n"))
            .map_err(|e| format!("cannot write command: {e}"))?;
        std::fs::rename(&tmp, &self.command_path)
            .map_err(|e| format!("cannot rename command: {e}"))?;
        Ok(Value::String(format!(
            "wrote {}",
            self.command_path.display()
        )))
    }

    /// Wait for a response from the bridge matching the given command ID.
    pub fn wait_for_response(
        &self,
        command_id: &str,
        timeout_secs: f64,
    ) -> Result<Option<Value>, String> {
        let deadline = SystemTime::now() + std::time::Duration::from_secs_f64(timeout_secs);
        let interval = std::time::Duration::from_millis(50);
        loop {
            if let Ok(response) = self.read_response() {
                let id = response.get("id").and_then(|v| v.as_str()).unwrap_or("");
                if id == command_id || response.get("_decode_error").is_some() {
                    return Ok(Some(response));
                }
            }
            if SystemTime::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(interval);
        }
    }

    fn read_response(&self) -> Result<Value, String> {
        let text = std::fs::read_to_string(&self.response_path)
            .map_err(|e| format!("cannot read response: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("invalid response JSON: {e}"))
    }

    /// Get the next command ID based on the last response ID.
    #[allow(dead_code)]
    pub fn next_command_id(&self) -> String {
        let previous = self
            .read_response()
            .ok()
            .and_then(|v| {
                v.get("id")
                    .and_then(|v| v.as_str().map(|s| s.parse::<u64>().ok()).unwrap_or(None))
            })
            .unwrap_or(0);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        std::cmp::max(previous + 1, now).to_string()
    }
}

/// Convert a JSON Value to a Lua table expression.
fn to_lua_value(value: &Value) -> String {
    match value {
        Value::Null => "nil".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(f) = n.as_f64() {
                f.to_string()
            } else {
                n.to_string()
            }
        }
        Value::String(s) => lua_quote(s),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(to_lua_value).collect();
            format!("{{{}}}", items.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("[{}] = {}", lua_quote(k), to_lua_value(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

fn lua_quote(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("\"{}\"", escaped)
}

fn to_lua_command(command: &Value) -> String {
    to_lua_value(command)
}

/// Execute a policy action (play/discard/buy/etc.) via the Lua bridge.
/// Builds a command JSON, writes it to the IPC channel, and waits for the response.
pub fn execute_policy_action(
    paths: &IpcPaths,
    action_id: &str,
    decision_id: &str,
    selected_action: Option<&Value>,
    play_limit: u32,
    discard_limit: u32,
    target_limit: u32,
    settle_timeout: f64,
) -> Result<Value, String> {
    let mut command = policy_action_command(selected_action, action_id, decision_id)?;
    let command_id = paths.next_command_id();
    if let Some(object) = command.as_object_mut() {
        object.insert("id".into(), serde_json::json!(command_id));
        object.insert("play_limit".into(), serde_json::json!(play_limit));
        object.insert("discard_limit".into(), serde_json::json!(discard_limit));
        object.insert("target_limit".into(), serde_json::json!(target_limit));
        object.insert("settle_timeout".into(), serde_json::json!(settle_timeout));
    }
    paths.write_command(&command)?;
    let response = paths
        .wait_for_response(&command_id, 60.0)?
        .ok_or("no response from bridge for policy action")?;
    Ok(response)
}

/// Translate a Rust policy action into the concrete command understood by the
/// Lovely bridge. The policy action is kept internal and is sanitized before
/// it crosses the MCP boundary.
pub fn policy_action_command(
    selected_action: Option<&Value>,
    action_id: &str,
    decision_id: &str,
) -> Result<Value, String> {
    let action = selected_action.ok_or_else(|| format!("policy action not found: {action_id}"))?;
    let kind = action
        .get("action")
        .and_then(Value::as_str)
        .ok_or("policy action has no action type")?;
    let mut command = serde_json::Map::new();
    command.insert("decision_id".into(), serde_json::json!(decision_id));
    command.insert("action_id".into(), serde_json::json!(action_id));
    match kind {
        "play" | "discard" => {
            command.insert("action".into(), serde_json::json!(kind));
            command.insert(
                "cards".into(),
                action
                    .get("card_indices")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([])),
            );
            command.insert(
                "card_ids".into(),
                action
                    .get("card_ids")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([])),
            );
        }
        "select_blind" => {
            command.insert("action".into(), serde_json::json!("select_blind"));
            command.insert(
                "blind_type".into(),
                action.get("blind").cloned().unwrap_or(Value::Null),
            );
        }
        "skip_blind" => {
            command.insert("action".into(), serde_json::json!("skip_blind"));
            command.insert(
                "blind_type".into(),
                action.get("blind").cloned().unwrap_or(Value::Null),
            );
        }
        "ui_click" => {
            command.insert("action".into(), serde_json::json!("ui_click"));
            for key in ["ui_id", "button"] {
                if let Some(value) = action.get(key) {
                    command.insert(key.into(), value.clone());
                }
            }
        }
        "buy_card" => {
            command.insert("action".into(), serde_json::json!("buy"));
            command.insert(
                "area".into(),
                action
                    .get("area")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!("shop_jokers")),
            );
            command.insert(
                "index".into(),
                action.get("card_index").cloned().unwrap_or(Value::Null),
            );
            if let Some(card_id) = action.get("card_id") {
                command.insert("card_id".into(), card_id.clone());
            }
        }
        "use_consumable" => {
            command.insert("action".into(), serde_json::json!("use"));
            command.insert("area".into(), serde_json::json!("consumeables"));
            command.insert(
                "index".into(),
                action.get("card_index").cloned().unwrap_or(Value::Null),
            );
            if let Some(targets) = action.get("targets") {
                command.insert("targets".into(), targets.clone());
            }
        }
        "sell_card" => {
            command.insert("action".into(), serde_json::json!("sell"));
            command.insert(
                "area".into(),
                action
                    .get("area")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!("jokers")),
            );
            command.insert(
                "index".into(),
                action.get("card_index").cloned().unwrap_or(Value::Null),
            );
        }
        "move_card" | "move_joker" => {
            command.insert("action".into(), serde_json::json!(kind));
            for key in ["area", "from_index", "to_index", "card_id"] {
                if let Some(value) = action.get(key) {
                    command.insert(key.into(), value.clone());
                }
            }
        }
        "reroll" => {
            command.insert("action".into(), serde_json::json!("reroll_shop"));
        }
        "next_round" => {
            command.insert("action".into(), serde_json::json!("next_round"));
        }
        "choose_pack" => {
            command.insert("action".into(), serde_json::json!("use"));
            command.insert("area".into(), serde_json::json!("pack_cards"));
            command.insert(
                "index".into(),
                action.get("card_index").cloned().unwrap_or(Value::Null),
            );
        }
        "safe_transition" => {
            command.insert(
                "action".into(),
                action.get("transition").cloned().unwrap_or(Value::Null),
            );
        }
        "resume_run" => {
            command.insert("action".into(), serde_json::json!("start_run"));
        }
        _ => return Err(format!("unsupported policy action type: {kind}")),
    };
    Ok(Value::Object(command))
}

/// Execute a safe transition action (skip_tutorial, cash_out, etc.) via the Lua bridge.
pub fn advance_safe_internal(paths: &IpcPaths, action: &str, steps: u32) -> Result<Value, String> {
    let command_id = paths.next_command_id();
    let command = serde_json::json!({
        "id": command_id,
        "action": "safe_transition",
        "transition": action,
        "max_steps": steps,
    });
    paths.write_command(&command)?;
    let response = paths
        .wait_for_response(
            command.get("id").and_then(Value::as_str).unwrap_or(""),
            60.0,
        )?
        .ok_or("no response from bridge for safe_transition")?;
    Ok(response)
}

/// Persist the current observation into a checkpoint file.
pub fn checkpoint_internal(paths: &IpcPaths, kind: &str) -> Result<Value, String> {
    let command_id = paths.next_command_id();
    let command = serde_json::json!({
        "id": command_id,
        "action": "checkpoint",
        "kind": kind,
    });
    paths.write_command(&command)?;
    let response = paths
        .wait_for_response(
            command.get("id").and_then(Value::as_str).unwrap_or(""),
            30.0,
        )?
        .ok_or("no response from bridge for checkpoint")?;
    Ok(response)
}

/// Start the only permitted seed, optionally replacing an existing save.
/// The caller must have already performed the runtime and seed-recovery checks.
pub fn start_new_run(paths: &IpcPaths, confirm_override: bool) -> Result<Value, String> {
    let setup_id = paths.next_command_id();
    let setup = serde_json::json!({
        "id": setup_id,
        "action": "setup_new_run",
        "override_saved": confirm_override,
    });
    paths.write_command(&setup)?;
    let mut setup_response = paths
        .wait_for_response(&setup_id, 30.0)?
        .ok_or("no response from bridge for setup_new_run")?;
    if setup_response
        .get("message")
        .and_then(Value::as_str)
        .is_some_and(|message| message.contains("retry setup_new_run"))
    {
        let retry_id = paths.next_command_id();
        let retry = serde_json::json!({
            "id": retry_id,
            "action": "setup_new_run",
            "override_saved": confirm_override,
        });
        paths.write_command(&retry)?;
        setup_response = paths
            .wait_for_response(&retry_id, 30.0)?
            .ok_or("no response from bridge for setup_new_run retry")?;
    }
    if setup_response.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(setup_response
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("bridge rejected setup_new_run")
            .to_string());
    }

    let start_id = paths.next_command_id();
    let start = serde_json::json!({
        "id": start_id,
        "action": "start_run",
        "seed": crate::models::SEED,"override_saved": confirm_override,
    });
    paths.write_command(&start)?;
    let start_response = paths
        .wait_for_response(&start_id, 30.0)?
        .ok_or("no response from bridge for start_run")?;
    if start_response.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(start_response
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("bridge rejected start_run")
            .to_string());
    }
    Ok(start_response)
}

/// Execute a safe transition action from the pre-known list of safe actions.
/// Picks the first safe action not yet executed.
#[allow(dead_code)]
pub fn advance_safe_with_discovery(
    paths: &IpcPaths,
    safe_actions: &[&str],
    steps: u32,
) -> Result<Value, String> {
    for action in safe_actions {
        let command_id = paths.next_command_id();
        let cmd = serde_json::json!({
            "id": command_id,
            "action": "safe_transition",
            "transition": *action,
            "max_steps": steps,
        });
        paths.write_command(&cmd)?;
        let response = paths.wait_for_response(&command_id, 30.0)?;
        if response.is_some() {
            return Ok(response.unwrap_or(Value::Null));
        }
    }
    Err("no safe transition succeeded".into())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::IpcPaths;

    fn paths() -> (IpcPaths, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        (IpcPaths::new(dir.path()), dir)
    }

    #[test]
    fn lua_values_are_encoded_safely() {
        assert_eq!(super::to_lua_value(&json!(null)), "nil");
        assert_eq!(super::to_lua_value(&json!(true)), "true");
        assert_eq!(super::to_lua_value(&json!(12)), "12");
        assert_eq!(super::to_lua_value(&json!(1.5)), "1.5");
        assert_eq!(super::to_lua_value(&json!([1, "x"])), "{1, \"x\"}");
        assert_eq!(
            super::to_lua_value(&json!({"x": "a\"b"})),
            "{[\"x\"] = \"a\\\"b\"}"
        );
        assert!(super::to_lua_value(&json!("line\n\r\t\\")).contains("\\n"));
    }

    #[test]
    fn command_payload_has_expected_action_shape() {
        let value = json!({"action": "checkpoint", "kind": "test"});
        let encoded = super::to_lua_command(&value);
        assert!(encoded.contains("checkpoint"));
        assert!(encoded.contains("test"));
    }

    #[test]
    fn observation_and_command_round_trip() {
        let (ipc, _dir) = paths();
        std::fs::write(&ipc.observation_path, r#"{"state":"PLAY"}"#).unwrap();
        assert_eq!(ipc.read_observation().unwrap()["state"], "PLAY");
        ipc.write_command(&json!({"action":"play"})).unwrap();
        assert!(
            std::fs::read_to_string(&ipc.command_path)
                .unwrap()
                .contains("play")
        );
    }

    #[test]
    fn malformed_files_and_write_failures_are_reported() {
        let (ipc, _dir) = paths();
        std::fs::write(&ipc.observation_path, "not-json").unwrap();
        assert!(
            ipc.read_observation()
                .unwrap_err()
                .contains("invalid observation")
        );
        std::fs::write(&ipc.response_path, "not-json").unwrap();
        assert!(ipc.wait_for_response("x", 0.0).unwrap().is_none());

        let bad = IpcPaths::new(std::path::Path::new("Z:\\missing\\parent"));
        assert!(
            bad.write_command(&json!({}))
                .unwrap_err()
                .contains("cannot write")
        );
    }

    #[test]
    fn responses_match_decode_errors_and_ignore_stale_ids() {
        let (ipc, _dir) = paths();
        std::fs::write(&ipc.response_path, r#"{"id":"old"}"#).unwrap();
        assert!(ipc.wait_for_response("new", 0.0).unwrap().is_none());
        std::fs::write(&ipc.response_path, r#"{"_decode_error":"bad"}"#).unwrap();
        assert!(ipc.wait_for_response("new", 0.0).unwrap().is_some());
        std::fs::write(&ipc.response_path, r#"{"id":"42"}"#).unwrap();
        assert!(ipc.next_command_id().parse::<u128>().unwrap() >= 43);
        std::fs::write(&ipc.response_path, "{}").unwrap();
        assert!(ipc.next_command_id().parse::<u128>().is_ok());
    }

    #[test]
    fn command_helpers_use_expected_bridge_ids() {
        let (ipc, _dir) = paths();
        std::fs::write(&ipc.response_path, r#"{"_decode_error":true}"#).unwrap();
        let action =
            json!({"action_id":"play_1","action":"play","card_indices":[1],"card_ids":["1"]});
        assert!(
            super::execute_policy_action(&ipc, "play_1", "d", Some(&action), 1, 2, 3, 0.1).is_ok()
        );
        std::fs::write(&ipc.response_path, r#"{"_decode_error":true}"#).unwrap();
        assert!(super::advance_safe_internal(&ipc, "cash_out", 2).is_ok());
        std::fs::write(&ipc.response_path, r#"{"_decode_error":true}"#).unwrap();
        assert!(super::checkpoint_internal(&ipc, "manual").is_ok());
        std::fs::write(&ipc.response_path, r#"{"_decode_error":true}"#).unwrap();
        assert!(super::advance_safe_with_discovery(&ipc, &["first", "second"], 1).is_ok());
        assert!(super::advance_safe_with_discovery(&ipc, &[], 1).is_err());
    }

    #[test]
    fn policy_actions_translate_to_live_bridge_commands() {
        let non_contiguous = json!({
            "action": "play",
            "card_indices": [4, 5],
            "card_ids": ["1617", "1604"]
        });
        let non_contiguous_command =
            super::policy_action_command(Some(&non_contiguous), "play_1617_1604", "decision")
                .unwrap();
        assert_eq!(non_contiguous_command["action"], "play");
        assert_eq!(non_contiguous_command["cards"], json!([4, 5]));
        assert_eq!(non_contiguous_command["card_ids"], json!(["1617", "1604"]));

        let cases = [
            (json!({"action":"discard","card_indices":[1]}), "discard"),
            (
                json!({"action":"select_blind","blind":"Boss"}),
                "select_blind",
            ),
            (json!({"action":"skip_blind","blind":"Small"}), "skip_blind"),
            (
                json!({"action":"ui_click","ui_id":"cash_out_button"}),
                "ui_click",
            ),
            (
                json!({"action":"buy_card","area":"shop_jokers","card_index":1}),
                "buy",
            ),
            (json!({"action":"use_consumable","card_index":1}), "use"),
            (
                json!({"action":"sell_card","area":"consumeables","card_index":1}),
                "sell",
            ),
            (
                json!({"action":"move_joker","from_index":1,"to_index":2}),
                "move_joker",
            ),
            (json!({"action":"reroll"}), "reroll_shop"),
            (json!({"action":"next_round"}), "next_round"),
            (json!({"action":"choose_pack","card_index":1}), "use"),
            (
                json!({"action":"safe_transition","transition":"cash_out"}),
                "cash_out",
            ),
            (json!({"action":"resume_run"}), "start_run"),
        ];
        for (action, expected) in cases {
            let command = super::policy_action_command(Some(&action), "id", "decision").unwrap();
            assert_eq!(command["action"], expected);
            assert_eq!(command["decision_id"], "decision");
        }
        assert!(super::policy_action_command(None, "id", "decision").is_err());
        assert!(
            super::policy_action_command(Some(&json!({"action":"unknown"})), "id", "d").is_err()
        );
    }
}
