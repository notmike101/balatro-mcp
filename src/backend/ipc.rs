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
    play_limit: u32,
    discard_limit: u32,
    target_limit: u32,
    settle_timeout: f64,
) -> Result<Value, String> {
    let command = serde_json::json!({
        "action": "policy_step",
        "action_id": action_id,
        "decision_id": decision_id,
        "play_limit": play_limit,
        "discard_limit": discard_limit,
        "target_limit": target_limit,
        "settle_timeout": settle_timeout,
    });
    paths.write_command(&command)?;
    let response = paths
        .wait_for_response("policy_step", 60.0)?
        .ok_or("no response from bridge for policy_step")?;
    Ok(response)
}

/// Execute a safe transition action (skip_tutorial, cash_out, etc.) via the Lua bridge.
pub fn advance_safe_internal(paths: &IpcPaths, action: &str, steps: u32) -> Result<Value, String> {
    let command = serde_json::json!({
        "action": "safe_transition",
        "transition": action,
        "max_steps": steps,
    });
    paths.write_command(&command)?;
    let response = paths
        .wait_for_response("safe_transition", 60.0)?
        .ok_or("no response from bridge for safe_transition")?;
    Ok(response)
}

/// Persist the current observation into a checkpoint file.
pub fn checkpoint_internal(paths: &IpcPaths, kind: &str) -> Result<Value, String> {
    let command = serde_json::json!({
        "action": "checkpoint",
        "kind": kind,
    });
    paths.write_command(&command)?;
    let response = paths
        .wait_for_response("checkpoint", 30.0)?
        .ok_or("no response from bridge for checkpoint")?;
    Ok(response)
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
        let cmd = serde_json::json!({
            "action": "safe_transition",
            "transition": *action,
            "max_steps": steps,
        });
        paths.write_command(&cmd)?;
        let response = paths.wait_for_response(*action, 30.0)?;
        if response.is_some() {
            return Ok(response.unwrap_or(Value::Null));
        }
    }
    Err("no safe transition succeeded".into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn lua_values_are_encoded_safely() {
        assert_eq!(super::to_lua_value(&json!(null)), "nil");
        assert_eq!(super::to_lua_value(&json!(true)), "true");
        assert_eq!(
            super::to_lua_value(&json!({"x": "a\"b"})),
            "{[\"x\"] = \"a\\\"b\"}"
        );
    }

    #[test]
    fn command_payload_has_expected_action_shape() {
        let value = json!({"action": "checkpoint", "kind": "test"});
        let encoded = super::to_lua_command(&value);
        assert!(encoded.contains("checkpoint"));
        assert!(encoded.contains("test"));
    }
}
