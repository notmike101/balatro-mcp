use serde_json::{Value, json};
use std::sync::LazyLock;

use super::scoring::score_hand;

static EMPTY_VEC: [Value; 0] = [];
static EMPTY_MAP: LazyLock<serde_json::Map<String, Value>> = LazyLock::new(serde_json::Map::new);

pub const DECISION_STATES: &[&str] = &[
    "BLIND_SELECT",
    "SELECTING_HAND",
    "SHOP",
    "GAME_OVER",
    "TAROT_PACK",
    "PLANET_PACK",
    "SPECTRAL_PACK",
    "STANDARD_PACK",
    "BUFFOON_PACK",
];

pub const SAFE_TRANSITION_ACTIONS: &[&str] = &[
    "skip_tutorial",
    "dismiss_unlock_overlay",
    "ensure_menu_ui",
    "cash_out",
];

pub fn is_decision_state(state: &str) -> bool {
    DECISION_STATES.contains(&state)
}

fn get_array<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> &'a [Value] {
    obj.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&EMPTY_VEC)
}

pub fn build_policy_state(
    observation: &Value,
    play_limit: usize,
    discard_limit: usize,
    target_limit: usize,
) -> Value {
    let game = observation
        .get("game")
        .and_then(|g| g.get("state"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let run = observation
        .get("run")
        .and_then(|r| r.as_object())
        .unwrap_or(&EMPTY_MAP);
    let areas = observation
        .get("areas")
        .and_then(|a| a.as_object())
        .unwrap_or(&EMPTY_MAP);

    let hand_array = get_array(areas, "hand");
    let jokers_array = get_array(areas, "jokers");
    let consumables_array = get_array(areas, "consumables");
    let poker_hands = observation
        .get("poker_hands")
        .cloned()
        .unwrap_or(Value::Null);

    let dollars = run.get("dollars").and_then(|d| d.as_i64()).unwrap_or(0);
    let full_interest_floor = 25;
    let interest_cap = 5;
    let dollars_to_next_interest = std::cmp::max(0, full_interest_floor - dollars);
    let current_interest = if dollars >= full_interest_floor {
        interest_cap
    } else {
        std::cmp::max(0, dollars.saturating_sub(10))
    };
    let current_interest_floor = if dollars >= full_interest_floor {
        interest_cap
    } else {
        std::cmp::max(0i64, dollars.saturating_sub(10))
    };

    let blind_chips = run
        .get("blind")
        .and_then(|b| b.get("chips_required"))
        .and_then(|c| c.as_i64())
        .unwrap_or(0);
    let hands_left = run.get("hands_left").and_then(|h| h.as_i64()).unwrap_or(0);
    let chips_remaining = std::cmp::max(0i64, blind_chips);

    let best_play_estimated = estimate_best_play(observation);
    let best_play_clears = best_play_estimated >= blind_chips && blind_chips > 0;
    let best_play_surplus = if blind_chips > 0 {
        Some(best_play_estimated - blind_chips)
    } else {
        None
    };
    let estimated_plays_needed = if best_play_estimated > 0 && blind_chips > 0 {
        std::cmp::max(
            1,
            (chips_remaining + best_play_estimated - 1) / best_play_estimated,
        )
    } else {
        0
    };

    let legal_actions = generate_legal_actions(
        observation,
        hand_array,
        jokers_array,
        consumables_array,
        play_limit,
        discard_limit,
        target_limit,
    );
    let decision_checks = build_decision_checks(
        observation,
        jokers_array,
        consumables_array,
        run,
        areas,
        &legal_actions,
    );
    let hand_analysis = analyze_hands(hand_array, &poker_hands);
    let joker_order_hint = classify_jokers(jokers_array);

    json!({
        "game": { "state": game, "round": run.get("round"), "blind": run.get("blind") },
        "economy": {
            "dollars": dollars, "full_interest_floor": full_interest_floor, "interest_cap": interest_cap,
            "current_interest": current_interest, "current_interest_floor": current_interest_floor,
            "dollars_to_next_interest": dollars_to_next_interest,
            "spendable_without_losing_current_interest": dollars.saturating_sub(10).max(0),
            "spendable_without_losing_full_interest": dollars.saturating_sub(full_interest_floor).max(0),
        },
        "score_pressure": {
            "blind_chips_required": blind_chips, "hands_left": hands_left, "chips_remaining": chips_remaining,
            "best_play_estimated_score": best_play_estimated, "best_play_clears_blind": best_play_clears,
            "best_play_surplus": best_play_surplus, "estimated_best_plays_needed": estimated_plays_needed,
        },
        "slots": { "jokers": extract_slots(jokers_array), "consumables": extract_slots(consumables_array) },
        "legal_actions": legal_actions, "hand_analysis": hand_analysis,
        "decision_checks": decision_checks,
        "most_played_poker_hand": run.get("most_played_poker_hand").and_then(|m| m.as_str()).unwrap_or("High Card"),
        "run_phase": classify_run_phase(run), "joker_order_hint": joker_order_hint,
    })
}

fn estimate_best_play(observation: &Value) -> i64 {
    let areas = observation
        .get("areas")
        .and_then(|a| a.as_object())
        .unwrap_or(&EMPTY_MAP);
    let hand_array = get_array(areas, "hand");
    let poker_hands = observation.get("poker_hands");
    let mut card_counts = std::collections::HashMap::new();
    let mut suits = std::collections::HashMap::new();
    for card in hand_array {
        if let Some(base) = card.get("base").and_then(|b| b.as_object()) {
            let rank = base.get("rank").and_then(|r| r.as_str()).unwrap_or("");
            *card_counts.entry(rank).or_insert(0i64) += 1;
        }
        if let Some(sv) = card.get("suits").and_then(|s| s.as_array()) {
            for suit in sv {
                if let Some(s) = suit.get("key").and_then(|k| k.as_str()) {
                    *suits.entry(s).or_insert(0i64) += 1;
                }
            }
        }
    }
    let best_hand = determine_best_hand(&card_counts, &suits);
    if let Some(po) = poker_hands.and_then(|p| p.as_object()) {
        let vals = po
            .get("values")
            .and_then(|v| v.as_object())
            .unwrap_or(&EMPTY_MAP);
        if let Some(hv) = vals.get(&best_hand).and_then(|h| h.as_object()) {
            let chips = hv.get("chips").and_then(|c| c.as_i64()).unwrap_or(5);
            let mult = hv.get("mult").and_then(|m| m.as_i64()).unwrap_or(1);
            return chips * mult;
        }
    }
    5
}

fn determine_best_hand(
    card_counts: &std::collections::HashMap<&str, i64>,
    suits: &std::collections::HashMap<&str, i64>,
) -> String {
    let total: i64 = card_counts.values().sum();
    let max_suit: Option<(&str, i64)> =
        suits.iter().max_by_key(|&(_, c)| *c).map(|(k, v)| (*k, *v));
    let max_suit_count = max_suit.map(|(_, count)| count).unwrap_or(0);
    let counts: Vec<i64> = card_counts.values().cloned().collect();
    let max_count = counts.iter().max().copied().unwrap_or(0);
    let distinct: Vec<i64> = counts.into_iter().filter(|&c| c > 0).collect();
    if max_count >= 5 {
        "Five of a Kind".into()
    } else if max_count == 4 {
        if max_suit_count >= 4 {
            "Flush Five".into()
        } else {
            "Four of a Kind".into()
        }
    } else if max_count == 3 && distinct.len() >= 2 {
        if distinct.iter().any(|&c| c == 2) {
            "Full House".into()
        } else {
            "Three of a Kind".into()
        }
    } else if max_count == 2 && distinct.iter().filter(|&&c| c == 2).count() >= 2 {
        "Two Pair".into()
    } else if max_count == 2 {
        "Pair".into()
    } else if max_suit_count >= 5 {
        "Flush".into()
    } else if total >= 5 {
        "Straight".into()
    } else if total >= 4 {
        "Straight".into()
    } else if max_suit_count >= 3 {
        "Flush".into()
    } else {
        "High Card".into()
    }
}

fn classify_jokers(jokers_array: &[Value]) -> Vec<&'static str> {
    let mut hints = Vec::new();
    let mut has_economy = false;
    let mut has_chips = false;
    let mut has_add_mult = false;
    let mut has_multiply_mult = false;
    for joker in jokers_array {
        if let Some(ab) = joker.get("ability").and_then(|a| a.as_object()) {
            if ab.get("dollars").is_some() || ab.get("interest").is_some() {
                has_economy = true;
            }
            if ab.get("chips").is_some() || ab.get("h_chips").is_some() {
                has_chips = true;
            }
            if ab.get("mult").is_some() || ab.get("h_mult").is_some() {
                has_add_mult = true;
            }
            if ab.get("x_mult").is_some() || ab.get("h_x_mult").is_some() {
                has_multiply_mult = true;
            }
        }
    }
    if has_economy {
        hints.push("utility/economy");
    }
    if has_chips {
        hints.push("chips");
    }
    if has_add_mult {
        hints.push("add_mult");
    }
    if has_multiply_mult {
        hints.push("multiply_mult");
    }
    if hints.is_empty() {
        hints.push("none");
    }
    hints
}

fn extract_slots(array: &[Value]) -> Value {
    let count = array.len();
    let limit = 5;
    let open = array
        .iter()
        .filter(|j| j.get("empty").and_then(|e| e.as_bool()).unwrap_or(false))
        .count();
    json!({ "count": count, "limit": limit, "open": open })
}

fn classify_run_phase(run: &serde_json::Map<String, Value>) -> &'static str {
    let ante = run.get("ante").and_then(|a| a.as_i64()).unwrap_or(1);
    if ante <= 2 {
        "early"
    } else if ante <= 5 {
        "mid"
    } else {
        "late"
    }
}

