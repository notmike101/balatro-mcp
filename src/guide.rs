pub const GUIDE_TOPICS: &[&str] = &[
    "core",
    "hands",
    "actions",
    "economy",
    "blinds",
    "jokers",
    "cards",
    "consumables",
];

pub fn guide(topic: &str) -> Option<&'static str> {
    match topic.to_ascii_lowercase().as_str() {
        "core" | "rules" | "ante8" => Some(
            "Clear blinds through Ante 8. Use game_status -> get_decision -> take_action. The default decision is compact; call decision_context for recall or strategy only when needed. Provide why_this_action for strategic actions, use exact 1-based indices, refresh on stale_decision, and never infer hidden cards.",
        ),
        "hands" | "scoring" => Some(
            "Poker hands score Chips multiplied by Mult. Use hand_values for the live contract and score_hand for explicit scoring. play_selected/discard_selected and score_hand use 1-based hand positions; select only the cards needed. Hidden or incomplete card identity means the hand and score are unknown.",
        ),
        "actions" | "discards" => Some(
            "Hands score; discards redraw. Check discard_status before discarding, choose a specific draw goal, and use only legal actions. ROUND_EVAL uses proceed_round; SHOP uses next_round.",
        ),
        "economy" | "shops" => Some(
            "Evaluate Joker slots, consumable slots, price, interest, and the next blind. Buy one item at a time, then reread decision state.",
        ),
        "blinds" | "bosses" | "debuffs" => Some(
            "Boss blinds impose special rules and debuffs. When one matters, call decision_context(section=checks), lookup_rule, and inspect the live score margin before acting.",
        ),
        "jokers" | "editions" => Some(
            "Jokers affect Chips, Mult, economy, and trigger order. Foil, Holographic, and Polychrome editions change scoring. Call decision_context(section=checks) when ordering matters.",
        ),
        "cards" | "enhancements" | "seals" => Some(
            "Playing cards may have one enhancement, edition, and seal. Face-down or incompletely populated card identity is always unknown; use only explicit rank/value data from a fresh observation and never infer identity from nominal values, suits, positions, instance ids, or prior observations.",
        ),
        "consumables" | "vouchers" | "stakes" | "decks" | "tags" | "progression" => Some(
            "Decks, Stakes, Vouchers, Tags, and consumables change run rules. Check owned consumables when a strategic choice is uncertain, verify targets and score pressure, and look up unfamiliar effects before acting.",
        ),
        _ => None,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn guide_topics_contains_all_expected() {
        assert!(GUIDE_TOPICS.contains(&"core"));
        assert!(GUIDE_TOPICS.contains(&"hands"));
        assert!(GUIDE_TOPICS.contains(&"actions"));
        assert!(GUIDE_TOPICS.contains(&"economy"));
        assert!(GUIDE_TOPICS.contains(&"blinds"));
        assert!(GUIDE_TOPICS.contains(&"jokers"));
        assert!(GUIDE_TOPICS.contains(&"cards"));
        assert!(GUIDE_TOPICS.contains(&"consumables"));
        assert_eq!(GUIDE_TOPICS.len(), 8);
    }

    #[test]
    fn guide_returns_text_for_core() {
        let text = guide("core").unwrap();
        assert!(text.contains("Ante 8"));
        assert!(text.contains("game_status"));
        assert!(text.len() < 500);
    }

    #[test]
    fn guide_aliases_for_core() {
        assert!(guide("rules").is_some());
        assert!(guide("ante8").is_some());
    }

    #[test]
    fn guide_returns_text_for_hands() {
        let text = guide("hands").unwrap();
        assert!(text.contains("Chips"));
        assert!(text.contains("Mult"));
    }

    #[test]
    fn guide_aliases_for_hands() {
        assert!(guide("scoring").is_some());
    }

    #[test]
    fn guide_returns_text_for_actions() {
        let text = guide("actions").unwrap();
        assert!(text.contains("discards"));
    }

    #[test]
    fn guide_aliases_for_actions() {
        assert!(guide("discards").is_some());
    }

    #[test]
    fn guide_returns_text_for_economy() {
        let text = guide("economy").unwrap();
        assert!(text.contains("Joker"));
        assert!(text.contains("interest"));
    }

    #[test]
    fn guide_aliases_for_economy() {
        assert!(guide("shops").is_some());
    }

    #[test]
    fn guide_returns_text_for_blinds() {
        let text = guide("blinds").unwrap();
        assert!(text.contains("Boss"));
        assert!(text.contains("debuffs"));
    }

    #[test]
    fn guide_aliases_for_blinds() {
        assert!(guide("bosses").is_some());
        assert!(guide("debuffs").is_some());
    }

    #[test]
    fn guide_returns_text_for_jokers() {
        let text = guide("jokers").unwrap();
        assert!(text.contains("Foil"));
        assert!(text.contains("Holographic"));
        assert!(text.contains("Polychrome"));
    }

    #[test]
    fn guide_aliases_for_jokers() {
        assert!(guide("editions").is_some());
    }

    #[test]
    fn guide_returns_text_for_cards() {
        let text = guide("cards").unwrap();
        assert!(text.contains("enhancement"));
        assert!(text.contains("Face-down"));
    }

    #[test]
    fn guide_aliases_for_cards() {
        assert!(guide("enhancements").is_some());
        assert!(guide("seals").is_some());
    }

    #[test]
    fn guide_returns_text_for_consumables() {
        let text = guide("consumables").unwrap();
        assert!(text.contains("Decks"));
        assert!(text.contains("Stakes"));
        assert!(text.contains("Vouchers"));
    }

    #[test]
    fn guide_aliases_for_consumables() {
        assert!(guide("vouchers").is_some());
        assert!(guide("stakes").is_some());
        assert!(guide("decks").is_some());
        assert!(guide("tags").is_some());
        assert!(guide("progression").is_some());
    }

    #[test]
    fn guide_returns_none_for_unknown_topic() {
        assert!(guide("nonexistent").is_none());
    }

    #[test]
    fn guide_is_case_insensitive() {
        assert_eq!(guide("CORE"), guide("core"));
        assert_eq!(guide("Hands"), guide("hands"));
        assert_eq!(guide("JOKERS"), guide("jokers"));
    }

    #[test]
    fn guide_case_insensitive_aliases() {
        assert_eq!(guide("RULES"), guide("rules"));
        assert_eq!(guide("SCORING"), guide("scoring"));
    }
}
