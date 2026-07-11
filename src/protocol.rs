use serde_json::{Map, Value, json};

use rmcp::model::CallToolResult;

pub fn value_state(data: &Value) -> Option<Value> {
    data.pointer("/game/state")
        .cloned()
        .or_else(|| data.get("state").cloned())
}

pub fn sanitize(value: Value) -> Value {
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

pub fn envelope(ok: bool, data: Value, code: &str, message: &str) -> Value {
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

pub fn tool(value: Value) -> Result<CallToolResult, rmcp::ErrorData> {
    let error = value.get("ok") == Some(&Value::Bool(false));
    Ok(if error {
        CallToolResult::structured_error(value)
    } else {
        CallToolResult::structured(value)
    })
}

pub fn compact_observation(data: Value, section: &str) -> Value {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
