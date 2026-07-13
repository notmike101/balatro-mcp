use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::LazyLock;

use super::observation;

static EMPTY_OBJECT: LazyLock<serde_json::Map<String, Value>> = LazyLock::new(serde_json::Map::new);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreResult {
    pub hand_name: String,
    pub hand_key: String,
    pub score_scope: String,
    pub run_most_played_hand: Option<String>,
    pub run_chips: Option<i64>,
    pub blind_chips_required: Option<i64>,
    pub blind_chips_remaining: Option<i64>,
    pub scoring_cards: Vec<usize>,
    pub chips: i64,
    pub mult: i64,
    pub x_mult: f64,
    pub exact_score: Option<i64>,
    pub estimated_score: i64,
    pub estimate_quality: String,
    pub unsupported_effects: Vec<String>,
    pub contributions: Vec<Value>,
}

fn rank_value(rank: &str) -> Option<u8> {
    match rank.to_ascii_uppercase().as_str() {
        "A" | "ACE" => Some(14),
        "K" | "KING" => Some(13),
        "Q" | "QUEEN" => Some(12),
        "J" | "JACK" => Some(11),
        "T" | "10" => Some(10),
        value => value.parse().ok().filter(|v: &u8| (2..=14).contains(v)),
    }
}

fn card_rank(card: &Value) -> Option<u8> {
    card.pointer("/base/rank")
        .or_else(|| card.pointer("/base/value"))
        .or_else(|| card.pointer("/base/id"))
        .or_else(|| card.get("rank"))
        .and_then(Value::as_str)
        .and_then(rank_value)
}

fn card_suit(card: &Value) -> Option<String> {
    card.pointer("/base/suit")
        .or_else(|| card.get("suit"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            card.get("suits")
                .and_then(Value::as_array)
                .and_then(|suits| suits.first())
                .and_then(|suit| suit.get("key").or_else(|| suit.get("name")))
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
        })
}

fn card_nominal(card: &Value) -> Option<i64> {
    card.pointer("/base/nominal")
        .and_then(Value::as_i64)
        .or_else(|| {
            card_rank(card).map(|rank| match rank {
                14 => 11,
                11..=13 => 10,
                value => i64::from(value),
            })
        })
}

fn scoring_card_positions(cards: &[Value], hand_name: &str) -> Vec<usize> {
    let grouped_hand = matches!(
        hand_name,
        "Pair"
            | "Two Pair"
            | "Three of a Kind"
            | "Full House"
            | "Four of a Kind"
            | "Five of a Kind"
    );
    if !grouped_hand {
        return (0..cards.len()).collect();
    }

    let mut counts = std::collections::HashMap::<u8, usize>::new();
    for card in cards {
        let Some(rank) = card_rank(card) else {
            return (0..cards.len()).collect();
        };
        *counts.entry(rank).or_default() += 1;
    }
    let minimum = match hand_name {
        "Pair" | "Two Pair" => 2,
        "Three of a Kind" => 3,
        "Full House" => 2,
        "Four of a Kind" => 4,
        "Five of a Kind" => 5,
        _ => unreachable!(),
    };
    cards
        .iter()
        .enumerate()
        .filter_map(|(position, card)| {
            card_rank(card)
                .filter(|rank| counts.get(rank).copied().unwrap_or(0) >= minimum)
                .map(|_| position)
        })
        .collect()
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| {
            v.get("name")
                .or_else(|| v.get("text"))
                .or_else(|| v.get("key"))
        })
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| value.get(key).and_then(Value::as_str).map(str::to_owned))
}

fn straight(ranks: &[u8]) -> bool {
    let mut values = ranks.to_vec();
    values.sort_unstable();
    values.dedup();
    if values.len() < 5 {
        return false;
    }
    values.windows(5).any(|window| window[4] - window[0] == 4)
        || values.contains(&14)
            && values.contains(&2)
            && values.contains(&3)
            && values.contains(&4)
            && values.contains(&5)
}

