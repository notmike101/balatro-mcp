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
            "Goal: clear Small, Big, and Boss blinds through Ante 8. Agent loop: game_status; query matching replays before each blind; get_decision; examine decision_checks.ordering when jokers are present; examine decision_checks.consumables for owned or shop Tarot/Planet/Spectral; examine decision_checks.shop and decision_checks.slots during SHOP state; lookup unknown effects; take one legal action with current decision_id; observe the new state. Never infer face-down cards.",
        ),
        "hands" | "scoring" => Some(
            "Poker hands score Chips multiplied by Mult. Planet consumables level a named hand. Use live hand_values; controller scores are estimates unless explicitly exact.",
        ),
        "actions" | "discards" => Some(
            "Hands score; discards redraw without scoring. Consider all legal discard sizes and a specific draw goal. Use only legal actions.",
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
