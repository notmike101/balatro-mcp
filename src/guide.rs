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
            "Goal: clear Small, Big, and Boss blinds through Ante 8. Agent loop: game_status; query matching replays before each blind; get_decision; examine decision_checks.ordering when jokers are present; examine decision_checks.consumables for owned or shop Tarot/Planet/Spectral; examine decision_checks.shop and decision_checks.slots during SHOP state; lookup unknown effects; take one legal action with current decision_id; use play_selected or discard_selected with 1-based card_indices for arbitrary hand positions; page get_decision when legal_actions_truncated is true; observe is read-only and does not return actions, while wait_for_state only confirms state; call get_decision after either. In GAME_OVER, from_game_over is a ui_click and return_to_menu is also a safe_transition. Never infer face-down cards.",
        ),
        "hands" | "scoring" => Some(
            "Poker hands score Chips multiplied by Mult. Planet consumables level a named hand. Use live hand_values; controller scores are estimates unless explicitly exact. score_analysis.hand_key describes the cards scored for that call, score_analysis.score_scope identifies current-hand versus selected-card scope, and score_analysis.run_chips plus score_analysis.blind_chips_remaining expose cumulative run progress. run_info.cards_played is cumulative by rank across the run; run_info.round_scores.cards_played.amt is the current round's count.",
        ),
        "actions" | "discards" => Some(
            "Hands score; discards redraw without scoring. Check discard_status.remaining, discard_status.used, and discard_status.configured_limit; current_limit is the configured capacity after recorded discard actions and does not include transient bridge lag. Consider all legal discard sizes and a specific draw goal. Use only legal actions.",
        ),
        "economy" | "shops" => Some(
            "Evaluate Joker slots, consumable slots, price, interest, and the next blind. Buy one item at a time, then reread decision state.",
        ),
        "blinds" | "bosses" | "debuffs" => Some(
            "Boss blinds impose special rules and debuffs. Before selecting or playing one, inspect strategy.decision_checks.boss_debuff, lookup_rule details, debuffed cards/Jokers, score margin, and legal boss-reroll actions.",
        ),
        "jokers" | "editions" => Some(
            "Jokers affect Chips, Mult, economy, and rules. Foil adds Chips, Holographic adds Mult, Polychrome adds X Mult, Negative adds a Joker slot. Stickers are separate constraints. Review decision_checks.ordering and legal move_joker actions when trigger order can matter.",
        ),
        "cards" | "enhancements" | "seals" => Some(
            "Playing cards may have one enhancement, edition, and seal. Face-down card identity is always unknown.",
        ),
        "consumables" | "vouchers" | "stakes" | "decks" | "tags" | "progression" => Some(
            "Decks, Stakes, Vouchers, Tags, and consumables change run rules and resources. Evaluate every owned and shop consumable through decision_checks.consumables before exiting; look up unfamiliar effects before acting.",
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
