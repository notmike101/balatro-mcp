use schemars::JsonSchema;
use serde::Deserialize;

pub const SEED: &str = "2K9H9HN";
pub const INFO_TYPES: &[&str] = &[
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

#[derive(Deserialize, JsonSchema)]
pub struct ObserveParams {
    #[serde(default = "summary")]
    pub section: String,
}
pub fn summary() -> String {
    "summary".into()
}

#[derive(Deserialize, JsonSchema)]
pub struct DecisionParams {
    #[serde(default)]
    pub action_type: String,
    #[serde(default = "decision_limit")]
    pub limit: u32,
}
pub fn decision_limit() -> u32 {
    40
}

#[derive(Deserialize, JsonSchema)]
pub struct ActionParams {
    pub action_id: String,
    pub decision_id: String,
    #[serde(default = "settle_timeout")]
    pub settle_timeout: f64,
}
pub fn settle_timeout() -> f64 {
    12.0
}

#[derive(Deserialize, JsonSchema)]
pub struct AdvanceParams {
    #[serde(default = "advance_steps")]
    pub max_steps: u32,
}
pub fn advance_steps() -> u32 {
    8
}

#[derive(Deserialize, JsonSchema)]
pub struct WaitParams {
    #[serde(default)]
    pub state: String,
    #[serde(default = "wait_timeout")]
    pub timeout: f64,
}
pub fn wait_timeout() -> f64 {
    10.0
}

#[derive(Deserialize, JsonSchema)]
pub struct CheckpointParams {
    #[serde(default = "checkpoint_kind")]
    pub kind: String,
}
pub fn checkpoint_kind() -> String {
    "mcp".into()
}

#[derive(Deserialize, JsonSchema)]
pub struct LookupParams {
    #[serde(rename = "type")]
    pub entity_type: String,
    pub name: String,
    #[serde(default)]
    pub suit: String,
    #[serde(default)]
    pub edition: String,
    #[serde(default)]
    pub enhancement: String,
    #[serde(default)]
    pub seal: String,
    #[serde(default)]
    pub stickers: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListParams {
    #[serde(default)]
    pub entity_type: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct TopicParams {
    #[serde(default = "core")]
    pub topic: String,
}
pub fn core() -> String {
    "core".into()
}

#[derive(Deserialize, JsonSchema)]
pub struct ReplayQueryParams {
    pub ante: i64,
    pub stake: i64,
    pub blind: String,
    #[serde(default = "best")]
    pub outcome: String,
}
pub fn best() -> String {
    "best".into()
}

#[derive(Deserialize, JsonSchema)]
pub struct ReplayLogParams {
    pub outcome: String,
    pub ante: i64,
    pub stake: i64,
    pub blind_key: String,
    #[serde(default)]
    pub jokers: Vec<String>,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub dollars_start: Option<i64>,
    #[serde(default)]
    pub dollars_end: Option<i64>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct RuntimeParams {
    #[serde(default = "log_lines")]
    pub lines: u32,
}
pub fn log_lines() -> u32 {
    120
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- SEED tests ----

    #[test]
    fn seed_is_correct() {
        assert_eq!(SEED, "2K9H9HN");
    }

    // ---- INFO_TYPES tests ----

    #[test]
    fn info_types_contains_all_expected() {
        assert!(INFO_TYPES.contains(&"joker"));
        assert!(INFO_TYPES.contains(&"tarot"));
        assert!(INFO_TYPES.contains(&"spectral"));
        assert!(INFO_TYPES.contains(&"planet"));
        assert!(INFO_TYPES.contains(&"voucher"));
        assert!(INFO_TYPES.contains(&"deck"));
        assert!(INFO_TYPES.contains(&"blind"));
        assert!(INFO_TYPES.contains(&"tag"));
        assert!(INFO_TYPES.contains(&"seal"));
        assert!(INFO_TYPES.contains(&"edition"));
        assert!(INFO_TYPES.contains(&"enhancement"));
        assert!(INFO_TYPES.contains(&"sticker"));
        assert!(INFO_TYPES.contains(&"stake"));
        assert!(INFO_TYPES.contains(&"card"));
        assert!(INFO_TYPES.contains(&"playing-card"));
    }

    #[test]
    fn info_types_has_correct_count() {
        assert_eq!(INFO_TYPES.len(), 15);
    }

    // ---- Parameter default functions ----

    #[test]
    fn summary_default_returns_summary() {
        assert_eq!(summary(), "summary");
    }

    #[test]
    fn decision_limit_default_returns_40() {
        assert_eq!(decision_limit(), 40);
    }

    #[test]
    fn settle_timeout_default_returns_12() {
        assert_eq!(settle_timeout(), 12.0);
    }

    #[test]
    fn advance_steps_default_returns_8() {
        assert_eq!(advance_steps(), 8);
    }

    #[test]
    fn wait_timeout_default_returns_10() {
        assert_eq!(wait_timeout(), 10.0);
    }

    #[test]
    fn checkpoint_kind_default_returns_mcp() {
        assert_eq!(checkpoint_kind(), "mcp");
    }

    #[test]
    fn core_default_returns_core() {
        assert_eq!(core(), "core");
    }

    #[test]
    fn best_default_returns_best() {
        assert_eq!(best(), "best");
    }

    #[test]
    fn log_lines_default_returns_120() {
        assert_eq!(log_lines(), 120);
    }

    // ---- Parameter deserialization with defaults ----

    #[test]
    fn observe_params_defaults_to_summary() {
        let json = json!({});
        let params: ObserveParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.section, "summary");
    }

    #[test]
    fn observe_params_accepts_custom_section() {
        let json = json!({"section": "hand"});
        let params: ObserveParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.section, "hand");
    }

    #[test]
    fn decision_params_defaults() {
        let json = json!({});
        let params: DecisionParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action_type, "");
        assert_eq!(params.limit, 40);
    }

    #[test]
    fn decision_params_accepts_custom_values() {
        let json = json!({"action_type": "play", "limit": 100});
        let params: DecisionParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action_type, "play");
        assert_eq!(params.limit, 100);
    }

    #[test]
    fn action_params_defaults_settle_timeout() {
        let json = json!({"action_id": "p:1", "decision_id": "d:1"});
        let params: ActionParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.settle_timeout, 12.0);
    }

    #[test]
    fn action_params_accepts_custom_settle_timeout() {
        let json = json!({"action_id": "p:1", "decision_id": "d:1", "settle_timeout": 25.5});
        let params: ActionParams = serde_json::from_value(json).unwrap();
        assert!((params.settle_timeout - 25.5).abs() < f64::EPSILON);
    }

    #[test]
    fn advance_params_defaults_to_8() {
        let json = json!({});
        let params: AdvanceParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.max_steps, 8);
    }

    #[test]
    fn wait_params_defaults() {
        let json = json!({});
        let params: WaitParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.state, "");
        assert!((params.timeout - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn checkpoint_params_defaults_to_mcp() {
        let json = json!({});
        let params: CheckpointParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.kind, "mcp");
    }

    #[test]
    fn lookup_params_defaults_empty_fields() {
        let json = json!({"type": "joker", "name": "Jolly"});
        let params: LookupParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.entity_type, "joker");
        assert_eq!(params.name, "Jolly");
        assert_eq!(params.suit, "");
        assert_eq!(params.edition, "");
        assert_eq!(params.enhancement, "");
        assert_eq!(params.seal, "");
        assert!(params.stickers.is_empty());
    }

    #[test]
    fn lookup_params_accepts_stickers() {
        let json = json!({"type": "joker", "name": "Jolly", "stickers": ["Glowing", "Shiny"]});
        let params: LookupParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.stickers, vec!["Glowing", "Shiny"]);
    }

    #[test]
    fn list_params_defaults_empty_entity_type() {
        let json = json!({});
        let params: ListParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.entity_type, "");
    }

    #[test]
    fn topic_params_defaults_to_core() {
        let json = json!({});
        let params: TopicParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.topic, "core");
    }

    #[test]
    fn replay_query_params_defaults_to_best() {
        let json = json!({"ante": 3, "stake": 2, "blind": "Ox"});
        let params: ReplayQueryParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.outcome, "best");
    }

    #[test]
    fn replay_query_params_accepts_fail_outcome() {
        let json = json!({"ante": 1, "stake": 0, "blind": "Maze", "outcome": "fail"});
        let params: ReplayQueryParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.outcome, "fail");
    }

    #[test]
    fn replay_log_params_defaults() {
        let json = json!({"outcome": "clear", "ante": 5, "stake": 3, "blind_key": "Big_Ox"});
        let params: ReplayLogParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.outcome, "clear");
        assert!(params.jokers.is_empty());
        assert!(params.steps.is_empty());
        assert!(params.dollars_start.is_none());
        assert!(params.dollars_end.is_none());
        assert_eq!(params.notes, "");
    }

    #[test]
    fn replay_log_params_accepts_jokers_and_steps() {
        let json = json!({
            "outcome": "clear", "ante": 1, "stake": 0, "blind_key": "Small_Cruel",
            "jokers": ["Joker", "Wolf"],
            "steps": ["play:1", "draw:2"],
            "dollars_start": 15,
            "dollars_end": 20,
            "notes": "Easy blind"
        });
        let params: ReplayLogParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.jokers, vec!["Joker", "Wolf"]);
        assert_eq!(params.steps, vec!["play:1", "draw:2"]);
        assert_eq!(params.dollars_start, Some(15));
        assert_eq!(params.dollars_end, Some(20));
        assert_eq!(params.notes, "Easy blind");
    }

    #[test]
    fn runtime_params_defaults_to_120() {
        let json = json!({});
        let params: RuntimeParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.lines, 120);
    }

    #[test]
    fn runtime_params_accepts_custom_lines() {
        let json = json!({"lines": 50});
        let params: RuntimeParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.lines, 50);
    }

    // ---- JsonSchema derive compiles ----

    #[test]
    fn observe_params_schema_compiles() {
        let _ = schemars::schema_for!(ObserveParams);
    }

    #[test]
    fn decision_params_schema_compiles() {
        let _ = schemars::schema_for!(DecisionParams);
    }
}
