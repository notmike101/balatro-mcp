from __future__ import annotations

from typing import Any


POKER_HAND_KEYS = (
    "Flush Five",
    "Flush House",
    "Five of a Kind",
    "Straight Flush",
    "Four of a Kind",
    "Full House",
    "Flush",
    "Straight",
    "Three of a Kind",
    "Two Pair",
    "Pair",
    "High Card",
)


class HandValueUnavailable(ValueError):
    pass


def hand_level_value(hand_name, contract):
    if not isinstance(contract, dict):
        raise HandValueUnavailable("canonical poker_hands contract missing")
    if contract.get("schema") != "balatro-poker-hand-values/v1":
        raise HandValueUnavailable(f"unsupported poker_hands schema: {contract.get('schema')}")
    if not contract.get("valid_for_scoring"):
        raise HandValueUnavailable(f"poker hand values are not valid for scoring: source={contract.get('source')}")
    value = (contract.get("values") or {}).get(hand_name)
    if not isinstance(value, dict):
        raise HandValueUnavailable(f"poker hand value missing: {hand_name}")
    if value.get("chips") is None or value.get("mult") is None or value.get("level") is None:
        raise HandValueUnavailable(f"poker hand value incomplete: {hand_name}")
    return value


def hand_level_values(hand_name, contract):
    value = hand_level_value(hand_name, contract)
    return int(value["chips"]), int(value["mult"])


def ordered_hand_values(contract):
    values = contract.get("values") or {}
    return sorted(
        (value for value in values.values() if isinstance(value, dict)),
        key=lambda value: (int(value.get("order") or 999), str(value.get("key") or "")),
    )


def resolve_hand_key(name):
    if name.lower() == "royal flush":
        return "Straight Flush"
    return next((key for key in POKER_HAND_KEYS if key.lower() == name.lower()), None)


def _has_joker(jokers, name):
    if not jokers:
        return False
    normalized = name.lower().replace(" ", "_")
    for j in jokers:
        center = j.get("center_key") or ""
        ability = j.get("ability") or {}
        jname = ability.get("name") or center.replace("j_", "").replace("j-", "_")
        if jname.lower().replace(" ", "_") == normalized:
            return True
    return False


def _is_straight_check(ranks, min_cards=5, allow_gap=False):
    unique = sorted(set(ranks))
    n = len(unique)
    if n < min_cards:
        return False
    if set(unique) == {2, 3, 4, 5, 14}:
        return n >= min_cards
    presence = [False] * 15
    for r in unique:
        if 1 <= r <= 14:
            presence[r] = True
    straight_length = 0
    skipped_rank = False
    for j in range(1, 15):
        if presence[j]:
            straight_length += 1
            skipped_rank = False
        elif allow_gap and not skipped_rank:
            skipped_rank = True
        else:
            straight_length = 0
            skipped_rank = False
        if straight_length >= min_cards:
            return True
    return False


def _card_valid(card):
    b = card.get("base")
    return isinstance(b, dict) and b is not None


def _card_rank_id(card):
    b = card.get("base")
    if not isinstance(b, dict):
        return 0
    return int(b.get("rank_id") or b.get("id") or 0)


def _card_nominal(card):
    b = card.get("base")
    if not isinstance(b, dict):
        return 0
    return int(b.get("nominal") or 0)


def classify_cards(cards, jokers=None):
    visible = [c for c in cards if _card_valid(c) and _card_rank_id(c) > 0]
    if not visible:
        return "High Card"
    ranks = sorted(_card_rank_id(c) for c in visible)
    suits = [c["base"]["suit"] for c in visible]
    counts = sorted((ranks.count(rank) for rank in set(ranks)), reverse=True)
    unique = sorted(set(ranks))
    min_cards = 4 if _has_joker(jokers, "Four Fingers") else 5
    allow_gap = _has_joker(jokers, "Shortcut")
    is_flush = len(visible) >= min_cards and len(set(suits)) == 1
    is_straight = _is_straight_check(ranks, min_cards=min_cards, allow_gap=allow_gap)
    if counts[:1] == [5] and is_flush:
        return "Flush Five"
    if counts[:2] == [3, 2] and is_flush:
        return "Flush House"
    if counts[:1] == [5]:
        return "Five of a Kind"
    if is_flush and is_straight:
        return "Straight Flush"
    if counts[:1] == [4]:
        return "Four of a Kind"
    if counts[:2] == [3, 2]:
        return "Full House"
    if is_flush:
        return "Flush"
    if is_straight:
        return "Straight"
    if counts[:1] == [3]:
        return "Three of a Kind"
    if counts[:2] == [2, 2]:
        return "Two Pair"
    if counts[:1] == [2]:
        return "Pair"
    return "High Card"


def scoring_cards(cards, hand_name):
    visible = [c for c in cards if _card_valid(c) and _card_rank_id(c) > 0]
    by_rank = {}
    for card in visible:
        by_rank.setdefault(_card_rank_id(card), []).append(card)
    if hand_name in {"Straight Flush", "Straight", "Flush", "Full House"}:
        return cards
    if hand_name == "Four of a Kind":
        rank = max((rank for rank, rank_cards in by_rank.items() if len(rank_cards) >= 4), default=None)
        return by_rank[rank][:4] if rank is not None else cards
    if hand_name == "Three of a Kind":
        rank = max((rank for rank, rank_cards in by_rank.items() if len(rank_cards) >= 3), default=None)
        return by_rank[rank][:3] if rank is not None else cards
    if hand_name == "Two Pair":
        pair_ranks = sorted((rank for rank, rank_cards in by_rank.items() if len(rank_cards) >= 2), reverse=True)[:2]
        return [card for rank in pair_ranks for card in by_rank[rank][:2]]
    if hand_name == "Pair":
        rank = max((rank for rank, rank_cards in by_rank.items() if len(rank_cards) >= 2), default=None)
        return by_rank[rank][:2] if rank is not None else cards
    return [max(cards, key=_card_nominal)] if cards else cards
