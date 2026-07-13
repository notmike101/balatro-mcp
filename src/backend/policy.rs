use serde_json::{Value, json};
use std::sync::LazyLock;

use super::{
    observation,
    scoring::{self, score_hand},
};

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
    "SMODS_BOOSTER_OPENED",
];

pub const SAFE_TRANSITION_ACTIONS: &[&str] = &[
    "skip_tutorial",
    "dismiss_unlock_overlay",
    "ensure_menu_ui",
    "cash_out",
    "return_to_menu",
];

pub fn is_decision_state(state: &str) -> bool {
    DECISION_STATES.contains(&state)
}

fn get_array<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> &'a [Value] {
    obj.get(key)
        .or_else(|| {
            (key == "consumables")
                .then(|| obj.get("consumeables"))
                .flatten()
        })
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&EMPTY_VEC)
}

fn card_id(card: &Value) -> Option<String> {
    card.get("instance_id")
        .or_else(|| card.get("id"))
        .and_then(|id| {
            id.as_str()
                .map(str::to_owned)
                .or_else(|| Some(id.to_string()))
        })
}

fn card_id_value(card: &Value) -> Value {
    card.get("instance_id")
        .or_else(|| card.get("id"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn recommendation_card(card: &Value, index: usize) -> Value {
    if scoring::card_is_hidden(card) {
        return json!({
            "index": index + 1,
            "instance_id": card_id_value(card),
            "hidden": true
        });
    }

    let base = card.get("base").and_then(Value::as_object);
    json!({
        "index": index + 1,
        "instance_id": card_id_value(card),
        "rank": card.get("rank").or_else(|| base.and_then(|base| base.get("rank"))),
        "suit": card.get("suit").or_else(|| base.and_then(|base| base.get("suit"))),
        "base": {
            "rank": base.and_then(|base| base.get("rank")),
            "value": base.and_then(|base| base.get("value")),
            "id": base.and_then(|base| base.get("id")),
            "suit": base.and_then(|base| base.get("suit")),
            "nominal": base.and_then(|base| base.get("nominal"))
        },
        "name": card.get("name"),
        "face_down": card.get("face_down")
    })
}

pub fn build_policy_state(
    observation: &Value,
    play_limit: usize,
    discard_limit: usize,
    target_limit: usize,
) -> Value {
    let game = observation
        .get("game")
        .and_then(|g| g.get("state_name"))
        .and_then(|s| s.as_str())
        .or_else(|| {
            observation
                .get("game")
                .and_then(|g| g.get("state"))
                .and_then(|s| s.as_str())
        })
        .unwrap_or("");
    let game = if game.eq_ignore_ascii_case("MAIN_MENU") {
        "MENU"
    } else {
        game
    };
    let run_source = observation
        .get("run")
        .or_else(|| observation.get("round"))
        .and_then(|r| r.as_object())
        .unwrap_or(&EMPTY_MAP);
    let mut normalized_run = run_source.clone();
    if !normalized_run.contains_key("blind") {
        if let Some(blind) = observation::active_blind(observation) {
            normalized_run.insert("blind".into(), blind);
        }
    }
    let run = &normalized_run;
    let areas = observation
        .get("areas")
        .and_then(|a| a.as_object())
        .unwrap_or(&EMPTY_MAP);

    let hand_array = get_array(areas, "hand");
    let jokers_array = get_array(areas, "jokers");
    let consumables_array = get_array(areas, "consumables");
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

    let blind_chips = observation::blind_chips_required(observation).unwrap_or(0);
    let current_chips = observation::current_chips(observation);
    let hands_left = run.get("hands_left").and_then(|h| h.as_i64()).unwrap_or(0);
    let discards_left = run
        .get("discards_left")
        .and_then(|d| d.as_i64())
        .unwrap_or(0);
    let discards_used = run
        .get("discards_used")
        .and_then(|d| d.as_i64())
        .unwrap_or(0);
    let configured_discard_limit = run
        .get("starting_params")
        .and_then(|params| params.get("discard_limit"))
        .and_then(Value::as_i64);
    let current_discard_limit = configured_discard_limit
        .map(|limit| limit.saturating_sub(discards_used).max(0))
        .unwrap_or(discards_left);
    let chips_remaining = current_chips.map(|current| blind_chips.saturating_sub(current).max(0));

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
            (chips_remaining.unwrap_or(blind_chips) + best_play_estimated - 1)
                / best_play_estimated,
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
    let hand_analysis = analyze_hands(&observation);
    let joker_order_hint = classify_jokers(jokers_array);

    json!({
        "game": { "state": game, "round": run.get("round"), "blind": run.get("blind") },
        "run": run,
        "economy": {
            "dollars": dollars, "full_interest_floor": full_interest_floor, "interest_cap": interest_cap,
            "current_interest": current_interest, "current_interest_floor": current_interest_floor,
            "dollars_to_next_interest": dollars_to_next_interest,
            "spendable_without_losing_current_interest": dollars.saturating_sub(10).max(0),
            "spendable_without_losing_full_interest": dollars.saturating_sub(full_interest_floor).max(0),
        },
        "score_pressure": {
            "blind_chips_required": blind_chips, "current_chips": current_chips, "hands_left": hands_left, "chips_remaining": chips_remaining,
            "best_play_estimated_score": best_play_estimated, "best_play_clears_blind": best_play_clears,
            "best_play_surplus": best_play_surplus, "estimated_best_plays_needed": estimated_plays_needed,
        },
        "discard_status": {
            "remaining": discards_left, "used": discards_used,
            "current_limit": current_discard_limit, "configured_limit": configured_discard_limit,
        },
        "slots": { "jokers": extract_slots(jokers_array), "consumables": extract_slots(consumables_array) },
        "hand": hand_array, "jokers": jokers_array, "consumables": consumables_array,
        "legal_actions": legal_actions, "hand_analysis": hand_analysis,
        "decision_checks": decision_checks,
        "most_played_poker_hand": run.get("most_played_poker_hand").and_then(|m| m.as_str()).unwrap_or("High Card"),
        "run_phase": classify_run_phase(run), "joker_order_hint": joker_order_hint,
    })
}

/// Reduce a policy snapshot to the fields needed for the normal gameplay loop.
/// The full policy remains available for explicit analysis requests.
pub fn compact_policy_state(full: &Value) -> Value {
    let compact_card = |card: &Value| {
        if scoring::card_is_hidden(card) {
            return json!({
                "index": card.get("index"),
                "instance_id": card.get("instance_id").or_else(|| card.get("id")),
                "hidden": true
            });
        }
        json!({
            "index": card.get("index"),
            "instance_id": card.get("instance_id").or_else(|| card.get("id")),
            "rank": card.get("rank").or_else(|| card.pointer("/base/rank")),
            "suit": card.get("suit").or_else(|| card.pointer("/base/suit")),
            "base": card.get("base").map(|base| json!({
                "rank": base.get("rank"), "value": base.get("value"), "id": base.get("id"),
                "suit": base.get("suit"), "nominal": base.get("nominal")
            })),
            "edition": card.get("edition"),
            "seal": card.get("seal"),
            "enhancement": card.get("enhancement"),
            "face_down": card.get("face_down"),
            "name": card.get("name"),
            "type": card.get("type")
        })
    };
    let cards =
        |key: &str| {
            full.get(key)
                .or_else(|| full.pointer(&format!("/areas/{key}")))
                .or_else(|| {
                    (key == "consumables")
                        .then(|| full.pointer("/areas/consumeables"))
                        .flatten()
                })
                .and_then(Value::as_array)
                .map(|items| {
                    Value::Array(items.iter().map(|card| match key {
                    "hand" => compact_card(card),
                    "jokers" => json!({
                        "name": card.get("name"), "edition": card.get("edition"),
                        "seal": card.get("seal"), "enhancement": card.get("enhancement"),
                        "slot_order": card.get("slot_order")
                    }),
                    _ => json!({"name": card.get("name"), "type": card.get("type")})
                }).collect())
                })
                .unwrap_or_else(|| json!([]))
        };
    let compact_action = |action: &Value| {
        let action_name = action.get("action").and_then(Value::as_str).unwrap_or("");
        let mut compact = serde_json::Map::new();
        if let Some(value) = action.get("action_id") {
            compact.insert("action_id".into(), value.clone());
        }
        if let Some(value) = action.get("action") {
            compact.insert("action".into(), value.clone());
        }
        if matches!(action_name, "play" | "discard") {
            for key in ["selection", "card_index_base", "max_cards"] {
                if let Some(value) = action.get(key) {
                    compact.insert(key.into(), value.clone());
                }
            }
        }
        Value::Object(compact)
    };
    let compact_actions = |pointer: &str| {
        full.pointer(pointer)
            .and_then(Value::as_array)
            .map(|items| Value::Array(items.iter().map(&compact_action).collect()))
            .unwrap_or_else(|| json!([]))
    };
    let action_refs = |pointer: &str| {
        full.pointer(pointer)
            .and_then(Value::as_array)
            .map(|items| {
                Value::Array(
                    items
                        .iter()
                        .filter_map(|item| item.get("action_id"))
                        .cloned()
                        .collect(),
                )
            })
            .unwrap_or_else(|| json!([]))
    };
    let run = full.get("run").cloned().unwrap_or(Value::Null);
    let blind = run
        .get("blind")
        .map(|b| {
            json!({
                "name": b.get("name"), "boss": b.get("boss"),
                "chips_required": b.get("chips_required"), "effect": b.get("effect"),
                "disabled": b.get("disabled")
            })
        })
        .unwrap_or(Value::Null);
    let checks = full.get("decision_checks").cloned().unwrap_or(Value::Null);
    let compact_checks = json!({
        "ordering": {
            "required_before_close_play": checks.pointer("/ordering/required_before_close_play"),
            "joker_order": checks.pointer("/ordering/joker_order"),
            "move_joker_actions": checks.pointer("/ordering/move_joker_actions")
        },
        "consumables": {
            "required": checks.pointer("/consumables/required"),
            "owned": checks.pointer("/consumables/owned"),
            "use_action_ids": action_refs("/decision_checks/consumables/use_actions"),
            "sell_action_ids": action_refs("/decision_checks/consumables/sell_actions"),
            "instruction": checks.pointer("/consumables/instruction"),
            "timing": checks.pointer("/consumables/timing")
        },
        "shop": {"required": checks.pointer("/shop/required")},
        "slots": checks.get("slots").cloned().unwrap_or(Value::Null),
        "boss_debuff": {
            "required": checks.pointer("/boss_debuff/required"),
            "current_blind": checks.pointer("/boss_debuff/current_blind"),
            "debuffed_cards": checks.pointer("/boss_debuff/debuffed_cards"),
            "debuffed_jokers": checks.pointer("/boss_debuff/debuffed_jokers"),
            "reroll_action_ids": action_refs("/decision_checks/boss_debuff/reroll_actions")
        }
    });
    let compact_hand_analysis = full
        .pointer("/hand_analysis/best_play")
        .map(|best_play| {
            json!({
                "best_play": {
                    "hand_name": best_play.get("hand_name"),
                    "card_indices": best_play.get("card_indices"),
                    "card_ids": best_play.get("card_ids"),
                    "cards": best_play.get("cards"),
                    "scoring_card_indices": best_play.get("scoring_card_indices"),
                    "estimated_score": best_play.get("estimated_score"),
                    "score_kind": best_play.get("score_kind"),
                    "estimate_quality": best_play.get("estimate_quality")
                }
            })
        })
        .unwrap_or_else(|| json!({}));
    json!({
        "schema": "balatro-mcp/policy-compact/v1",
        "game": {"state": full.pointer("/game/state"), "ante": run.get("ante"), "round": run.get("round"), "dollars": run.get("dollars"), "hands_left": run.get("hands_left"), "discards_left": run.get("discards_left"), "blind": blind},
        "score_pressure": full.get("score_pressure"),
        "hand_analysis": compact_hand_analysis,
        "slots": full.get("slots"),
        "hand": cards("hand"), "jokers": cards("jokers"), "consumables": cards("consumables"),
        "legal_actions": compact_actions("/legal_actions"),
        "active_directives": full.get("active_directives"),
        "decision_checks": compact_checks,
        "most_played_poker_hand": full.get("most_played_poker_hand"),
        "run_phase": full.get("run_phase"),
        "decision_id": full.get("decision_id"), "observation_id": full.get("observation_id"),
        "estimate_quality": full.get("estimate_quality")
    })
}

fn estimate_best_play(observation: &Value) -> i64 {
    let areas = observation
        .get("areas")
        .and_then(|a| a.as_object())
        .unwrap_or(&EMPTY_MAP);
    let hand_array = get_array(areas, "hand");
    if hand_array.is_empty() || observation.pointer("/poker_hands/values").is_none() {
        return 5;
    }
    best_play_subset(observation).1
}

fn determine_best_hand(
    card_counts: &std::collections::HashMap<&str, i64>,
    suits: &std::collections::HashMap<&str, i64>,
) -> String {
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
    } else if has_straight(card_counts) {
        "Straight".into()
    } else if max_suit_count >= 3 {
        "Flush".into()
    } else {
        "High Card".into()
    }
}

fn rank_value(rank: &str) -> Option<u8> {
    match rank.to_ascii_uppercase().as_str() {
        "A" | "ACE" => Some(14),
        "K" | "KING" => Some(13),
        "Q" | "QUEEN" => Some(12),
        "J" | "JACK" => Some(11),
        "T" | "10" => Some(10),
        value => value
            .parse()
            .ok()
            .filter(|value: &u8| (2..=9).contains(value)),
    }
}

fn has_straight(card_counts: &std::collections::HashMap<&str, i64>) -> bool {
    let mut ranks: Vec<u8> = card_counts
        .keys()
        .filter_map(|rank| rank_value(rank))
        .collect();
    ranks.sort_unstable();
    ranks.dedup();
    if ranks.len() < 5 {
        return false;
    }
    ranks.windows(5).any(|window| window[4] - window[0] == 4)
        || ranks.contains(&14)
            && ranks.contains(&2)
            && ranks.contains(&3)
            && ranks.contains(&4)
            && ranks.contains(&5)
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

fn analyze_hands(source: &Value) -> Value {
    let hand_array = source
        .pointer("/areas/hand")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if hand_array.is_empty() {
        return json!({});
    }
    let (indices, score) = best_play_subset(source);
    let analysis = score_hand(source, Some(&indices));
    let playable = analysis.hand_name != "Unknown" && analysis.estimate_quality != "hidden_cards";
    let card_indices: Vec<usize> = if playable {
        indices.iter().map(|index| index + 1).collect()
    } else {
        Vec::new()
    };
    let card_ids: Vec<Value> = if playable {
        indices
            .iter()
            .filter_map(|index| source.pointer("/areas/hand")?.get(*index))
            .map(card_id_value)
            .collect()
    } else {
        Vec::new()
    };
    let cards: Vec<Value> = if playable {
        indices
            .iter()
            .filter_map(|index| source.pointer("/areas/hand")?.get(*index))
            .enumerate()
            .map(|(position, card)| recommendation_card(card, indices[position]))
            .collect()
    } else {
        Vec::new()
    };
    let scoring_card_indices = if playable {
        analysis.scoring_cards.clone()
    } else {
        Vec::new()
    };
    json!({
        "best_play": {
            "hand_name": analysis.hand_name,
            "card_indices": card_indices,
            "card_ids": card_ids,
            "cards": cards,
            "scoring_card_indices": scoring_card_indices,
            "estimated_score": score,
            "score_kind": "estimate",
            "estimate_quality": analysis.estimate_quality
        }
    })
}

fn best_play_subset(observation: &Value) -> (Vec<usize>, i64) {
    let hand_len = observation
        .pointer("/areas/hand")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let max_cards = hand_len.min(5);
    let mut best = (Vec::new(), 0);
    for size in 1..=max_cards {
        let mut selected = Vec::with_capacity(size);
        collect_best_play(observation, size, 0, &mut selected, &mut best);
    }
    best
}

fn collect_best_play(
    observation: &Value,
    target_size: usize,
    start: usize,
    selected: &mut Vec<usize>,
    best: &mut (Vec<usize>, i64),
) {
    let hand_len = observation
        .pointer("/areas/hand")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if selected.len() == target_size {
        let score = score_hand(observation, Some(selected)).estimated_score;
        if score > best.1 || (score == best.1 && selected.len() > best.0.len()) {
            best.0 = selected.clone();
            best.1 = score;
        }
        return;
    }
    let remaining = target_size - selected.len();
    for index in start..=hand_len.saturating_sub(remaining) {
        selected.push(index);
        collect_best_play(observation, target_size, index + 1, selected, best);
        selected.pop();
    }
}

fn estimate_best_play_raw(hand_array: &[Value], poker_hands: &Value) -> i64 {
    if hand_array.is_empty() || poker_hands.pointer("/values").is_none() {
        return 5;
    }
    let observation = json!({
        "areas": {"hand": hand_array},
        "poker_hands": poker_hands
    });
    best_play_subset(&observation).1
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
        .and_then(|g| g.get("state_name"))
        .and_then(|s| s.as_str())
        .or_else(|| {
            observation
                .get("game")
                .and_then(|g| g.get("state"))
                .and_then(|s| s.as_str())
        })
        .unwrap_or("");
    let state = if state.eq_ignore_ascii_case("MAIN_MENU") {
        "MENU"
    } else {
        state
    };
    let run = observation
        .get("run")
        .or_else(|| observation.get("round"))
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
                let blind_states = run.get("blind_states").and_then(Value::as_object);
                if let Some(choices) = run.get("blind_choices").and_then(Value::as_object) {
                    for (name, choice) in choices {
                        let state = choice.get("state").and_then(Value::as_str).or_else(|| {
                            blind_states
                                .and_then(|states| states.get(name))
                                .and_then(Value::as_str)
                        });
                        if state == Some("Select") {
                            actions.push(json!({"action_id":format!("select_{}", name.to_ascii_lowercase()),"action":"select_blind","blind":name,"reason":format!("select {} blind", name)}));
                            actions.push(json!({"action_id":format!("skip_{}", name.to_ascii_lowercase()),"action":"skip_blind","blind":name,"reason":format!("skip {} blind", name)}));
                        }
                    }
                } else if let Some(states) = blind_states {
                    for (name, state) in states {
                        if state.as_str() == Some("Select") {
                            actions.push(json!({"action_id":format!("select_{}", name.to_ascii_lowercase()),"action":"select_blind","blind":name,"reason":format!("select {} blind", name)}));
                            actions.push(json!({"action_id":format!("skip_{}", name.to_ascii_lowercase()),"action":"skip_blind","blind":name,"reason":format!("skip {} blind", name)}));
                        }
                    }
                }
            }
            let hands_left = run.get("hands_left").and_then(|h| h.as_i64()).unwrap_or(0) as usize;
            if hands_left > 0 && hand_count > 0 {
                let hand_has_ids = hand_array.iter().all(|card| card_id(card).is_some());
                let max_play_cards = std::cmp::min(hand_count, play_limit).min(5);
                if hand_has_ids && max_play_cards > 0 {
                    actions.push(json!({
                        "action_id": "play_selected",
                        "action": "play",
                        "cards": [],
                        "card_indices": [],
                        "card_ids": [],
                        "selection": "caller_supplied",
                        "card_index_base": 1,
                        "max_cards": max_play_cards,
                        "reason": "select only the cards needed for the hand; any distinct 1-based positions are allowed"
                    }));
                }
            }
            let discards_left = run
                .get("discards_left")
                .and_then(|d| d.as_i64())
                .unwrap_or(0) as usize;
            if discards_left > 0 && hand_count > 1 {
                let hand_has_ids = hand_array.iter().all(|card| card_id(card).is_some());
                let max_discard_cards = std::cmp::min(hand_count - 1, discard_limit);
                if hand_has_ids && max_discard_cards > 0 {
                    actions.push(json!({
                        "action_id": "discard_selected",
                        "action": "discard",
                        "cards": [],
                        "card_indices": [],
                        "card_ids": [],
                        "selection": "caller_supplied",
                        "card_index_base": 1,
                        "max_cards": max_discard_cards,
                        "reason": "discard any distinct hand positions supplied in card_indices"
                    }));
                }
            }
        }
        "SHOP" => {
            if joker_open > 0 {
                if let Some(bc) = run.get("blind_choices") {
                    if bc
                        .get("Small")
                        .and_then(|small| small.get("state"))
                        .and_then(Value::as_str)
                        == Some("Select")
                    {
                        actions.push(json!({ "action_id": "select_small_blind", "action": "ui_click", "ui_id": "blind_select_small", "reason": "select Small Blind" }));
                    }
                    if bc
                        .get("Big")
                        .and_then(|big| big.get("state"))
                        .and_then(Value::as_str)
                        == Some("Select")
                    {
                        actions.push(json!({ "action_id": "select_big_blind", "action": "ui_click", "ui_id": "blind_select_big", "reason": "select Big Blind" }));
                    }
                }
            }
            if consumable_open > 0 || joker_open > 0 {
                let shop_areas = [
                    ("shop_jokers", "/areas/shop_jokers", joker_open > 0),
                    ("shop_vouchers", "/areas/shop_vouchers", true),
                    ("shop_booster", "/areas/shop_booster", true),
                    ("consumeables", "/areas/shop", consumable_open > 0),
                ];
                for (area, pointer, slot_available) in shop_areas {
                    if !slot_available {
                        continue;
                    }
                    let cards = observation
                        .pointer(pointer)
                        .and_then(Value::as_array)
                        .map(|v| v.as_slice())
                        .unwrap_or(&EMPTY_VEC);
                    for (i, c) in cards.iter().enumerate() {
                        if let Some(name) = c.get("name").and_then(|n| n.as_str()) {
                            actions.push(json!({ "action_id": format!("buy_{}_{}", area, i), "action": "buy_card", "area": area, "card_index": i + 1, "card_id": card_id_value(c), "reason": format!("buy {} (slot available)", name) }));
                        }
                    }
                }
            }
            if run.get("reroll_cost").and_then(Value::as_i64).unwrap_or(0) > 0 {
                actions.push(json!({"action_id":"reroll_shop","action":"reroll","reason":"reroll shop only when economy and score pressure justify it"}));
            }
            actions.push(json!({"action_id":"next_round","action":"next_round","reason":"leave the shop and continue to the next blind"}));
        }
        "TAROT_PACK"
        | "PLANET_PACK"
        | "SPECTRAL_PACK"
        | "STANDARD_PACK"
        | "BUFFOON_PACK"
        | "SMODS_BOOSTER_OPENED" => {
            let choices = observation
                .pointer("/areas/pack")
                .and_then(Value::as_array)
                .or_else(|| {
                    observation
                        .pointer("/areas/pack_cards")
                        .and_then(Value::as_array)
                })
                .or_else(|| observation.pointer("/pack/cards").and_then(Value::as_array))
                .map(|v| v.as_slice())
                .unwrap_or(&EMPTY_VEC);
            for (index, card) in choices.iter().enumerate().take(target_limit.max(1)) {
                let card_index = index + 1;
                actions.push(json!({"action_id":format!("pack_{}", card_index),"action":"choose_pack","card_index":card_index,"name":card.get("name")}));
            }
            actions.push(json!({"action_id":"skip_booster","action":"ui_click","button":"skip_booster","reason":"skip the opened booster pack"}));
        }
        "GAME_OVER" => {
            actions.push(json!({"action_id":"from_game_over","action":"ui_click","ui_id":"from_game_over","reason":"return to the main menu after game over"}));
            actions.push(json!({
                "action_id": "return_to_menu",
                "action": "safe_transition",
                "transition": "return_to_menu",
                "reason": "return to the main menu after game over"
            }));
        }
        _ => {}
    }

    // GAME_OVER is terminal: only the explicit recovery action above is
    // legal. Do not append shared gameplay actions below this point.
    if state == "GAME_OVER" {
        return actions;
    }

    for (index, card) in consumables_array.iter().enumerate() {
        let name = card
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("consumable");
        actions.push(json!({"action_id":format!("use_consumable_{}", index + 1),"action":"use_consumable","card_index":index + 1,"target_limit":target_limit,"target_index_base":1,"reason":format!("evaluate {} before any strategic action; consumables are usable during active play and transitions", name)}));
        actions.push(json!({"action_id":format!("sell_consumable_{}", index + 1),"action":"sell_card","area":"consumeables","card_index":index + 1,"reason":format!("sell {} if no useful target exists", name)}));
    }
    for (index, joker) in jokers_array.iter().enumerate() {
        actions.push(json!({"action_id":format!("sell_joker_{}", index + 1),"action":"sell_card","area":"jokers","card_index":index + 1,"reason":format!("sell {} when required", joker.get("name").and_then(Value::as_str).unwrap_or("Joker"))}));
    }
    match state {
        "ROUND_EVAL" => actions.push(json!({
            "action_id": "proceed_round",
            "action": "safe_transition",
            "transition": "cash_out",
            "reason": "proceed from the cleared blind evaluation to the shop"
        })),
        "GAME_OVER" => actions.push(json!({
            "action_id": "return_to_menu",
            "action": "safe_transition",
            "transition": "return_to_menu",
            "reason": "return to the main menu after game over"
        })),
        "" => {
            for transition in SAFE_TRANSITION_ACTIONS {
                actions.push(json!({"action_id":transition,"action":"safe_transition","transition":transition,"reason":"confirmed non-strategic transition"}));
            }
        }
        _ => {
            if observation
                .pointer("/ready/overlay_menu_present")
                .and_then(Value::as_bool)
                == Some(true)
            {
                actions.push(json!({"action_id":"dismiss_unlock_overlay","action":"safe_transition","transition":"dismiss_unlock_overlay","reason":"dismiss the active unlock overlay"}));
            }
            if observation
                .pointer("/ready/tutorial_complete")
                .and_then(Value::as_bool)
                == Some(false)
            {
                actions.push(json!({"action_id":"skip_tutorial","action":"safe_transition","transition":"skip_tutorial","reason":"skip the incomplete tutorial"}));
            }
        }
    }

    if joker_count > 1 {
        for i in 0..joker_count {
            for j in 0..joker_count {
                if i != j {
                    actions.push(json!({ "action_id": format!("move_joker_{}_to_{}", i + 1, j + 1), "action": "move_joker", "from_index": i + 1, "to_index": j + 1, "reason": "reorder joker trigger sequence" }));
                }
            }
        }
    }
    if hand_count > 1 {
        for i in 0..hand_count {
            for j in 0..hand_count {
                if i != j {
                    actions.push(json!({ "action_id": format!("move_card_{}_to_{}", i + 1, j + 1), "action": "move_card", "area": "hand", "from_index": i + 1, "to_index": j + 1, "reason": "reorder hand for play" }));
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
                    move_joker_actions.push(json!({ "action_id": format!("move_joker_{}_to_{}", i + 1, j + 1), "action": "move_joker", "from_index": i + 1, "to_index": j + 1 }));
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
    let is_boss = blind
        .get("boss")
        .and_then(|boss| {
            boss.as_bool().or_else(|| {
                boss.as_str().map(|value| {
                    value.eq_ignore_ascii_case("true")
                        || value.eq_ignore_ascii_case("boss")
                        || value == "1"
                })
            })
        })
        .unwrap_or(false);
    let boss_effect = blind
        .get("effect")
        .and_then(|e| e.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");
    json!({
        "ordering": { "required_before_close_play": joker_count > 1, "hand_order": hand_order, "joker_order": joker_order, "move_card_actions": move_card_actions, "move_joker_actions": move_joker_actions, "instruction": "Evaluate hand and Joker trigger order when a scoring effect can depend on sequence; do not move cards by default, but do not dismiss legal reorder actions.", "estimate_caveat": "Play estimates may not model every ordering interaction; verify relevant ordering when margin is tight." },
        "consumables": { "required": !owned_consumables.is_empty(), "owned": owned_consumables, "use_actions": use_actions, "sell_actions": sell_actions, "shop_purchase_actions": legal_actions.iter().filter(|action| action.get("action").and_then(Value::as_str) == Some("buy_card")).cloned().collect::<Vec<_>>(), "timing": ["SELECTING_HAND", "BLIND_SELECT", "ROUND_EVAL", "SHOP", "other non-terminal states"], "instruction": "Evaluate every owned use/sell action before every strategic action. Consumables may be used during active hand play, before discarding, during round evaluation, blind selection, and shop decisions; only defer one deliberately after checking its effect, targets, score pressure, and upcoming blind." },
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
    fn compact_policy_state_preserves_actions_and_reduces_payload() {
        let full = build_policy_state(&observation("SELECTING_HAND"), 40, 40, 60);
        let compact = compact_policy_state(&full);
        let full_bytes = serde_json::to_vec(&full).unwrap().len();
        let compact_bytes = serde_json::to_vec(&compact).unwrap().len();
        assert!(
            compact_bytes < full_bytes,
            "compact={compact_bytes} full={full_bytes}"
        );
        assert!(
            compact_bytes < full_bytes,
            "compact={compact_bytes} full={full_bytes}"
        );
        let compact_ids: Vec<_> = compact["legal_actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["action_id"].clone())
            .collect();
        let full_ids: Vec<_> = full["legal_actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["action_id"].clone())
            .collect();
        assert_eq!(compact_ids, full_ids);
        assert_eq!(
            compact["decision_id"],
            full.get("decision_id").cloned().unwrap_or(Value::Null)
        );
        assert!(compact["decision_checks"].get("ordering").is_some());
        assert!(compact["decision_checks"].get("consumables").is_some());
    }

    #[test]
    fn hidden_cards_do_not_drive_best_play_or_compact_identity() {
        let mut hidden = observation("SELECTING_HAND");
        hidden["run"]["blind"]["chips_required"] = json!(300);
        hidden["areas"]["hand"] = json!([
            {"index": 1, "instance_id": 101, "base": {"nominal": 11, "suit": "Diamonds"}},
            {"index": 2, "instance_id": 102, "base": {"nominal": 10, "suit": "Spades"}},
            {"index": 3, "instance_id": 103, "base": {"nominal": 10, "suit": "Hearts"}},
            {"index": 4, "instance_id": 104, "base": {"nominal": 10, "suit": "Diamonds"}},
            {"index": 5, "instance_id": 105, "base": {"nominal": 10, "suit": "Clubs"}}
        ]);

        let state = build_policy_state(&hidden, 5, 5, 60);
        assert_eq!(state["score_pressure"]["best_play_estimated_score"], 0);
        assert_eq!(state["score_pressure"]["best_play_clears_blind"], false);
        assert_eq!(state["hand_analysis"]["best_play"]["hand_name"], "Unknown");
        assert_eq!(
            state["hand_analysis"]["best_play"]["card_indices"],
            json!([])
        );
        assert_eq!(state["hand_analysis"]["best_play"]["card_ids"], json!([]));
        assert_eq!(state["hand_analysis"]["best_play"]["cards"], json!([]));

        let compact = compact_policy_state(&state);
        assert_eq!(compact["hand"][0]["hidden"], true);
        assert!(compact["hand"][0].get("base").is_none());
        assert!(compact["hand"][0].get("suit").is_none());
    }

    #[test]
    fn best_play_mapping_matches_exact_eight_card_hand() {
        let mut hand = Vec::new();
        for (index, (instance_id, rank, suit, nominal)) in [
            ("a", "A", "Clubs", 11),
            ("b", "8", "Spades", 8),
            ("c", "8", "Hearts", 8),
            ("d", "7", "Hearts", 7),
            ("e", "6", "Hearts", 6),
            ("f", "4", "Hearts", 4),
            ("g", "3", "Spades", 3),
            ("h", "2", "Spades", 2),
        ]
        .into_iter()
        .enumerate()
        {
            hand.push(json!({
                "index": index + 1,
                "instance_id": instance_id,
                "base": {"rank": rank, "value": rank, "id": rank, "suit": suit, "nominal": nominal}
            }));
        }
        let observation = json!({
            "game": {"state": "SELECTING_HAND"},
            "run": {"hands_left": 3, "discards_left": 2, "blind": {"chips_required": 300}},
            "areas": {"hand": hand},
            "poker_hands": {"values": {
                "High Card": {"chips": 5, "mult": 1},
                "Pair": {"chips": 20, "mult": 2},
                "Flush": {"chips": 35, "mult": 4}
            }}
        });

        let state = build_policy_state(&observation, 40, 40, 60);
        let best = &state["hand_analysis"]["best_play"];
        assert_eq!(best["hand_name"], "Pair");
        assert_ne!(best["hand_name"], "Flush");

        let indices = best["card_indices"].as_array().unwrap();
        let ids = best["card_ids"].as_array().unwrap();
        let cards = best["cards"].as_array().unwrap();
        assert_eq!(indices.len(), ids.len());
        assert_eq!(indices.len(), cards.len());
        assert!(!indices.is_empty());
        for (position, index) in indices.iter().enumerate() {
            let index = index.as_u64().unwrap() as usize;
            assert!((1..=8).contains(&index));
            let expected = &observation["areas"]["hand"][index - 1];
            assert_eq!(ids[position], expected["instance_id"]);
            assert_eq!(cards[position]["index"], json!(index));
            assert_eq!(cards[position]["instance_id"], expected["instance_id"]);
        }
        for index in best["scoring_card_indices"].as_array().unwrap() {
            assert!(indices.contains(index));
        }

        let compact = compact_policy_state(&state);
        assert_eq!(
            compact["hand_analysis"]["best_play"]["card_indices"],
            best["card_indices"]
        );
        assert_eq!(
            compact["hand_analysis"]["best_play"]["card_ids"],
            best["card_ids"]
        );
    }

    #[test]
    fn compact_visible_cards_keep_authoritative_identity_fields() {
        let mut source = observation("SELECTING_HAND");
        source["areas"]["hand"][0] = json!({
            "index": 1,
            "instance_id": 101,
            "base": {"rank": "A", "value": "A", "id": "A", "suit": "Clubs", "nominal": 11}
        });
        let state = build_policy_state(&source, 5, 5, 60);
        let compact = compact_policy_state(&state);
        assert_eq!(compact["hand"][0]["base"]["rank"], "A");
        assert_eq!(compact["hand"][0]["base"]["value"], "A");
        assert_eq!(compact["hand"][0]["base"]["id"], "A");
        assert_ne!(compact["hand"][0].get("hidden"), Some(&Value::Bool(true)));
    }

    #[test]
    fn policy_state_covers_play_discard_ordering_and_checks() {
        let state = build_policy_state(&observation("SELECTING_HAND"), 2, 1, 60);
        assert_eq!(state["legal_actions"][0]["action"], "play");
        let mut discard_observation = observation("SELECTING_HAND");
        discard_observation["run"]["discards_used"] = json!(1);
        discard_observation["run"]["starting_params"] = json!({"discard_limit": 5});
        let discard_status =
            build_policy_state(&discard_observation, 2, 1, 60)["discard_status"].clone();
        assert_eq!(discard_status["remaining"], 1);
        assert_eq!(discard_status["used"], 1);
        assert_eq!(discard_status["current_limit"], 4);
        assert_eq!(discard_status["configured_limit"], 5);
        let play_selected = state["legal_actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|action| action["action_id"] == "play_selected")
            .unwrap();
        assert_eq!(play_selected["selection"], "caller_supplied");
        assert_eq!(play_selected["card_index_base"], 1);
        assert_eq!(play_selected["max_cards"], 2);
        assert!(
            state["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|action| {
                    let action_id = action["action_id"].as_str().unwrap_or("");
                    action_id == "play_selected" || !action_id.starts_with("play_")
                })
        );
        assert!(
            state["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action"] == "discard")
        );
        let discard_selected = state["legal_actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|action| action["action_id"] == "discard_selected")
            .unwrap();
        assert_eq!(discard_selected["selection"], "caller_supplied");
        assert_eq!(discard_selected["max_cards"], 1);
        assert!(
            state["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|action| action["action_id"] != "discard_d")
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
        assert!(
            state["decision_checks"]["consumables"]["timing"]
                .as_array()
                .unwrap()
                .iter()
                .any(|state| state == "SELECTING_HAND")
        );
        assert!(
            state["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|action| action["action"] == "use_consumable")
                .is_some()
        );
        assert_eq!(
            state["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|action| action["action"] == "use_consumable")
                .unwrap()["target_index_base"],
            1
        );
        assert_eq!(state["decision_checks"]["boss_debuff"]["required"], true);
        assert!(
            state["score_pressure"]["best_play_estimated_score"]
                .as_i64()
                .unwrap()
                > 0
        );
    }

    #[test]
    fn consumables_are_legal_across_non_terminal_gameplay_states() {
        for state_name in ["SELECTING_HAND", "BLIND_SELECT", "ROUND_EVAL", "SHOP"] {
            let state = build_policy_state(&observation(state_name), 40, 40, 60);
            assert!(
                state["legal_actions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|action| action["action"] == "use_consumable"),
                "missing consumable action in {state_name}"
            );
        }
    }

    #[test]
    fn policy_does_not_classify_regular_blind_effects_as_bosses() {
        let mut state = observation("SELECTING_HAND");
        state["run"]["blind"] = json!({
            "name": "Small Blind",
            "boss": false,
            "effect": {"name": "Small Blind"}
        });
        let checks = build_policy_state(&state, 40, 40, 60)["decision_checks"].clone();
        assert_eq!(checks["boss_debuff"]["required"], false);
        assert_eq!(checks["boss_debuff"]["current_blind"]["boss"], false);
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
        let numeric_main_menu = build_policy_state(
            &json!({
                "game":{"state":11,"state_name":"MAIN_MENU"},
                "ready":{"saved_game_present":false}
            }),
            40,
            40,
            60,
        );
        assert_eq!(numeric_main_menu["legal_actions"][0]["action"], "ui_click");
        let blind_select = build_policy_state(
            &json!({
                "game":{"state":"BLIND_SELECT"},
                "round":{"blind_choices":{"Small":{"state":"Select"},"Big":{"state":"Upcoming"}}}
            }),
            40,
            40,
            60,
        );
        assert_eq!(blind_select["legal_actions"][0]["action"], "select_blind");
        assert_eq!(blind_select["legal_actions"][0]["blind"], "Small");
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
                .any(|a| a["action"] == "safe_transition")
        );
        assert!(actions.as_array().unwrap().iter().all(|action| {
            matches!(
                action["action_id"].as_str(),
                Some("from_game_over") | Some("return_to_menu")
            )
        }));

        let mut numeric_game_over = observation("not-a-state-name");
        numeric_game_over["game"] = json!({"state": 4, "state_name": "GAME_OVER"});
        let recovery_state = build_policy_state(&numeric_game_over, 40, 40, 60);
        let recovery = recovery_state["legal_actions"].as_array().unwrap();
        assert!(recovery.iter().any(|a| a["action_id"] == "from_game_over"));
        assert!(recovery.iter().any(|a| a["action_id"] == "return_to_menu"));

        let round_eval = build_policy_state(&observation("ROUND_EVAL"), 40, 40, 60);
        assert!(
            round_eval["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action_id"] == "proceed_round" && a["transition"] == "cash_out")
        );
        assert!(
            !round_eval["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action_id"] == "dismiss_unlock_overlay")
        );

        let shop = build_policy_state(&observation("SHOP"), 40, 40, 60);
        assert!(
            shop["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action_id"] == "next_round")
        );
    }

    #[test]
    fn policy_reports_current_chips_and_boss_requirement_separately() {
        let observation = json!({
            "game": {"state": "SELECTING_HAND"},
            "round": {
                "chips": 1200,
                "hands_left": 1,
                "discards_left": 0
            },
            "blind": {"name": "The Wall", "boss": true, "chips": 1600},
            "areas": {"hand": []}
        });
        let state = build_policy_state(&observation, 40, 40, 60);
        assert_eq!(state["score_pressure"]["blind_chips_required"], 1600);
        assert_eq!(state["score_pressure"]["current_chips"], 1200);
        assert_eq!(state["score_pressure"]["chips_remaining"], 400);
        assert_eq!(state["score_pressure"]["best_play_clears_blind"], false);
        assert_eq!(state["run"]["blind"]["chips_required"], 1600);

        let mut cleared = observation.clone();
        cleared["round"]["chips"] = json!(1600);
        assert_eq!(
            build_policy_state(&cleared, 40, 40, 60)["score_pressure"]["chips_remaining"],
            0
        );
    }

    #[test]
    fn policy_reads_legacy_consumeables_area_and_exposes_all_cards() {
        let observation = json!({
            "game": {"state": "SELECTING_HAND"},
            "run": {"hands_left": 1, "discards_left": 0},
            "areas": {"consumeables": [
                {"name":"Neptune","set":"Planet"},
                {"name":"Venus","set":"Planet"},
                {"name":"Neptune","set":"Planet","edition":"negative"},
                {"name":"Neptune","set":"Planet","edition":"negative"},
                {"name":"Neptune","set":"Planet","edition":"negative"}
            ]}
        });
        let state = build_policy_state(&observation, 40, 40, 60);
        assert_eq!(state["slots"]["consumables"]["count"], 5);
        assert_eq!(
            state["decision_checks"]["consumables"]["owned"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        assert_eq!(
            state["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|a| a["action"] == "use_consumable")
                .count(),
            5
        );
        assert_eq!(
            state["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|a| a["action"] == "sell_card")
                .count(),
            5
        );
        let compact = compact_policy_state(&state);
        assert_eq!(compact["consumables"].as_array().unwrap().len(), 5);
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
                "High Card",
            ),
            (
                vec![("A", 1), ("K", 1), ("Q", 1), ("J", 1), ("9", 1)],
                vec![],
                "High Card",
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
        assert_eq!(
            estimate_best_play(&json!({
                "areas":{"hand":[{"suits":[{"key":"H"}, {}]}]},
                "poker_hands":{"values":{}}
            })),
            5
        );
        assert_eq!(estimate_best_play_raw(&[], &json!({})), 5);
        assert_eq!(
            estimate_best_play_raw(&[json!({"suits":[{"key":"H"}]})], &json!({})),
            5
        );
        assert_eq!(
            estimate_best_play(&json!({
                "areas":{"hand":[
                    {"base":{"rank":"A"}}, {"base":{"rank":"K"}},
                    {"base":{"rank":"Q"}}, {"base":{"rank":"J"}},
                    {"base":{"rank":"9"}}
                ]},
                "poker_hands":{"values":{
                    "High Card":{"chips":10,"mult":2},
                    "Straight":{"chips":100,"mult":100}
                }}
            })),
            120
        );
        assert_eq!(analyze_hands(&json!({"areas":{"hand":[]}})), json!({}));
        assert!(
            analyze_hands(&json!({
                "areas":{"hand":[json!({"base":{"rank":"A"},"suits":[{"key":"H"}]})]},
                "poker_hands":{"values":{"High Card":{"chips":7,"mult":3}}}
            }))["best_play"]["estimated_score"]
                .as_i64()
                .unwrap()
                > 0
        );
        assert!(
            analyze_hands(&json!({
                "areas":{"hand":[{"base":{"rank":"A"},"suits":[{}]}]},
                "poker_hands":{"values":{}}
            }))["best_play"]["estimated_score"]
                .as_i64()
                .is_some()
        );

        let mut no_moves = observation("SELECTING_HAND");
        no_moves["run"]["hands_left"] = json!(0);
        no_moves["run"]["discards_left"] = json!(0);
        let no_moves_state = build_policy_state(&no_moves, 40, 40, 60);
        assert!(
            no_moves_state["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|action| action["action"] != "play" && action["action"] != "discard")
        );

        let mut shop_without_blinds = observation("SHOP");
        shop_without_blinds["run"]
            .as_object_mut()
            .unwrap()
            .remove("blind_choices");
        shop_without_blinds["run"]["joker_slots"] = json!(5);
        assert!(
            build_policy_state(&shop_without_blinds, 40, 40, 60)["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|action| action.get("action") != Some(&json!("ui_click")))
        );

        let mut boss_details = observation("SELECTING_HAND");
        boss_details["run"]["blind"]["effect"] = json!({"name":"No face cards"});
        boss_details["run"]["blind"]["disabled"] = json!("true");
        boss_details["run"]["blind"]["state"] = json!("Upcoming");
        boss_details["areas"]["debuffed_cards"] = json!([{"instance_id":"a"}]);
        boss_details["areas"]["debuffed_jokers"] = json!([{"name":"Joker"}]);
        let checks = build_policy_state(&boss_details, 1, 1, 1)["decision_checks"].clone();
        assert_eq!(
            checks["boss_debuff"]["current_blind"]["effect"],
            "No face cards"
        );
        assert_eq!(
            checks["boss_debuff"]["debuffed_cards"][0]["instance_id"],
            "a"
        );

        let shop_blind_choices = json!({
            "game":{"state":"SHOP"},
            "run":{"joker_slots":1,"blind_choices":{"Small":{"state":"Select"},"Big":{"state":"Select"}}},
            "areas":{"jokers":[]}
        });
        let shop_actions =
            build_policy_state(&shop_blind_choices, 0, 0, 1)["legal_actions"].clone();
        assert!(
            shop_actions
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action_id"] == "select_small_blind")
        );
        assert!(
            shop_actions
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["action_id"] == "select_big_blind")
        );
        let shop_blind_skipped = json!({
            "game":{"state":"SHOP"},
            "run":{"joker_slots":1,"blind_choices":{"Small":{"state":"Skip"},"Big":{"state":"Skip"}}},
            "areas":{"jokers":[]}
        });
        let skipped_actions =
            build_policy_state(&shop_blind_skipped, 0, 0, 1)["legal_actions"].clone();
        assert!(
            skipped_actions
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["action_id"] != "select_small_blind"
                    && a["action_id"] != "select_big_blind")
        );
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
        obs["run"]["most_played_poker_hand"] = json!("Pair");
        obs["run"]["blind"]["effect"] = json!({});
        obs["areas"]["shop"] = json!([{"name":"Joker"}, {}]);
        obs["areas"]["jokers"] = json!([{"empty":true}]);
        let populated = build_policy_state(&obs, 1, 1, 1);
        assert_eq!(populated["most_played_poker_hand"], "Pair");

        let mut pack_fallback = observation("TAROT_PACK");
        pack_fallback["areas"]
            .as_object_mut()
            .unwrap()
            .remove("pack");
        pack_fallback["pack"] = json!({"cards":[{"name":"Fallback"}]});
        assert!(
            build_policy_state(&pack_fallback, 1, 1, 1)["legal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["action"] == "choose_pack")
        );

        let smods_pack = json!({
            "game": {"state": 999, "state_name": "SMODS_BOOSTER_OPENED"},
            "areas": {"pack_cards": [
                {"name":"one"}, {"name":"two"}, {"name":"three"},
                {"name":"four"}, {"name":"five"}
            ]}
        });
        let smods_actions = build_policy_state(&smods_pack, 40, 40, 40)["legal_actions"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(smods_actions.len(), 6);
        assert_eq!(smods_actions[0]["action_id"], "pack_1");
        assert_eq!(smods_actions[0]["card_index"], 1);
        assert_eq!(smods_actions[4]["action_id"], "pack_5");
        assert_eq!(smods_actions[4]["card_index"], 5);
        assert_eq!(smods_actions[5]["button"], "skip_booster");

        let unknown = json!({
            "game": {"state": 1000, "state_name": "UNKNOWN_STATE"},
            "areas": {"pack_cards": [{"name":"hidden"}]}
        });
        assert!(
            build_policy_state(&unknown, 40, 40, 40)["legal_actions"]
                .as_array()
                .unwrap()
                .is_empty()
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