pub fn classify_hand(cards: &[Value]) -> String {
    let ranks: Vec<u8> = cards.iter().filter_map(card_rank).collect();
    let suits: Vec<String> = cards.iter().filter_map(card_suit).collect();
    let mut counts = HashMap::<u8, usize>::new();
    for rank in &ranks {
        *counts.entry(*rank).or_default() += 1;
    }
    let mut groups: Vec<usize> = counts.values().copied().collect();
    groups.sort_unstable_by(|a, b| b.cmp(a));
    let flush = suits.len() >= 5 && suits.iter().all(|suit| suit == &suits[0]);
    let straight = straight(&ranks);
    match (flush, straight, groups.as_slice()) {
        (true, true, _) => "Straight Flush",
        (_, _, [5, ..]) => "Five of a Kind",
        (_, _, [4, ..]) => "Four of a Kind",
        (_, _, [3, 2, ..]) => "Full House",
        (true, _, _) => "Flush",
        (_, true, _) => "Straight",
        (_, _, [3, ..]) => "Three of a Kind",
        (_, _, [2, 2, ..]) => "Two Pair",
        (_, _, [2, ..]) => "Pair",
        _ => "High Card",
    }
    .to_owned()
}

fn hand_value(contract: &Value, hand: &str) -> Option<(i64, i64)> {
    let values = contract.get("values").unwrap_or(contract);
    let entry = match values.get(hand) {
        Some(entry) => entry,
        None => values.get("High Card")?,
    };
    let chips = entry.get("chips")?.as_i64()?;
    let mult = entry.get("mult")?.as_i64()?;
    Some((chips, mult))
}

fn add_modifier(result: &mut ScoreResult, card: &Value, index: usize) {
    if let Some(edition) = text_field(card, "edition") {
        match edition.to_ascii_lowercase().as_str() {
            "foil" => {
                result.chips += 50;
                result
                    .contributions
                    .push(json!({"source": "card", "index": index, "effect": "foil", "chips": 50}));
            }
            "holographic" | "holo" => {
                result.mult += 10;
                result.contributions.push(
                    json!({"source": "card", "index": index, "effect": "holographic", "mult": 10}),
                );
            }
            "polychrome" => {
                result.x_mult *= 1.5;
                result.contributions.push(json!({"source": "card", "index": index, "effect": "polychrome", "x_mult": 1.5}));
            }
            "negative" => {}
            other => result
                .unsupported_effects
                .push(format!("card edition: {other}")),
        }
    }
    if let Some(enhancement) = text_field(card, "enhancement") {
        match enhancement.to_ascii_lowercase().as_str() {
            "bonus" => result.chips += 30,
            "mult" => result.mult += 4,
            "glass" => result.x_mult *= 2.0,
            "steel" => result.x_mult *= 1.5,
            "wild" | "stone" | "gold" | "lucky" => {}
            other => result
                .unsupported_effects
                .push(format!("card enhancement: {other}")),
        }
    }
}

