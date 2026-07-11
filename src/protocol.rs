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

    // ---- value_state tests ----

    #[test]
    fn value_state_from_game_state() {
        let data = json!({"game": {"state": "SHOP"}});
        let result = value_state(&data);
        assert!(result.is_some());
        let s = result.unwrap().as_str().unwrap().to_string();
        assert_eq!(s, "SHOP");
    }

    #[test]
    fn value_state_from_root_state() {
        let data = json!({"state": "BLIND"});
        let result = value_state(&data);
        assert!(result.is_some());
        let s = result.unwrap().as_str().unwrap().to_string();
        assert_eq!(s, "BLIND");
    }

    #[test]
    fn value_state_prefers_game_state_over_root() {
        let data = json!({"game": {"state": "SHOP"}, "state": "BLIND"});
        let result = value_state(&data);
        assert!(result.is_some());
        let s = result.unwrap().as_str().unwrap().to_string();
        assert_eq!(s, "SHOP");
    }

    #[test]
    fn value_state_returns_none_when_missing() {
        let data = json!({"foo": "bar"});
        assert!(value_state(&data).is_none());
    }

    // ---- sanitize tests ----

    #[test]
    fn sanitize_removes_command_field() {
        let input = json!({"command": {"action": "play"}, "data": 1});
        let result = sanitize(input);
        assert!(result.get("command").is_none());
        assert_eq!(result["data"], 1);
    }

    #[test]
    fn sanitize_removes_raw_command_field() {
        let input = json!({"raw_command": "raw stuff", "data": 2});
        let result = sanitize(input);
        assert!(result.get("raw_command").is_none());
        assert_eq!(result["data"], 2);
    }

    #[test]
    fn sanitize_hides_face_down_card() {
        let input = json!({"index": 0, "instance_id": 42, "face_down": true, "rank": "King"});
        let result = sanitize(input);
        assert_eq!(result["index"], 0);
        assert_eq!(result["instance_id"], 42);
        assert_eq!(result["hidden"], true);
        assert!(result.get("rank").is_none());
    }

    #[test]
    fn sanitize_hides_back_facing_card() {
        let input = json!({"index": 1, "instance_id": 99, "facing": "back"});
        let result = sanitize(input);
        assert_eq!(result["hidden"], true);
        assert!(result.get("facing").is_none());
    }

    #[test]
    fn sanitize_leaves_visible_card_untouched() {
        let input = json!({"index": 3, "rank": "7", "suit": "Hearts"});
        let result = sanitize(input);
        assert_eq!(result["index"], 3);
        assert_eq!(result["rank"], "7");
        assert_eq!(result["suit"], "Hearts");
    }

    #[test]
    fn sanitize_recurses_through_nested_objects() {
        let input = json!({
            "outer": {
                "command": "secret",
                "inner": {
                    "face_down": true,
                    "rank": "Ace"
                }
            }
        });
        let result = sanitize(input);
        assert!(result["outer"]["inner"].get("command").is_none());
        assert_eq!(result["outer"]["inner"]["hidden"], true);
        assert!(result["outer"]["inner"].get("rank").is_none());
    }

    #[test]
    fn sanitize_recurses_through_arrays() {
        let input = json!({
            "cards": [
                {"index": 0, "face_down": true, "rank": "X"},
                {"index": 1, "rank": "7", "suit": "D"}
            ]
        });
        let result = sanitize(input);
        assert_eq!(result["cards"][0]["hidden"], true);
        assert!(result["cards"][0].get("rank").is_none());
        assert_eq!(result["cards"][1]["rank"], "7");
        assert_eq!(result["cards"][1]["suit"], "D");
    }

    #[test]
    fn sanitize_leaves_primitives_untouched() {
        assert_eq!(sanitize(json!(42)), json!(42));
        assert_eq!(sanitize(json!("hello")), json!("hello"));
        assert_eq!(sanitize(json!(true)), json!(true));
        assert_eq!(sanitize(json!(null)), json!(null));
    }

    // ---- envelope tests ----

    #[test]
    fn envelope_ok_contains_expected_keys() {
        let result = envelope(true, json!({}), "", "");
        assert_eq!(result["ok"], true);
        assert!(result.get("state").is_some());
        assert!(result.get("decision_id").is_some());
        assert!(result.get("legal_actions").is_some());
        assert!(result.get("data").is_some());
        assert!(!result.get("error").is_some());
    }

    #[test]
    fn envelope_error_contains_error_field() {
        let result = envelope(false, json!({}), "badThing", "something went wrong");
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["code"], "badThing");
        assert_eq!(result["error"]["message"], "something went wrong");
    }

    #[test]
    fn envelope_extracts_state_from_data() {
        let data = json!({"game": {"state": "SHOP"}, "decision_id": "abc"});
        let result = envelope(true, data, "", "");
        assert_eq!(result["state"].as_str().unwrap(), "SHOP");
        assert_eq!(result["decision_id"], "abc");
    }

    #[test]
    fn envelope_defaults_state_to_null_when_missing() {
        let result = envelope(true, json!({}), "", "");
        assert_eq!(result["state"], json!(null));
    }

    #[test]
    fn envelope_defaults_legal_actions_to_empty_array() {
        let result = envelope(true, json!({}), "", "");
        assert_eq!(result["legal_actions"], json!([]));
    }

    #[test]
    fn envelope_preserves_data_legal_actions() {
        let data = json!({"legal_actions": [{"id": "play:1"}]});
        let result = envelope(true, data, "", "");
        assert_eq!(result["legal_actions"][0]["id"], "play:1");
    }

    #[test]
    fn envelope_sanitizes_data_before_wrapping() {
        let data = json!({"command": "secret", "data": "visible"});
        let result = envelope(true, data, "", "");
        assert!(result["data"]["command"].is_null());
        assert_eq!(result["data"]["data"], "visible");
    }

    // ---- tool wrapper tests ----

    #[test]
    fn tool_wraps_ok_as_structured_success() {
        let value = envelope(true, json!({}), "", "");
        let result = tool(value).unwrap();
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn tool_wraps_error_as_structured_error() {
        let value = envelope(false, json!({}), "fail", "reason");
        let result = tool(value).unwrap();
        assert_eq!(result.is_error, Some(true));
    }

    // ---- compact_observation tests ----

    #[test]
    fn compact_observation_all_returns_data_unchanged() {
        let data = json!({"game": {}, "run": {}, "blind": {}});
        let result = compact_observation(data.clone(), "all");
        assert_eq!(result, data);
    }

    #[test]
    fn compact_observation_hand_section() {
        let data = json!({
            "game": {"name": "g"}, "run": {"name": "r"},
            "areas": {"hand": [{"id": 1}]},
            "decision_id": "d1", "legal_actions": [1]
        });
        let result = compact_observation(data, "hand");
        assert_eq!(result["game"]["name"], "g");
        assert_eq!(result["run"]["name"], "r");
        assert_eq!(result["hand"][0]["id"], 1);
        assert_eq!(result["decision_id"], "d1");
        assert_eq!(result["legal_actions"][0], 1);
    }

    #[test]
    fn compact_observation_build_section() {
        let data = json!({
            "game": {}, "run": {},
            "areas": {"jokers": [{"id": 1}], "consumeables": [{"id": 2}]},
            "decision_id": "d2", "legal_actions": []
        });
        let result = compact_observation(data, "build");
        assert_eq!(result["jokers"][0]["id"], 1);
        assert_eq!(result["consumables"][0]["id"], 2);
    }

    #[test]
    fn compact_observation_blind_section() {
        let data = json!({
            "game": {}, "run": {},
            "blind": {"name": "Cruel"},
            "active_directives": [{"id": 3}],
            "decision_id": "d3", "legal_actions": []
        });
        let result = compact_observation(data, "blind");
        assert_eq!(result["blind"]["name"], "Cruel");
        assert_eq!(result["active_directives"][0]["id"], 3);
    }

    #[test]
    fn compact_observation_hand_values_section() {
        let data = json!({
            "game": {}, "run": {},
            "poker_hand_values": {"High Card": 5},
            "decision_id": "d4", "legal_actions": []
        });
        let result = compact_observation(data, "hand_values");
        assert_eq!(result["poker_hand_values"]["High Card"], 5);
    }

    #[test]
    fn compact_observation_summary_fallback_section() {
        let data = json!({
            "game": {}, "run": {},
            "blind": {"name": "Stung"},
            "ready": true,
            "estimate_quality": 0.85,
            "decision_id": "d5", "legal_actions": []
        });
        let result = compact_observation(data, "summary");
        assert_eq!(result["blind"]["name"], "Stung");
        assert_eq!(result["ready"], true);
        assert_eq!(result["estimate_quality"], 0.85);
    }

    #[test]
    fn compact_observation_missing_keys_default_to_null() {
        let data = json!({});
        let result = compact_observation(data, "hand");
        assert_eq!(result["game"], json!(null));
        assert_eq!(result["hand"], json!(null));
        assert_eq!(result["decision_id"], json!(null));
        assert_eq!(result["legal_actions"], json!(null));
    }}










