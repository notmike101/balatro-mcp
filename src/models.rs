use schemars::JsonSchema;
use serde::Deserialize;

fn object_value_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    // `serde_json::Value` normally emits the boolean schema `true`, which is
    // valid JSON Schema but rejected by some MCP hosts for tool properties.
    schemars::json_schema!({"type": "object"})
}

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
    /// Maximum number of legal actions returned in this page.
    #[serde(default = "decision_action_limit")]
    pub action_limit: u32,
    /// Zero-based offset into the filtered legal-action list.
    #[serde(default)]
    pub action_offset: u32,
}
pub fn decision_limit() -> u32 {
    40
}
pub fn decision_action_limit() -> u32 {
    80
}

#[derive(Deserialize, JsonSchema)]
pub struct ActionParams {
    pub action_id: String,
    pub decision_id: String,
    #[serde(default = "settle_timeout")]
    pub settle_timeout: f64,
    /// Optional 1-based hand positions for play_selected/discard_selected.
    #[serde(default)]
    pub card_indices: Vec<usize>,
}
pub fn settle_timeout() -> f64 {
    12.0
}

#[derive(Deserialize, JsonSchema)]
pub struct StartNewRunParams {
    /// Required when an existing saved run would be replaced.
    #[serde(default)]
    pub confirm_override: bool,
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
    #[serde(default = "default_ante")]
    pub ante: i64,
    #[serde(default = "default_stake")]
    pub stake: i64,
    #[serde(default)]
    pub blind: String,
    #[serde(default = "best")]
    pub outcome: String,
}
pub fn default_ante() -> i64 {
    1
}
pub fn default_stake() -> i64 {
    1
}
pub fn best() -> String {
    "best".into()
}

#[derive(Deserialize, JsonSchema)]
pub struct ReplayLogParams {
    #[serde(default = "default_replay_outcome")]
    pub outcome: String,
    #[serde(default = "default_ante")]
    pub ante: i64,
    #[serde(default = "default_stake")]
    pub stake: i64,
    #[serde(default = "default_blind_key")]
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
pub fn default_replay_outcome() -> String {
    "fail".into()
}
pub fn default_blind_key() -> String {
    "unknown".into()
}

#[derive(Deserialize, JsonSchema)]
pub struct RuntimeParams {
    #[serde(default = "log_lines")]
    pub lines: u32,
}
pub fn log_lines() -> u32 {
    120
}

#[derive(Deserialize, JsonSchema)]
pub struct ScoreParams {
    /// Optional 1-based hand positions. Empty uses the live highlighted selection, or the full hand.
    #[serde(default)]
    pub card_indices: Vec<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct StateParams {
    #[serde(default = "checkpoint_kind")]
    pub kind: String,
    #[serde(default = "log_lines")]
    pub limit: u32,
}

#[derive(Deserialize, JsonSchema)]
pub struct StrategyRuleParams {
    pub id: String,
    pub kind: String,
    #[schemars(schema_with = "object_value_schema")]
    pub conditions: serde_json::Value,
    pub directive: String,
    #[serde(default)]
    pub absolute: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct StrategyEvidenceParams {
    pub rule_id: String,
    pub outcome: String,
    pub event_id: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct LessonParams {
    pub category: String,
    pub lesson: String,
    #[serde(default)]
    pub source: String,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
}

pub fn default_confidence() -> f64 {
    0.5
}

#[derive(Deserialize, JsonSchema)]
pub struct EstimateParams {
    pub hand_type: String,
    pub estimated: i64,
    pub actual: i64,
    #[serde(default)]
    #[schemars(schema_with = "object_value_schema")]
    pub context: serde_json::Value,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
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
        assert_eq!(params.action_limit, 80);
        assert_eq!(params.action_offset, 0);
    }

    #[test]
    fn decision_params_accepts_custom_values() {
        let json =
            json!({"action_type": "play", "limit": 100, "action_limit": 12, "action_offset": 24});
        let params: DecisionParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action_type, "play");
        assert_eq!(params.limit, 100);
        assert_eq!(params.action_limit, 12);
        assert_eq!(params.action_offset, 24);
    }

    #[test]
    fn action_params_defaults_settle_timeout() {
        let json = json!({"action_id": "p:1", "decision_id": "d:1"});
        let params: ActionParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.settle_timeout, 12.0);
        assert!(params.card_indices.is_empty());
    }

    #[test]
    fn action_params_accepts_custom_settle_timeout() {
        let json = json!({"action_id": "p:1", "decision_id": "d:1", "settle_timeout": 25.5, "card_indices": [4, 5]});
        let params: ActionParams = serde_json::from_value(json).unwrap();
        assert!((params.settle_timeout - 25.5).abs() < f64::EPSILON);
        assert_eq!(params.card_indices, vec![4, 5]);
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

    #[test]
    fn arbitrary_json_tool_fields_use_object_schemas() {
        let strategy = serde_json::to_value(schemars::schema_for!(StrategyRuleParams)).unwrap();
        let estimate = serde_json::to_value(schemars::schema_for!(EstimateParams)).unwrap();

        assert_eq!(strategy["properties"]["conditions"]["type"], "object");
        assert_eq!(estimate["properties"]["context"]["type"], "object");
    }
}