pub fn score_hand(observation: &Value, card_indices: Option<&[usize]>) -> ScoreResult {
    let hand = observation
        .pointer("/areas/hand")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let highlighted_indices: Vec<usize> = observation
        .pointer("/areas/hand_highlighted")
        .and_then(Value::as_array)
        .map(|indices| {
            indices
                .iter()
                .filter_map(Value::as_u64)
                .filter_map(|index| index.checked_sub(1))
                .map(|index| index as usize)
                .filter(|index| *index < hand.len())
                .collect()
        })
        .unwrap_or_default();
    let highlighted_scope = card_indices.is_none() && !highlighted_indices.is_empty();
    let contract = observation.get("poker_hands").unwrap_or(&Value::Null);
    let best_subset_scope = card_indices.is_none() && !highlighted_scope && hand.len() > 5;
    let indices: Vec<usize> = card_indices.map(ToOwned::to_owned).unwrap_or_else(|| {
        if highlighted_scope {
            highlighted_indices.clone()
        } else if best_subset_scope {
            best_subset_indices(&hand, contract)
        } else {
            (0..hand.len()).collect()
        }
    });
    let cards: Vec<Value> = indices
        .iter()
        .filter_map(|index| hand.get(*index).cloned())
        .collect();
    let hand_name = classify_hand(&cards);
    let scoring_positions = scoring_card_positions(&cards, &hand_name);
    let base = hand_value(contract, &hand_name);
    let (base_chips, mut mult) = base.unwrap_or((5, 1));
    let run = observation.get("run").or_else(|| observation.get("round"));
    let run_most_played_hand = run
        .and_then(|run| run.get("most_played_poker_hand"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let run_chips = observation::current_chips(observation);
    let blind_chips_required = observation::blind_chips_required(observation);
    let blind_chips_remaining =
        blind_chips_required.map(|required| required.saturating_sub(run_chips.unwrap_or(0)).max(0));
    let mut result = ScoreResult {
        hand_key: hand_name.clone(),
        hand_name,
        score_scope: if card_indices.is_some() || highlighted_scope {
            "selected_cards".into()
        } else if best_subset_scope {
            "best_play".into()
        } else {
            "current_hand".into()
        },
        run_most_played_hand,
        run_chips,
        blind_chips_required,
        blind_chips_remaining,
        scoring_cards: scoring_positions
            .iter()
            .map(|position| indices[*position] + 1)
            .collect(),
        chips: base_chips,
        mult,
        x_mult: 1.0,
        exact_score: None,
        estimated_score: 0,
        estimate_quality: "estimate".into(),
        unsupported_effects: Vec::new(),
        contributions: Vec::new(),
    };
    for position in scoring_positions {
        let card = &cards[position];
        if let Some(nominal) = card_nominal(card) {
            result.chips += nominal;
        }
        let original_index = indices[position];
        add_modifier(&mut result, card, original_index);
    }
    let mut chips = result.chips;
    if let Some(jokers) = observation
        .pointer("/areas/jokers")
        .and_then(Value::as_array)
    {
        for joker in jokers {
            let ability = joker
                .get("ability")
                .and_then(Value::as_object)
                .unwrap_or(&EMPTY_OBJECT);
            let name = joker.get("name").and_then(Value::as_str).unwrap_or("joker");
            let mut known = false;
            if let Some(value) = ability.get("chips").and_then(Value::as_i64) {
                chips += value;
                known = true;
            }
            if let Some(value) = ability.get("mult").and_then(Value::as_i64) {
                mult += value;
                known = true;
            }
            if let Some(value) = ability.get("x_mult").and_then(Value::as_f64) {
                result.x_mult *= value;
                known = true;
            }
            let center_key = joker
                .get("center_key")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let normalized_name = name.to_ascii_lowercase();
            let is = |key: &str, label: &str| center_key == key || normalized_name == label;
            if is("j_banner", "banner") {
                let extra = ability.get("extra").and_then(Value::as_i64);
                let extra = match extra {
                    Some(extra) => extra,
                    None => joker
                        .pointer("/config/extra")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                };
                let discards = observation
                    .pointer("/run/discards_left")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                chips += extra * discards;
                known = true;
            } else if is("j_raised_fist", "raised fist") {
                if let Some(min_rank) = hand.iter().filter_map(card_rank).min() {
                    mult += i64::from(min_rank) * 2;
                }
                known = true;
            } else if is("j_mystic_summit", "mystic summit") {
                if observation
                    .pointer("/run/discards_left")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    == 0
                {
                    mult += 15;
                }
                known = true;
            } else if is("j_hanging_chad", "hanging chad") {
                let extra = ability.get("extra").and_then(Value::as_i64);
                let extra = match extra {
                    Some(extra) => extra,
                    None => joker
                        .pointer("/config/extra")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                };
                let nominal = hand
                    .first()
                    .and_then(|card| card.pointer("/base/nominal"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                chips += extra * nominal;
                known = true;
            } else if is("j_vampire", "vampire") {
                let x = ability.get("x_mult").and_then(Value::as_f64);
                let x = match x {
                    Some(x) => x,
                    None => joker
                        .pointer("/config/Xmult")
                        .and_then(Value::as_f64)
                        .unwrap_or(1.0),
                };
                result.x_mult *= x.max(1.0);
                known = true;
            } else if is("j_triboulet", "triboulet") {
                let faces = hand
                    .iter()
                    .filter_map(card_rank)
                    .filter(|rank| *rank == 12 || *rank == 13)
                    .count();
                result.x_mult *= 2f64.powi(faces as i32);
                known = true;
            }
            if !known && !ability.is_empty() {
                result.unsupported_effects.push(name.to_owned());
            }
        }
    }
    result.chips = chips;
    result.mult = mult;
    result.estimated_score = ((chips as f64) * (mult as f64) * result.x_mult).round() as i64;
    if base.is_some() && result.unsupported_effects.is_empty() {
        result.exact_score = Some(result.estimated_score);
        result.estimate_quality = "exact_contract_plus_supported_modifiers".into();
    } else if base.is_some() {
        result.estimate_quality = "partial_contract".into();
    } else {
        result
            .unsupported_effects
            .push("missing poker_hands contract".into());
    }
    result
}

fn best_subset_indices(hand: &[Value], contract: &Value) -> Vec<usize> {
    let max_cards = hand.len().min(5);
    let mut best: Option<(usize, i64, Vec<usize>)> = None;
    for size in 1..=max_cards {
        let mut selected = Vec::with_capacity(size);
        collect_best_subset(hand, contract, size, 0, &mut selected, &mut best);
    }
    best.map(|(_, _, indices)| indices)
        .unwrap_or_else(|| (0..hand.len()).collect())
}

fn collect_best_subset(
    hand: &[Value],
    contract: &Value,
    target_size: usize,
    start: usize,
    selected: &mut Vec<usize>,
    best: &mut Option<(usize, i64, Vec<usize>)>,
) {
    if selected.len() == target_size {
        let cards: Vec<Value> = selected
            .iter()
            .filter_map(|index| hand.get(*index).cloned())
            .collect();
        let hand_name = classify_hand(&cards);
        let hand_rank = [
            "High Card",
            "Pair",
            "Two Pair",
            "Three of a Kind",
            "Straight",
            "Flush",
            "Full House",
            "Four of a Kind",
            "Straight Flush",
            "Five of a Kind",
            "Flush House",
            "Flush Five",
        ]
        .iter()
        .position(|name| *name == hand_name)
        .unwrap_or(0);
        let base_score = hand_value(contract, &hand_name)
            .map(|(chips, mult)| chips.saturating_mul(mult))
            .unwrap_or(0);
        let should_replace = best.as_ref().is_none_or(|(rank, score, indices)| {
            hand_rank > *rank
                || (hand_rank == *rank
                    && (base_score > *score
                        || (base_score == *score && selected.len() > indices.len())))
        });
        if should_replace {
            *best = Some((hand_rank, base_score, selected.clone()));
        }
        return;
    }
    let remaining = target_size - selected.len();
    for index in start..=hand.len().saturating_sub(remaining) {
        selected.push(index);
        collect_best_subset(hand, contract, target_size, index + 1, selected, best);
        selected.pop();
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn card(rank: &str, suit: &str) -> Value {
        json!({"base":{"rank":rank,"suit":suit},"suits":[{"key":suit}]})
    }

    #[test]
    fn classifies_core_poker_hands() {
        assert_eq!(classify_hand(&[card("A", "H"), card("A", "S")]), "Pair");
        assert_eq!(
            classify_hand(&[
                card("A", "H"),
                card("A", "S"),
                card("K", "D"),
                card("K", "C")
            ]),
            "Two Pair"
        );
        assert_eq!(
            classify_hand(&[
                card("2", "H"),
                card("3", "H"),
                card("4", "H"),
                card("5", "H"),
                card("6", "H")
            ]),
            "Straight Flush"
        );
        assert_eq!(
            classify_hand(&[
                json!({"base":{"value":"9","suit":"Hearts"}}),
                json!({"base":{"value":"9","suit":"Diamonds"}})
            ]),
            "Pair"
        );
        assert_eq!(
            classify_hand(&[
                card("A", "H"),
                card("K", "H"),
                card("Q", "H"),
                card("J", "H"),
                card("10", "H")
            ]),
            "Straight Flush"
        );
        assert_eq!(
            classify_hand(&[
                card("A", "H"),
                card("A", "S"),
                card("A", "D"),
                card("A", "C"),
                card("A", "H")
            ]),
            "Five of a Kind"
        );
        assert_eq!(
            classify_hand(&[
                card("A", "H"),
                card("A", "S"),
                card("A", "D"),
                card("K", "C"),
                card("K", "H")
            ]),
            "Full House"
        );
        assert_eq!(
            classify_hand(&[card("A", "H"), card("A", "S"), card("A", "D")]),
            "Three of a Kind"
        );
        assert_eq!(
            classify_hand(&[
                card("A", "H"),
                card("K", "H"),
                card("Q", "H"),
                card("J", "H"),
                card("9", "H")
            ]),
            "Flush"
        );
        assert_eq!(
            classify_hand(&[
                card("A", "H"),
                card("2", "S"),
                card("3", "D"),
                card("4", "C"),
                card("5", "H")
            ]),
            "Straight"
        );
        assert_eq!(
            classify_hand(&[card("A", "H"), card("K", "S")]),
            "High Card"
        );
    }

    #[test]
    fn scores_contract_and_supported_modifiers() {
        let observation = json!({"areas":{"hand":[{"base":{"rank":"A","suit":"H"},"suits":[{"key":"H"}],"edition":"Foil"}]},"poker_hands":{"values":{"High Card":{"chips":10,"mult":2}}}});
        let result = score_hand(&observation, None);
        assert_eq!(result.chips, 71);
        assert_eq!(result.exact_score, Some(142));
        assert_eq!(result.score_scope, "current_hand");
        assert_eq!(result.run_most_played_hand, None);
    }

    #[test]
    fn score_metadata_distinguishes_selection_from_run_history() {
        let observation = json!({
            "run": {"most_played_poker_hand": "Pair"},
            "areas": {"hand": [card("A", "H"), card("K", "S")]},
            "poker_hands": {"values": {"High Card": {"chips": 10, "mult": 2}}}
        });
        let result = score_hand(&observation, Some(&[0]));
        assert_eq!(result.hand_key, "High Card");
        assert_eq!(result.score_scope, "selected_cards");
        assert_eq!(result.run_most_played_hand.as_deref(), Some("Pair"));
    }

    #[test]
    fn score_metadata_includes_run_progress() {
        let observation = json!({
            "run": {
                "chips": 120,
                "blind": {"chips_required": 300}
            },
            "areas": {"hand": [card("A", "H")]},
            "poker_hands": {"values": {"High Card": {"chips": 10, "mult": 2}}}
        });
        let result = score_hand(&observation, None);
        assert_eq!(result.run_chips, Some(120));
        assert_eq!(result.blind_chips_required, Some(300));
        assert_eq!(result.blind_chips_remaining, Some(180));
    }

    #[test]
    fn score_metadata_resolves_root_blind_without_using_target_as_score() {
        let observation = json!({
            "round": {"chips": 1200},
            "blind": {"chips": 1600},
            "areas": {"hand": [card("A", "H")]},
            "poker_hands": {"values": {"High Card": {"chips": 10, "mult": 2}}}
        });
        let result = score_hand(&observation, None);
        assert_eq!(result.run_chips, Some(1200));
        assert_eq!(result.blind_chips_required, Some(1600));
        assert_eq!(result.blind_chips_remaining, Some(400));
    }

    #[test]
    fn score_uses_live_highlighted_cards_when_indices_are_omitted() {
        let observation = json!({
            "areas": {
                "hand": [card("K", "S"), card("9", "H"), card("9", "D"), card("7", "C")],
                "hand_highlighted": [2, 3]
            },
            "poker_hands": {
                "values": {
                    "High Card": {"chips": 5, "mult": 1},
                    "Pair": {"chips": 10, "mult": 2}
                }
            }
        });
        let result = score_hand(&observation, None);
        assert_eq!(result.hand_key, "Pair");
        assert_eq!(result.scoring_cards, vec![2, 3]);
        assert_eq!(result.score_scope, "selected_cards");
        assert_eq!(result.exact_score, Some(56));
    }

    #[test]
    fn score_uses_live_nominal_chips_for_aces_and_faces() {
        let observation = json!({
            "areas": {"hand": [
                {"base":{"rank":"A","nominal":11,"suit":"Hearts"}},
                {"base":{"rank":"K","nominal":10,"suit":"Spades"}}
            ]},
            "poker_hands": {"values": {"High Card": {"chips": 5, "mult": 1}}}
        });
        let result = score_hand(&observation, Some(&[0, 1]));
        assert_eq!(result.hand_name, "High Card");
        assert_eq!(result.chips, 26);
        assert_eq!(result.estimated_score, 26);
    }

    #[test]
    fn score_pair_does_not_include_unselected_cards() {
        let observation = json!({
            "areas": {"hand": [
                card("A", "H"), card("A", "S"), card("K", "D"), card("Q", "C")
            ]},
            "poker_hands": {"values": {"Pair": {"chips": 10, "mult": 2}}}
        });
        let result = score_hand(&observation, Some(&[0, 1]));
        assert_eq!(result.hand_name, "Pair");
        assert_eq!(result.scoring_cards, vec![1, 2]);
        assert_eq!(result.chips, 32);
        assert_eq!(result.estimated_score, 64);
    }

    #[test]
    fn score_pair_ignores_non_scoring_cards_in_selected_hand() {
        let observation = json!({
            "areas": {"hand": [
                card("A", "H"), card("A", "S"), card("K", "D"), card("Q", "C"), card("J", "H")
            ]},
            "poker_hands": {"values": {"Pair": {"chips": 10, "mult": 2}}}
        });
        let result = score_hand(&observation, Some(&[0, 1, 2, 3, 4]));
        assert_eq!(result.hand_name, "Pair");
        assert_eq!(result.scoring_cards, vec![1, 2]);
        assert_eq!(result.chips, 32);
        assert_eq!(result.estimated_score, 64);
    }

    #[test]
    fn score_full_live_hand_chooses_best_five_card_subset() {
        let observation = json!({
            "areas": {"hand": [
                {"base":{"value":"9","suit":"Hearts"}},
                {"base":{"value":"9","suit":"Diamonds"}},
                {"base":{"value":"K","suit":"Spades"}},
                {"base":{"value":"7","suit":"Hearts"}},
                {"base":{"value":"6","suit":"Hearts"}},
                {"base":{"value":"4","suit":"Hearts"}},
                {"base":{"value":"3","suit":"Spades"}},
                {"base":{"value":"2","suit":"Spades"}}
            ]},
            "poker_hands": {"values": {
                "High Card":{"chips":5,"mult":1},
                "Pair":{"chips":10,"mult":2}
            }}
        });
        let result = score_hand(&observation, None);
        assert_eq!(result.hand_name, "Pair");
        assert_eq!(result.score_scope, "best_play");
        assert_eq!(result.scoring_cards.len(), 2);
        assert!(result.scoring_cards.contains(&1));
        assert!(result.scoring_cards.contains(&2));
    }

    #[test]
    fn scores_known_joker_effects_without_marking_them_unsupported() {
        let observation = json!({
            "run":{"discards_left":0},
            "areas":{"hand":[card("Q","H"),card("Q","S")],"jokers":[{"name":"Mystic Summit","ability":{}}]},
            "poker_hands":{"values":{"Pair":{"chips":10,"mult":2}}}
        });
        let result = score_hand(&observation, None);
        assert_eq!(result.mult, 17);
        assert!(result.unsupported_effects.is_empty());
        let without_ability = json!({
            "areas":{"hand":[card("A","H")],"jokers":[{"name":"Mystery"}]},
            "poker_hands":{"values":{"High Card":{"chips":10,"mult":2}}}
        });
        assert!(
            score_hand(&without_ability, None)
                .unsupported_effects
                .is_empty()
        );
        let fallback_effects = json!({
            "run":{"discards_left":2},
            "areas":{"hand":[],"jokers":[
                {"name":"Hanging Chad","config":{"extra":2}},
                {"name":"Raised Fist"},
                {"name":"Mystic Summit"},
                {"name":"Vampire","config":{"Xmult":2.0}}
            ]},
            "poker_hands":{"values":{"High Card":{}}}
        });
        let fallback = score_hand(&fallback_effects, None);
        assert!(fallback.exact_score.is_none());
        let banner_fallback = score_hand(
            &json!({
                "run":{"discards_left":1},
                "areas":{"hand":[],"jokers":[{"name":"Banner","config":{"extra":3}}]},
                "poker_hands":{"values":{"High Card":{"chips":5,"mult":1}}}
            }),
            None,
        );
        assert_eq!(banner_fallback.chips, 8);
    }

    #[test]
    fn modifiers_and_jokers_are_deterministic_and_visible() {
        let mut hand = card("A", "H");
        hand["edition"] = json!("Polychrome");
        hand["enhancement"] = json!("Glass");
        let observation = json!({
            "run":{"discards_left":2},
            "areas":{"hand":[hand],"jokers":[
                {"name":"Banner","ability":{"extra":3}},
                {"name":"Raised Fist","ability":{}},
                {"name":"Hanging Chadd","center_key":"j_hanging_chad","ability":{"extra":2},"config":{"extra":2}},
                {"name":"Vampire","ability":{"x_mult":1.5}},
                {"name":"Triboulet","ability":{}},
                {"name":"Unknown","ability":{"mystery":true}}
            ]},
            "poker_hands":{"values":{"High Card":{"chips":10,"mult":2}}}
        });
        let result = score_hand(&observation, Some(&[0, 99]));
        assert_eq!(result.scoring_cards, vec![1]);
        assert!(result.x_mult > 1.0);
        assert!(result.unsupported_effects.iter().any(|x| x == "Unknown"));
        assert_eq!(result.estimate_quality, "partial_contract");
    }

    #[test]
    fn alternate_card_shapes_and_missing_contract_are_safe() {
        let observation = json!({"areas":{"hand":[
            {"rank":"10","suit":"Spades","edition":{"name":"Holographic"},"enhancement":{"key":"Bonus"}},
            {"rank":"bad","suits":[{"name":"Hearts"}],"edition":"Negative"}
        ]}});
        let result = score_hand(&observation, None);
        assert_eq!(result.hand_name, "High Card");
        assert!(result.exact_score.is_none());
        assert!(
            result
                .unsupported_effects
                .iter()
                .any(|x| x.contains("missing poker_hands"))
        );
        let malformed_values = score_hand(
            &json!({
                "areas":{"hand":[card("A","H")]},
                "poker_hands":{"values":{"High Card":{}}}
            }),
            None,
        );
        assert!(malformed_values.exact_score.is_none());
        let partially_malformed_values = score_hand(
            &json!({
                "areas":{"hand":[card("A","H")]},
                "poker_hands":{"values":{"High Card":{"chips":7}}}
            }),
            None,
        );
        assert!(partially_malformed_values.exact_score.is_none());
        let malformed_chips = score_hand(
            &json!({
                "areas":{"hand":[card("A","H")]},
                "poker_hands":{"values":{"High Card":{"chips":"bad","mult":2}}}
            }),
            None,
        );
        assert!(malformed_chips.exact_score.is_none());
        let malformed_mult = score_hand(
            &json!({
                "areas":{"hand":[card("A","H")]},
                "poker_hands":{"values":{"High Card":{"chips":7,"mult":"bad"}}}
            }),
            None,
        );
        assert!(malformed_mult.exact_score.is_none());
        let fallback_hand_value = score_hand(
            &json!({
                "areas":{"hand":[card("A","H"), card("A","S")]},
                "poker_hands":{"values":{"High Card":{"chips":7,"mult":3}}}
            }),
            None,
        );
        assert_eq!(fallback_hand_value.hand_name, "Pair");
        assert_eq!(fallback_hand_value.chips, 7 + 11 + 11);
    }

    #[test]
    fn all_card_modifier_families_are_explicitly_handled() {
        for edition in ["Holographic", "Holo", "Negative", "Mystery"] {
            for enhancement in ["Mult", "Steel", "Wild", "Stone", "Gold", "Lucky", "Mystery"] {
                let observation = json!({
                    "areas":{"hand":[{"base":{"rank":"A","suit":"H"},"edition":edition,"enhancement":enhancement}]},
                    "poker_hands":{"values":{"High Card":{"chips":10,"mult":2}}}
                });
                let result = score_hand(&observation, None);
                assert_eq!(result.hand_name, "High Card");
                if edition == "Mystery" || enhancement == "Mystery" {
                    assert!(result.exact_score.is_none());
                }
            }
        }
        let four = vec![
            card("A", "H"),
            card("A", "S"),
            card("A", "D"),
            card("A", "C"),
        ];
        assert_eq!(classify_hand(&four), "Four of a Kind");
    }
}
