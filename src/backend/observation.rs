use serde_json::Value;

/// Extract a compact section of the observation/policy state.
pub fn compact_observation(data: &Value, section: &str) -> Value {
    match section {
        "summary" => compact_summary(data),
        "hand" => compact_hand(data),
        "build" => compact_build(data),
        "blind" => compact_blind(data),
        "hand_values" => compact_hand_values(data),
        "all" => data.clone(),
        _ => data.clone(),
    }
}

fn compact_summary(data: &Value) -> Value {
    let game = data.get("game");
    let run = data.get("run");
    let areas = data.get("areas");
    let chips = match run.and_then(|r| r.get("chips")).and_then(|r| r.as_i64()) {
        Some(chips) => Some(chips),
        None => run
            .and_then(|r| r.get("blind"))
            .and_then(|b| b.get("chips_required"))
            .and_then(|c| c.as_i64()),
    };

    serde_json::json!({
        "state": game.and_then(|g| g.get("state")).and_then(|g| g.as_str()),
        "ante": run.and_then(|r| r.get("ante")).and_then(|r| r.as_i64()),
        "round": run.and_then(|r| r.get("round")).and_then(|r| r.as_i64()),
        "blind": run.and_then(|r| r.get("blind")).and_then(|r| r.get("name")).and_then(|r| r.as_str()),
        "chips": chips,
        "hands_left": run.and_then(|r| r.get("hands_left")).and_then(|r| r.as_i64()),
        "discards_left": run.and_then(|r| r.get("discards_left")).and_then(|r| r.as_i64()),
        "dollars": run.and_then(|r| r.get("dollars")).and_then(|r| r.as_i64()),
        "jokers": areas.and_then(|a| a.get("jokers")).and_then(|j| j.as_array()).map(|arr| {
            arr.iter().filter_map(|j| j.get("name").and_then(|n| n.as_str())).collect::<Vec<_>>()
        }),
    })
}

fn compact_hand(data: &Value) -> Value {
    let areas = data
        .get("areas")
        .and_then(|a| a.get("hand"))
        .and_then(|h| h.as_array());
    if let Some(cards) = areas {
        serde_json::json!({
            "cards": cards.iter().map(|card| {
                serde_json::json!({
                    "index": card.get("index"),
                    "instance_id": card.get("instance_id"),
                    "base": card.get("base"),
                    "edition": card.get("edition"),
                    "seal": card.get("seal"),
                    "enhancement": card.get("enhancement"),
                    "face_down": card.get("face_down"),
                    "suits": card.get("suits"),
                    "rank": card.get("rank").or_else(|| card.get("base").and_then(|b| b.get("rank"))),
                    "center": card.get("center"),
                })
            }).collect::<Vec<_>>()
        })
    } else {
        serde_json::json!({"cards": []})
    }
}

fn compact_build(data: &Value) -> Value {
    let areas = data.get("areas");
    serde_json::json!({
        "jokers": areas.and_then(|a| a.get("jokers")).and_then(|j| j.as_array()).map(|arr| {
            arr.iter().map(|j| {
                serde_json::json!({
                    "name": j.get("name"),
                    "edition": j.get("edition").and_then(|v| v.get("text")).or_else(|| j.get("edition")),
                    "seal": j.get("seal").and_then(|v| v.get("text")).or_else(|| j.get("seal")),
                    "enhancement": j.get("enhancement"),
                    "slot_order": j.get("slot_order"),
                    "ability": j.get("ability"),
                })
            }).collect::<Vec<_>>()
        }),
        "consumables": areas.and_then(|a| a.get("consumables")).and_then(|c| c.as_array()).map(|arr| {
            arr.iter().map(|c| {
                serde_json::json!({
                    "name": c.get("name"),
                    "type": c.get("type"),
                })
            }).collect::<Vec<_>>()
        }),
    })
}

fn compact_blind(data: &Value) -> Value {
    let run = data.get("run").and_then(|r| r.get("blind"));
    serde_json::json!({
        "name": run.and_then(|b| b.get("name")).and_then(|b| b.as_str()),
        "boss": run.and_then(|b| b.get("boss")).and_then(|b| b.as_str()),
        "chips_required": run.and_then(|b| b.get("chips_required")).and_then(|b| b.as_i64()),
        "effect": run.and_then(|b| b.get("effect")).and_then(|b| b.get("name")).and_then(|b| b.as_str()),
        "disabled": run.and_then(|b| b.get("disabled")).and_then(|b| b.as_str()),
    })
}