fn analyze_hands(hand_array: &[Value], poker_hands: &Value) -> Value {
    if hand_array.is_empty() {
        return json!({});
    }
    let best_score = estimate_best_play_raw(hand_array, poker_hands);
    json!({ "best_play": { "estimated_score": best_score, "score_kind": "estimate" } })
}

fn estimate_best_play_raw(hand_array: &[Value], poker_hands: &Value) -> i64 {
    let mut card_counts = std::collections::HashMap::new();
    let mut suits = std::collections::HashMap::new();
    for card in hand_array {
        if let Some(base) = card.get("base").and_then(|b| b.as_object()) {
            let rank = base.get("rank").and_then(|r| r.as_str()).unwrap_or("");
            *card_counts.entry(rank).or_insert(0i64) += 1;
        }
        if let Some(sv) = card.get("suits").and_then(|s| s.as_array()) {
            for suit in sv {
                if let Some(s) = suit.get("key").and_then(|k| k.as_str()) {
                    *suits.entry(s).or_insert(0i64) += 1;
                }
            }
        }
    }
    let best_hand = determine_best_hand(&card_counts, &suits);
    if let Some(po) = poker_hands.as_object() {
        let vals = po
            .get("values")
            .and_then(|v| v.as_object())
            .unwrap_or(&EMPTY_MAP);
        if let Some(hv) = vals.get(&best_hand).and_then(|h| h.as_object()) {
            let chips = hv.get("chips").and_then(|c| c.as_i64()).unwrap_or(5);
            let mult = hv.get("mult").and_then(|m| m.as_i64()).unwrap_or(1);
            return chips * mult;
        }
    }
    5
}

