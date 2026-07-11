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