fn compact_hand_values(data: &Value) -> Value {
    data.get("poker_hands").cloned().unwrap_or(Value::Null)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use serde_json::json;

    fn full_observation() -> Value {
        json!({
            "game": {"state": "SELECTING_HAND"},
            "run": {"ante": 3, "round": 2, "blind": {"name": "Cerebral", "boss": "Cerebral", "chips_required": 2850, "effect": {"name": "No face cards"}, "disabled": null}, "hands_left": 3, "discards_left": 2, "dollars": 15, "most_played_poker_hand": "Flush"},
            "areas": {
                "hand": [
                    {"index": 0, "instance_id": "abc1", "base": {"rank": "7", "suit": "Hearts"}, "edition": null, "seal": null, "enhancement": null, "face_down": false, "suits": [{"key": "H", "symbol": "h", "color": "red"}], "center": {"rank": {"display": "7"}}}
                ],
                "jokers": [
                    {"name": "Joker", "edition": {"text": "Sealed"}, "seal": {"text": "Red"}, "enhancement": null, "slot_order": 0, "ability": {"x_mult": 2}}
                ],
                "consumables": [
                    {"name": "Tower", "type": "Voucher"}
                ]
            },
            "poker_hands": {"values": {"Flush": {"chips": 20, "mult": 4}}}
        })
    }

    #[test]
    fn test_compact_observation_all() {
        let obs = full_observation();
        let r = compact_observation(&obs, "all");
        assert_eq!(r, obs);
    }
    #[test]
    fn test_compact_observation_unknown_section() {
        let obs = full_observation();
        let r = compact_observation(&obs, "unknown");
        assert_eq!(r, obs);
    }
    #[test]
    fn test_compact_observation_summary() {
        let obs = full_observation();
        let r = compact_observation(&obs, "summary");
        assert_eq!(r["state"].as_str().unwrap(), "SELECTING_HAND");
        assert_eq!(r["ante"].as_i64().unwrap(), 3);
        assert_eq!(r["round"].as_i64().unwrap(), 2);
        assert_eq!(r["blind"].as_str().unwrap(), "Cerebral");
        assert_eq!(r["chips"].as_i64().unwrap(), 2850);
        assert_eq!(r["hands_left"].as_i64().unwrap(), 3);
        assert_eq!(r["discards_left"].as_i64().unwrap(), 2);
        assert_eq!(r["dollars"].as_i64().unwrap(), 15);
        assert_eq!(
            r["jokers"].as_array().unwrap()[0].as_str().unwrap(),
            "Joker"
        );
    }
    #[test]
    fn test_compact_observation_summary_missing() {
        let obs = json!({});
        let r = compact_observation(&obs, "summary");
        assert_eq!(r["state"].as_str(), None);
        assert_eq!(r["ante"].as_i64(), None);
    }
    #[test]
    fn test_compact_observation_summary_uses_blind_chip_fallback() {
        let r = compact_observation(&json!({"run":{"blind":{"chips_required":99}}}), "summary");
        assert_eq!(r["chips"], 99);
        let r = compact_observation(
            &json!({"run":{"chips":123,"blind":{"chips_required":99}}}),
            "summary",
        );
        assert_eq!(r["chips"], 123);
    }
    #[test]
    fn test_compact_observation_hand() {
        let obs = full_observation();
        let r = compact_observation(&obs, "hand");
        assert_eq!(r["cards"].as_array().unwrap().len(), 1);
        assert_eq!(r["cards"][0]["index"].as_i64().unwrap(), 0);
        assert_eq!(r["cards"][0]["instance_id"].as_str().unwrap(), "abc1");
        assert_eq!(r["cards"][0]["rank"].as_str().unwrap(), "7");
    }
    #[test]
    fn test_compact_observation_hand_empty() {
        let obs = json!({"areas": {"hand": []}});
        let r = compact_observation(&obs, "hand");
        assert_eq!(r["cards"].as_array().unwrap().len(), 0);
    }
    #[test]
    fn test_compact_observation_hand_missing() {
        let obs = json!({});
        let r = compact_observation(&obs, "hand");
        assert_eq!(r["cards"].as_array().unwrap().len(), 0);
    }
    #[test]
    fn test_compact_observation_build() {
        let obs = full_observation();
        let r = compact_observation(&obs, "build");
        assert_eq!(
            r["jokers"].as_array().unwrap()[0]["name"].as_str().unwrap(),
            "Joker"
        );
        assert_eq!(r["jokers"][0]["edition"].as_str().unwrap(), "Sealed");
        assert_eq!(r["jokers"][0]["seal"].as_str().unwrap(), "Red");
        assert_eq!(r["consumables"][0]["name"].as_str().unwrap(), "Tower");
        assert_eq!(r["consumables"][0]["type"].as_str().unwrap(), "Voucher");
        let primitive = compact_observation(
            &json!({"areas":{"jokers":[{"name":"J","edition":"Foil","seal":"Blue"}]}}),
            "build",
        );
        assert_eq!(primitive["jokers"][0]["edition"], "Foil");
        assert_eq!(primitive["jokers"][0]["seal"], "Blue");
    }
    #[test]
    fn test_compact_observation_blind() {
        let obs = full_observation();
        let r = compact_observation(&obs, "blind");
        assert_eq!(r["name"].as_str().unwrap(), "Cerebral");
        assert_eq!(r["boss"].as_str().unwrap(), "Cerebral");
        assert_eq!(r["chips_required"].as_i64().unwrap(), 2850);
        assert_eq!(r["effect"].as_str().unwrap(), "No face cards");
    }
    #[test]
    fn test_compact_observation_blind_missing() {
        let obs = json!({"run": {}});
        let r = compact_observation(&obs, "blind");
        assert_eq!(r["name"].as_str(), None);
        assert_eq!(r["boss"].as_str(), None);
        assert_eq!(r["chips_required"].as_i64(), None);
    }
    #[test]
    fn test_compact_observation_hand_values() {
        let obs = full_observation();
        let r = compact_observation(&obs, "hand_values");
        assert!(r.is_object());
        assert_eq!(r["values"]["Flush"]["chips"].as_i64().unwrap(), 20);
        assert_eq!(r["values"]["Flush"]["mult"].as_i64().unwrap(), 4);
    }
    #[test]
    fn test_compact_observation_hand_values_missing() {
        let obs = json!({});
        let r = compact_observation(&obs, "hand_values");
        assert_eq!(r, Value::Null);
    }
    #[test]
    fn test_compact_observation_face_down_card() {
        let obs = json!({
            "areas": {"hand": [{"index": 0, "instance_id": "x", "base": {"rank": "?"}, "face_down": true, "suits": [], "enhancement": null, "seal": null, "edition": null, "center": {"rank": {"display": "?"}}}]}
        });
        let r = compact_observation(&obs, "hand");
        assert_eq!(r["cards"][0]["face_down"].as_bool().unwrap(), true);
        assert_eq!(r["cards"][0]["rank"].as_str().unwrap(), "?");
    }
}