fn generate_legal_actions(
    observation: &Value,
    hand_array: &[Value],
    jokers_array: &[Value],
    consumables_array: &[Value],
    play_limit: usize,
    discard_limit: usize,
    target_limit: usize,
) -> Vec<Value> {
    let mut actions = Vec::new();
    let state = observation
        .get("game")
        .and_then(|g| g.get("state"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let run = observation
        .get("run")
        .and_then(|r| r.as_object())
        .unwrap_or(&EMPTY_MAP);
    let hand_count = hand_array.len();
    let joker_count = jokers_array.len();
    let consumable_count = consumables_array.len();
    let joker_limit = run.get("joker_slots").and_then(|j| j.as_i64()).unwrap_or(5) as usize;
    let consumable_limit = run
        .get("consumable_slots")
        .and_then(|c| c.as_i64())
        .unwrap_or(2) as usize;
    let joker_open = joker_limit.saturating_sub(joker_count);
    let consumable_open = consumable_limit.saturating_sub(consumable_count);

    match state {
        "MENU" => {
            if let Some(ready) = observation.get("ready") {
                if ready
                    .get("saved_game_present")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    actions.push(json!({ "action_id": "resume_run", "action": "resume_run", "reason": "saved run available" }));
                } else {
                    actions.push(json!({ "action_id": "open_run_setup", "action": "ui_click", "ui_id": "main_menu_play", "reason": "main menu Play button is available" }));
                }
            }
        }
        "SELECTING_HAND" | "BLIND_SELECT" => {
            if state == "BLIND_SELECT" {
                if let Some(choices) = run.get("blind_choices").and_then(Value::as_object) {
                    for (name, choice) in choices {
                        if choice.get("state").and_then(Value::as_str) == Some("Select") {
                            actions.push(json!({"action_id":format!("select_{}", name.to_ascii_lowercase()),"action":"select_blind","blind":name,"reason":format!("select {} blind", name)}));
                        }
                    }
                }
            }
            let hands_left = run.get("hands_left").and_then(|h| h.as_i64()).unwrap_or(0) as usize;
            if hands_left > 0 && hand_count > 0 {
                for pc in 1..=std::cmp::min(hand_count, play_limit) {
                    let ids: Vec<String> = (0..pc)
                        .filter_map(|i| {
                            hand_array
                                .get(i)
                                .and_then(|c| c.get("instance_id"))
                                .and_then(|id| id.as_str())
                                .map(|s| s.to_string())
                        })
                        .collect();
                    if !ids.is_empty() {
                        let score = score_hand(observation, Some(&(0..pc).collect::<Vec<_>>()));
                        actions.push(json!({ "action_id": format!("play_{}", ids.join("_")), "action": "play", "card_ids": ids, "estimated_score": score.estimated_score, "score_quality": score.estimate_quality, "reason": format!("play {} cards ({} hands left)", pc, hands_left) }));
                    }
                }
            }
            let discards_left = run
                .get("discards_left")
                .and_then(|d| d.as_i64())
                .unwrap_or(0) as usize;
            if discards_left > 0 && hand_count > 1 {
                for dc in 1..=std::cmp::min(hand_count - 1, discard_limit) {
                    let ids: Vec<String> = (0..dc)
                        .filter_map(|i| {
                            hand_array
                                .get(i)
                                .and_then(|c| c.get("instance_id"))
                                .and_then(|id| id.as_str())
                                .map(|s| s.to_string())
                        })
                        .collect();
                    if !ids.is_empty() {
                        actions.push(json!({ "action_id": format!("discard_{}", ids.join("_")), "action": "discard", "card_ids": ids, "reason": format!("discard {} cards ({} discards left)", dc, discards_left) }));
                    }
                }
            }
        }
        "SHOP" => {
            if joker_open > 0 {
                if let Some(bc) = run.get("blind_choices") {
                    if let Some(small) = bc.get("Small") {
                        if small.get("state").and_then(|s| s.as_str()) == Some("Select") {
                            actions.push(json!({ "action_id": "select_small_blind", "action": "ui_click", "ui_id": "blind_select_small", "reason": "select Small Blind" }));
                        }
                    }
                    if let Some(big) = bc.get("Big") {
                        if big.get("state").and_then(|s| s.as_str()) == Some("Select") {
                            actions.push(json!({ "action_id": "select_big_blind", "action": "ui_click", "ui_id": "blind_select_big", "reason": "select Big Blind" }));
                        }
                    }
                }
            }
            if consumable_open > 0 || joker_open > 0 {
                let shop_cards = observation
                    .pointer("/areas/shop")
                    .and_then(Value::as_array)
                    .or_else(|| observation.pointer("/shop/cards").and_then(Value::as_array))
                    .map(|v| v.as_slice())
                    .unwrap_or(&EMPTY_VEC);
                for (i, c) in shop_cards.iter().enumerate() {
                    if let Some(name) = c.get("name").and_then(|n| n.as_str()) {
                        actions.push(json!({ "action_id": format!("buy_consumable_{}", i), "action": "buy_card", "card_index": i, "reason": format!("buy {} (slot available)", name) }));
                    }
                }
            }
            if run.get("reroll_cost").and_then(Value::as_i64).unwrap_or(0) > 0 {
                actions.push(json!({"action_id":"reroll_shop","action":"reroll","reason":"reroll shop only when economy and score pressure justify it"}));
            }
        }
        "TAROT_PACK" | "PLANET_PACK" | "SPECTRAL_PACK" | "STANDARD_PACK" | "BUFFOON_PACK" => {
            let choices = observation
                .pointer("/areas/pack")
                .and_then(Value::as_array)
                .or_else(|| observation.pointer("/pack/cards").and_then(Value::as_array))
                .map(|v| v.as_slice())
                .unwrap_or(&EMPTY_VEC);
            for (index, card) in choices.iter().enumerate().take(target_limit.max(1)) {
                actions.push(json!({"action_id":format!("pack_{}", index),"action":"choose_pack","card_index":index,"name":card.get("name")}));
            }
        }
        _ => {}
    }

    for (index, card) in consumables_array.iter().enumerate() {
        let name = card
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("consumable");
        actions.push(json!({"action_id":format!("use_consumable_{}", index),"action":"use_consumable","card_index":index,"target_limit":target_limit,"reason":format!("evaluate {} before advancing", name)}));
        actions.push(json!({"action_id":format!("sell_consumable_{}", index),"action":"sell_card","area":"consumables","card_index":index,"reason":format!("sell {} if no useful target exists", name)}));
    }
    for (index, joker) in jokers_array.iter().enumerate() {
        actions.push(json!({"action_id":format!("sell_joker_{}", index),"action":"sell_card","area":"jokers","card_index":index,"reason":format!("sell {} when required", joker.get("name").and_then(Value::as_str).unwrap_or("Joker"))}));
    }
    if !matches!(
        state,
        "MENU"
            | "SELECTING_HAND"
            | "BLIND_SELECT"
            | "SHOP"
            | "TAROT_PACK"
            | "PLANET_PACK"
            | "SPECTRAL_PACK"
            | "STANDARD_PACK"
            | "BUFFOON_PACK"
    ) {
        for transition in SAFE_TRANSITION_ACTIONS {
            actions.push(json!({"action_id":transition,"action":"safe_transition","transition":transition,"reason":"confirmed non-strategic transition"}));
        }
    }

    if joker_count > 1 {
        for i in 0..joker_count {
            for j in 0..joker_count {
                if i != j {
                    actions.push(json!({ "action_id": format!("move_joker_{}_to_{}", i, j), "action": "move_joker", "from_index": i, "to_index": j, "reason": "reorder joker trigger sequence" }));
                }
            }
        }
    }
    if hand_count > 1 {
        for i in 0..hand_count {
            for j in 0..hand_count {
                if i != j {
                    actions.push(json!({ "action_id": format!("move_card_{}_to_{}", i, j), "action": "move_card", "from_index": i, "to_index": j, "reason": "reorder hand for play" }));
                }
            }
        }
    }
    actions
}

fn build_decision_checks(
    observation: &Value,
    jokers_array: &[Value],
    consumables_array: &[Value],
    run: &serde_json::Map<String, Value>,
    _areas: &serde_json::Map<String, Value>,
    legal_actions: &[Value],
) -> Value {
    let joker_count = jokers_array.len();
    let state = observation
        .get("game")
        .and_then(|g| g.get("state"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let hand_order = Vec::<Value>::new();
    let mut joker_order = Vec::<Value>::new();
    let move_card_actions = Vec::<Value>::new();
    let mut move_joker_actions = Vec::<Value>::new();
    if joker_count > 1 {
        for i in 0..joker_count {
            for j in 0..joker_count {
                if i != j {
                    joker_order.push(json!({ "from": i, "to": j }));
                    move_joker_actions.push(json!({ "action_id": format!("move_joker_{}_to_{}", i, j), "action": "move_joker", "from_index": i, "to_index": j }));
                }
            }
        }
    }
    let owned_consumables: Vec<Value> = consumables_array
        .iter()
        .filter_map(|c| {
            c.get("name")
                .and_then(|n| n.as_str())
                .map(|name| json!({ "name": name, "type": c.get("type") }))
        })
        .collect();
    let use_actions: Vec<Value> = legal_actions
        .iter()
        .filter(|action| action.get("action").and_then(Value::as_str) == Some("use_consumable"))
        .cloned()
        .collect();
    let sell_actions: Vec<Value> = legal_actions
        .iter()
        .filter(|action| action.get("action").and_then(Value::as_str) == Some("sell_card"))
        .cloned()
        .collect();
    let pack_actions: Vec<Value> = legal_actions
        .iter()
        .filter(|action| action.get("action").and_then(Value::as_str) == Some("choose_pack"))
        .cloned()
        .collect();
    let reroll_actions: Vec<Value> = legal_actions
        .iter()
        .filter(|action| action.get("action").and_then(Value::as_str) == Some("reroll"))
        .cloned()
        .collect();
    let in_shop = state == "SHOP";
    let mut shop_data = serde_json::Map::new();
    shop_data.insert("required".into(), json!(in_shop));
    let joker_slots = extract_slots(jokers_array);
    let consumable_slots = extract_slots(consumables_array);
    let blind = run
        .get("blind")
        .and_then(|b| b.as_object())
        .unwrap_or(&EMPTY_MAP);
    let boss = blind.get("boss").and_then(|b| b.as_str()).unwrap_or("");
    let boss_effect = blind
        .get("effect")
        .and_then(|e| e.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");
    let is_boss = !boss.is_empty() || !boss_effect.is_empty();
    json!({
        "ordering": { "required_before_close_play": joker_count > 1, "hand_order": hand_order, "joker_order": joker_order, "move_card_actions": move_card_actions, "move_joker_actions": move_joker_actions, "instruction": "Evaluate hand and Joker trigger order when a scoring effect can depend on sequence; do not move cards by default, but do not dismiss legal reorder actions.", "estimate_caveat": "Play estimates may not model every ordering interaction; verify relevant ordering when margin is tight." },
        "consumables": { "required": !owned_consumables.is_empty(), "owned": owned_consumables, "use_actions": use_actions, "sell_actions": sell_actions, "shop_purchase_actions": legal_actions.iter().filter(|action| action.get("action").and_then(Value::as_str) == Some("buy_card")).cloned().collect::<Vec<_>>(), "instruction": "Evaluate every owned use/sell action and every shop consumable purchase before exiting or advancing." },
        "shop": shop_data,
        "slots": { "required": true, "jokers": joker_slots, "consumables": consumable_slots, "instruction": "Track joker and consumable slot counts (count/limit/open) across all purchases; do not buy if no slots remain without a voucher or other expansion." },
        "boss_debuff": { "required": is_boss, "current_blind": { "boss": is_boss, "name": blind.get("name").and_then(|n| n.as_str()), "effect": Some(boss_effect), "disabled": blind.get("disabled").and_then(|d| d.as_str()) }, "upcoming_boss": { "state": blind.get("state").and_then(|s| s.as_str()).unwrap_or("Upcoming") }, "debuffed_cards": observation.pointer("/areas/debuffed_cards").cloned().unwrap_or_else(|| json!([])), "debuffed_jokers": observation.pointer("/areas/debuffed_jokers").cloned().unwrap_or_else(|| json!([])), "reroll_actions": reroll_actions, "select_actions": pack_actions, "instruction": "Before selecting or playing a Boss Blind, inspect its live effect, lookup_rule details, debuffed cards/Jokers, and legal boss-reroll actions." },
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use serde_json::json;

    fn observation(state: &str) -> Value {
        json!({
            "game": {"state": state},
            "run": {"ante": 3, "round": 2, "dollars": 18, "hands_left": 2, "discards_left": 1,
                "blind": {"name": "Boss", "boss": "Boss", "chips_required": 100},
                "joker_slots": 2, "consumable_slots": 1,
                "blind_choices": {"Small": {"state": "Select"}, "Big": {"state": "Select"}}},
            "areas": {
                "hand": [
                    {"instance_id":"a", "base":{"rank":"A"}, "suits":[{"key":"H"}]},
                    {"instance_id":"b", "base":{"rank":"A"}, "suits":[{"key":"H"}]},
                    {"instance_id":"c", "base":{"rank":"K"}, "suits":[{"key":"H"}]},
                    {"instance_id":"d", "base":{"rank":"Q"}, "suits":[{"key":"H"}]},
                    {"instance_id":"e", "base":{"rank":"J"}, "suits":[{"key":"H"}]}
                ],
                "jokers": [{"name":"Chip Joker", "ability":{"chips":20}}, {"name":"Mult Joker", "ability":{"mult":4}}],
                "consumables": [{"name":"Tower", "type":"Tarot"}]
            },
            "poker_hands": {"values": {"Flush": {"chips":20,"mult":4}, "Pair": {"chips":10,"mult":2}}}
        })
    }

    #[test]
    fn decision_states_and_unknown_states_are_classified() {
        assert!(DECISION_STATES.iter().all(|state| is_decision_state(state)));
        assert!(!is_decision_state("MENU"));
        assert!(!is_decision_state("UNKNOWN"));
    }

    #[test]
    fn policy_state_covers_play_discard_ordering_and_checks() {
        let state = build_policy_state(&observation("SELECTING_HAND"), 2, 1, 60);
        assert_eq!(state["legal_actions"][0]["action"], "play");
        assert!(
            state["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action"] == "discard")
        );
        assert!(
            state["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action"] == "move_joker")
        );
        assert_eq!(
            state["decision_checks"]["ordering"]["required_before_close_play"],
            true
        );
        assert_eq!(state["decision_checks"]["consumables"]["required"], true);
        assert_eq!(state["decision_checks"]["boss_debuff"]["required"], true);
        assert!(
            state["score_pressure"]["best_play_estimated_score"]
                .as_i64()
                .unwrap()
                > 0
        );
    }

    #[test]
    fn policy_state_covers_menu_shop_and_empty_inputs() {
        let menu = build_policy_state(
            &json!({"game":{"state":"MENU"},"ready":{"saved_game_present":false}}),
            40,
            40,
            60,
        );
        assert_eq!(menu["legal_actions"][0]["action"], "ui_click");
        let menu_saved = build_policy_state(
            &json!({"game":{"state":"MENU"},"ready":{"saved_game_present":true}}),
            40,
            40,
            60,
        );
        assert_eq!(menu_saved["legal_actions"][0]["action"], "resume_run");
        let menu_without_ready = build_policy_state(&json!({"game":{"state":"MENU"}}), 40, 40, 60);
        assert!(
            menu_without_ready["legal_actions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let mut shop_observation = observation("SHOP");
        shop_observation["run"]["consumable_slots"] = json!(2);
        shop_observation["areas"]["shop"] = json!([{"name":"Planet"}]);
        let shop = build_policy_state(&shop_observation, 40, 40, 60);
        assert!(
            shop["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action"] == "buy_card")
        );
        shop_observation["run"]["joker_slots"] = json!(5);
        let shop_blinds = build_policy_state(&shop_observation, 40, 40, 60);
        assert!(
            shop_blinds["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["ui_id"] == "blind_select_small")
        );
        assert!(
            shop_blinds["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["ui_id"] == "blind_select_big")
        );
        let mut blind_without_choices = observation("BLIND_SELECT");
        blind_without_choices["run"]["blind_choices"] = Value::Null;
        assert!(
            build_policy_state(&blind_without_choices, 0, 0, 1)["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["action"] != "select_blind")
        );
        let no_ids = json!({
            "game":{"state":"SELECTING_HAND"},
            "run":{"hands_left":1,"discards_left":1},
            "areas":{"hand":[{"base":{"rank":"A"}},{"base":{"rank":"K"}}]}
        });
        assert!(
            build_policy_state(&no_ids, 40, 40, 60)["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["action"] != "play" && a["action"] != "discard")
        );
        let empty = build_policy_state(&json!({}), 0, 0, 0);
        assert_eq!(empty["game"]["state"], "");
        assert!(
            empty["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action"] == "safe_transition")
        );
    }

    #[test]
    fn policy_state_covers_run_phases_and_slot_limits() {
        for (ante, phase) in [(1, "early"), (3, "mid"), (6, "late")] {
            let mut obs = observation("GAME_OVER");
            obs["run"]["ante"] = json!(ante);
            let state = build_policy_state(&obs, 40, 40, 60);
            assert_eq!(state["run_phase"], phase);
        }
        let mut obs = observation("SHOP");
        obs["run"]["joker_slots"] = json!(1);
        let state = build_policy_state(&obs, 40, 40, 60);
        assert_eq!(state["slots"]["jokers"]["open"], 0);
    }

    #[test]
    fn policy_state_emits_pack_blind_use_sell_and_safe_actions() {
        let mut pack = observation("TAROT_PACK");
        pack["areas"]["pack"] = json!([{"name":"The Tower"}]);
        let actions = build_policy_state(&pack, 40, 40, 60)["legal_actions"].clone();
        assert!(
            actions
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action"] == "choose_pack")
        );

        let mut blind = observation("BLIND_SELECT");
        blind["run"]["blind_choices"] = json!({"Small":{"state":"Select"}});
        let actions = build_policy_state(&blind, 40, 40, 60)["legal_actions"].clone();
        assert!(
            actions
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action"] == "select_blind")
        );

        let mut game_over = observation("GAME_OVER");
        game_over["areas"]["jokers"] = json!([{"name":"Joker"}]);
        let actions = build_policy_state(&game_over, 40, 40, 60)["legal_actions"].clone();
        assert!(
            actions
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action"] == "sell_card" && a["area"] == "jokers")
        );
        assert!(
            actions
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action"] == "safe_transition")
        );
    }

    #[test]
    fn best_hand_classifier_covers_rank_suit_and_fallback_branches() {
        let cases: Vec<(Vec<(&str, i64)>, Vec<(&str, i64)>, &str)> = vec![
            (vec![("A", 5)], vec![], "Five of a Kind"),
            (vec![("A", 4)], vec![("H", 4)], "Flush Five"),
            (vec![("A", 4)], vec![], "Four of a Kind"),
            (vec![("A", 3), ("K", 2)], vec![], "Full House"),
            (vec![("A", 3), ("K", 1)], vec![], "Three of a Kind"),
            (vec![("A", 2), ("K", 2)], vec![], "Two Pair"),
            (vec![("A", 2)], vec![], "Pair"),
            (vec![("A", 1)], vec![("H", 5)], "Flush"),
            (
                vec![("A", 1), ("K", 1), ("Q", 1), ("J", 1), ("T", 1)],
                vec![],
                "Straight",
            ),
            (
                vec![("A", 1), ("K", 1), ("Q", 1), ("J", 1)],
                vec![],
                "Straight",
            ),
            (vec![("A", 1), ("K", 1), ("Q", 1)], vec![("H", 3)], "Flush"),
            (vec![("A", 1)], vec![], "High Card"),
        ];
        for (counts, suits, expected) in cases {
            let counts = counts.into_iter().collect();
            let suits = suits.into_iter().collect();
            assert_eq!(determine_best_hand(&counts, &suits), expected);
        }
        assert_eq!(estimate_best_play(&json!({"areas":{"hand":[]}})), 5);
        assert_eq!(estimate_best_play_raw(&[], &json!({})), 5);
    }

    #[test]
    fn policy_state_covers_economy_joker_hints_shop_edges_and_limits() {
        let mut obs = observation("SHOP");
        obs["run"]["dollars"] = json!(30);
        obs["run"]["reroll_cost"] = json!(2);
        obs["run"]["joker_slots"] = json!(5);
        obs["run"]["consumable_slots"] = json!(2);
        obs["areas"]["jokers"] = json!([
            {"ability":{"dollars":1}}, {"ability":{"chips":2}},
            {"ability":{"mult":3}}, {"ability":{"x_mult":1.5}}
        ]);
        obs["areas"]["consumables"] = json!([]);
        obs["areas"]["shop"] = json!([{"name":"Joker"}]);
        let state = build_policy_state(&obs, 1, 1, 1);
        assert_eq!(state["economy"]["current_interest"], 5);
        assert!(state["joker_order_hint"].as_array().unwrap().len() >= 4);
        assert!(
            state["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["action"] == "reroll")
        );

        let mut blind = observation("BLIND_SELECT");
        blind["run"]["blind_choices"] = json!({"Small":{"state":"Skip"},"Big":{"state":"Select"}});
        let actions = build_policy_state(&blind, 0, 0, 1)["legal_actions"].clone();
        assert!(
            actions
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["blind"] == "Big")
        );
    }
}
