#!/usr/bin/env python3
"""Command implementation for the supported Balatro reliability CLI."""

from __future__ import annotations

import argparse
import itertools
import json
import os
import sys
import time
from typing import Any

from .config import BALATRO_DIR, COMMAND_PATH, OBSERVATION_PATH, RESPONSE_PATH
from . import ipc as reliable_ipc
from . import scoring as reliable_scoring
from .policy import is_decision_state, select_safe_transition
from . import estimation_feedback as _est_feedback
from .reliability import (
    add_strategy_rule,
    add_strategy_evidence,
    checkpoint,
    decision_id,
    enhance_policy_state,
    read_observation,
    status_payload,
    validate_decision_id,
)
from . import ALLOWED_SEED, POLICY_SCHEMA
from .runtime import SafetyError, guard_command
from .ipc import read_json, to_lua, next_command_id, write_command, wait_for_response
from .ipc import _group_actions, _resolve_field






def cmd_command(args: argparse.Namespace) -> int:
    command = json.loads(args.json_command)
    if not isinstance(command, dict):
        raise SystemExit("JSON command must be an object")
    if str(command.get("action") or command.get("type") or "") != "observe":
        provided_decision_id = str(command.pop("decision_id", ""))
        policy_state = load_policy_state(30, 15, 60)
        current_decision_id = str(policy_state.get("decision_id") or "")
        if provided_decision_id != current_decision_id:
            print(json.dumps({"ok": False, "message": "stale or missing decision_id", "expected": current_decision_id}), file=sys.stderr)
            return 1
        try:
            guard_command(command, read_json(OBSERVATION_PATH))
        except SafetyError as exc:
            print(json.dumps({"ok": False, "message": str(exc)}), file=sys.stderr)
            return 2
    command = write_command(command, echo=False)
    if args.wait:
        response = wait_for_response(command.get("id"), args.timeout)
        if response is None:
            print("Timed out waiting for command response", file=sys.stderr)
            return 1
        if not getattr(args, "quiet", False):
            print(json.dumps(response, indent=2, sort_keys=True))
    return 0


def cmd_action(args: argparse.Namespace) -> int:
    command: dict[str, Any] = {"action": args.action}

    if args.action in {"play", "discard"}:
        cards = []
        card_ids = []
        for item in args.values:
            if item.startswith("card_ids="):
                card_ids.extend(parse_int_list(item.split("=", 1)[1]))
            elif item.startswith("instance_ids="):
                card_ids.extend(parse_int_list(item.split("=", 1)[1]))
            elif item.startswith("card_id="):
                card_ids.append(int(item.split("=", 1)[1]))
            elif item.startswith("instance_id="):
                card_ids.append(int(item.split("=", 1)[1]))
            elif "_" in item and not item.startswith("_"):
                parts = item.rsplit("_", 1)
                cards.append(int(parts[0]))
                card_ids.append(int(parts[1]))
            else:
                cards.append(int(item))
        command["cards"] = cards
        if card_ids:
            if len(card_ids) != len(cards):
                raise SystemExit("card_ids count must match selected card count")
            command["card_ids"] = card_ids
    elif args.action in {"buy", "buy_and_use", "use", "sell"}:
        if len(args.values) < 2:
            raise SystemExit(f"{args.action} requires AREA INDEX [TARGET_INDEX ...] [card_id=ID]")
        command["area"] = args.values[0]
        command["index"] = int(args.values[1])
        targets = []
        if len(args.values) > 2:
            for item in args.values[2:]:
                if item.startswith("card_id="):
                    command["card_id"] = int(item.split("=", 1)[1])
                elif item.startswith("instance_id="):
                    command["card_id"] = int(item.split("=", 1)[1])
                elif item.startswith("target_card_ids="):
                    command["target_card_ids"] = parse_int_list(item.split("=", 1)[1])
                elif item.startswith("target_instance_ids="):
                    command["target_card_ids"] = parse_int_list(item.split("=", 1)[1])
                else:
                    targets.append(int(item))
        if "target_card_ids" in command and len(command["target_card_ids"]) != len(targets):
            raise SystemExit("target_card_ids count must match selected target count")
        if targets:
            command["targets"] = targets
    elif args.action == "move_card":
        if len(args.values) < 3:
            raise SystemExit("move_card requires AREA FROM_INDEX TO_INDEX [CARD_ID]")
        command["area"] = args.values[0]
        command["from_index"] = int(args.values[1])
        command["to_index"] = int(args.values[2])
        if len(args.values) > 3:
            command["card_id"] = int(args.values[3])
    elif args.action == "move_joker":
        if len(args.values) < 2:
            raise SystemExit("move_joker requires FROM_INDEX TO_INDEX [CARD_ID]")
        command["action"] = "move_joker"
        command["from_index"] = int(args.values[0])
        command["to_index"] = int(args.values[1])
        if len(args.values) > 2:
            command["card_id"] = int(args.values[2])
    elif args.action == "click":
        if len(args.values) != 2:
            raise SystemExit("click requires X Y")
        command["x"] = float(args.values[0])
        command["y"] = float(args.values[1])
    elif args.action == "ui_click":
        for value in args.values:
            if value.startswith("ui_id="):
                command["ui_id"] = value.split("=", 1)[1]
            elif value.startswith("id="):
                command["ui_id"] = value.split("=", 1)[1]
            elif value.startswith("button="):
                command["button"] = value.split("=", 1)[1]
            elif value.startswith("occurrence="):
                command["occurrence"] = int(value.split("=", 1)[1])
            else:
                raise SystemExit("ui_click values must be ui_id=..., button=..., or occurrence=...")
    elif args.action == "start_run":
        for value in args.values:
            if value.startswith("seed="):
                command["seed"] = value.split("=", 1)[1]
            elif value.startswith("stake="):
                command["stake"] = int(value.split("=", 1)[1])
            else:
                raise SystemExit(f"Unknown start_run option: {value}")
    elif args.action == "speed":
        for value in args.values:
            if value.startswith("game_speed="):
                command["game_speed"] = float(value.split("=", 1)[1])
            elif value.startswith("fps_cap="):
                command["fps_cap"] = int(value.split("=", 1)[1])
            else:
                raise SystemExit(f"Unknown speed option: {value}")
    elif args.action == "sort_hand":
        command["mode"] = args.values[0] if args.values else "value"
    elif args.action == "skip_blind":
        pass
    elif args.values:
        raise SystemExit(f"{args.action} does not take positional values")

    if args.action != "observe":
        policy_state = load_policy_state(30, 15, 60)
        current_decision_id = str(policy_state.get("decision_id") or "")
        if args.decision_id != current_decision_id:
            print(
                json.dumps(
                    {
                        "ok": False,
                        "message": "stale or missing decision_id",
                        "expected": current_decision_id,
                        "provided": args.decision_id,
                    }
                ),
                file=sys.stderr,
            )
            return 1
        try:
            guard_command(command, read_json(OBSERVATION_PATH))
        except SafetyError as exc:
            print(json.dumps({"ok": False, "message": str(exc)}), file=sys.stderr)
            return 2

    command = write_command(command, echo=False)
    if args.wait:
        response = wait_for_response(command.get("id"), args.timeout)
        if response is None:
            print("Timed out waiting for command response", file=sys.stderr)
            return 1
        if not getattr(args, "quiet", False):
            print(json.dumps(response, indent=2, sort_keys=True))
    return 0


def cmd_wait(args: argparse.Namespace) -> int:
    deadline = time.time() + args.timeout
    while time.time() < deadline:
        if OBSERVATION_PATH.exists():
            data = read_json(OBSERVATION_PATH)
            state_name = ((data.get("game") or {}).get("state_name") or "").upper()
            ready = data.get("ready") or {}
            state_ok = not args.state or state_name == args.state.upper()
            ready_ok = not args.ready or bool(ready.get(args.ready))
            if state_ok and ready_ok:
                if args.json:
                    print(json.dumps(data, indent=2, sort_keys=True))
                else:
                    print(f"State: {data.get('game', {}).get('state_name', '?')} Seed: {data.get('round', {}).get('seed', '?')} Ante {data.get('round', {}).get('ante', '?')}, Round {data.get('round', {}).get('round', '?')}")
                    print(f"Blind: {data.get('blind', {}).get('name', '?')} ({data.get('blind', {}).get('chips', '?')} chips)")
                    print(f"Chips: {data.get('round', {}).get('chips', 0)} | Hands: {data.get('round', {}).get('hands_left', 0)} | Discards: {data.get('round', {}).get('discards_left', 0)} | $: {data.get('round', {}).get('dollars', 0)}")
                return 0
        if getattr(args, "quiet", False):
            return 1
        time.sleep(args.interval)
    print("Timed out waiting for observation", file=sys.stderr)
    return 1


def int_or_none(value: Any) -> int | None:
    try:
        if value is None:
            return None
        return int(value)
    except (TypeError, ValueError):
        return None


def parse_int_list(value: str) -> list[int]:
    if not value:
        return []
    return [int(item.strip()) for item in value.replace(";", ",").split(",") if item.strip()]


def card_label(card: dict[str, Any]) -> str:
    base = card.get("base") or {}
    return str(base.get("name") or card.get("name") or card.get("center_key") or card.get("index"))


def card_ref(card: dict[str, Any]) -> str:
    return f"{int(card['index'])}_{card.get('id') or card.get('instance_id')}"


def card_instance_id(card: dict[str, Any]) -> Any:
    return card.get("id") or card.get("instance_id")


def card_command(action: str, area_name: str, card: dict[str, Any]) -> dict[str, Any]:
    command = {"action": action, "area": area_name, "index": card.get("index")}
    instance_id = card_instance_id(card)
    if instance_id is not None:
        command["card_id"] = instance_id
    return command


def hand_card_command(action: str, cards: list[dict[str, Any]]) -> dict[str, Any]:
    command = {"action": action, "cards": [int(card["index"]) for card in cards]}
    card_ids = [card_instance_id(card) for card in cards]
    if all(card_id is not None for card_id in card_ids):
        command["card_ids"] = card_ids
    return command


def compact_card(card: dict[str, Any]) -> dict[str, Any]:
    # Mask identity for face-down cards (e.g., The Fish boss effect)
    face_down = card.get("facing") == "back"
    base = card.get("base") or {}
    ability = card.get("ability") or {}
    return {
        "index": card.get("index"),
        "instance_id": card_instance_id(card),
        "name": None if face_down else card.get("name"),
        "set": card.get("set"),
        "center_key": card.get("center_key"),
        "card_key": card.get("card_key"),
        "effect": card.get("effect"),
        "config": card.get("center_config"),
        "cost": card.get("cost"),
        "sell_cost": card.get("sell_cost"),
        "debuffed": card.get("debuffed"),
        "edition": card.get("edition"),
        "seal": None if face_down else card.get("seal"),
        "pinned": card.get("pinned"),
        "base": {
            "name": None if face_down else base.get("name"),
            "rank": None if face_down else base.get("value"),
            "rank_id": None if face_down else base.get("id"),
            "suit": None if face_down else base.get("suit"),
            "nominal": None if face_down else base.get("nominal"),
        },
        "ability": {
            key: ability.get(key)
            for key in (
                "name",
                "set",
                "extra",
                "bonus",
                "mult",
                "x_mult",
                "chips",
                "h_mult",
                "h_x_mult",
                "t_chips",
                "t_mult",
                "perma_bonus",
                "perma_mult",
                "perma_x_mult",
                "perma_h_chips",
                "perma_h_mult",
                "hands",
                "discards",
                "d_size",
                "consumeable",
                "max_highlighted",
                "min_highlighted",
                "mod_conv",
                "suit_conv",
                "hand_type",
                "choose",
            )
            if key in ability
        },
    }


def use_target_range(card: dict[str, Any]) -> tuple[int, int]:
    ability = card.get("ability") or {}
    consumeable = ability.get("consumeable")
    consumeable = consumeable if isinstance(consumeable, dict) else {}
    max_highlighted = int_or_none(ability.get("max_highlighted"))
    if max_highlighted is None:
        max_highlighted = int_or_none(consumeable.get("max_highlighted"))
    min_highlighted = int_or_none(ability.get("min_highlighted"))
    if min_highlighted is None:
        min_highlighted = int_or_none(consumeable.get("min_highlighted"))
    if not max_highlighted and not min_highlighted:
        return 0, 0
    max_count = max(0, max_highlighted or min_highlighted or 0)
    min_count = min_highlighted if min_highlighted is not None else min(1, max_count)
    min_count = max(0, min(min_count, max_count))
    return min_count, max_count


def use_actions_for_card(
    *,
    area_name: str,
    card: dict[str, Any],
    hand: list[dict[str, Any]],
    target_limit: int,
) -> list[dict[str, Any]]:
    min_targets, max_targets = use_target_range(card)
    if target_limit <= 0: return []
    base_action = {
        "action": "use",
        "area": area_name,
        "index": card.get("index"),
        "card_ref": card_ref(card),
        "card": compact_card(card),
    }

    if max_targets <= 0:
        action = dict(base_action)
        action["command"] = card_command("use", area_name, card)
        return [action]

    actions: list[dict[str, Any]] = []
    remaining = max(0, target_limit)
    if min_targets == 0:
        action = dict(base_action)
        action["targets"] = []
        action["target_refs"] = []
        action["target_card_names"] = []
        action["command"] = card_command("use", area_name, card)
        action["command"]["targets"] = []
        actions.append(action)
        remaining -= 1

    max_targets = min(max_targets, len(hand))
    for count in range(max(1, min_targets), max_targets + 1):
        for combo in itertools.combinations(hand, count):
            if remaining <= 0:
                return actions
            combo_cards = list(combo)
            action = dict(base_action)
            action["targets"] = [int(target["index"]) for target in combo_cards]
            action["target_refs"] = [card_ref(target) for target in combo_cards]
            action["target_card_names"] = [card_label(target) for target in combo_cards]
            action["command"] = card_command("use", area_name, card)
            action["command"]["targets"] = action["targets"]
            target_card_ids = [card_instance_id(target) for target in combo_cards]
            if all(card_id is not None for card_id in target_card_ids):
                action["command"]["target_card_ids"] = target_card_ids
            actions.append(action)
            remaining -= 1
    return actions


def move_card_actions(area_name: str, cards: list[dict[str, Any]]) -> list[dict[str, Any]]:
    actions: list[dict[str, Any]] = []
    if len(cards) < 2:
        return actions
    for card in cards:
        from_index = int(card["index"])
        for to_index in range(1, len(cards) + 1):
            if to_index == from_index:
                continue
            instance_id = card_instance_id(card)
            actions.append(
                {
                    "action": "move_card",
                    "area": area_name,
                    "from_index": from_index,
                    "to_index": to_index,
                    "card_ref": card_ref(card),
                    "card": compact_card(card),
                    "command": {
                        "action": "move_card",
                        "area": area_name,
                        "from_index": from_index,
                        "to_index": to_index,
                        "card_id": instance_id,
                    },
                }
            )
    return actions


def sell_actions_for_area(area_name: str, cards: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [
        {
            "action": "sell",
            "area": area_name,
            "index": card.get("index"),
            "card_ref": card_ref(card),
            "card": compact_card(card),
            "command": card_command("sell", area_name, card),
        }
        for card in cards
    ]


def hand_level_values(hand_name: str, poker_hand_values: dict[str, Any] | None = None) -> tuple[int, int]:
    return reliable_scoring.hand_level_values(hand_name, poker_hand_values)


# Delegate to scoring module for hand classification
classify_cards = reliable_scoring.classify_cards
scoring_cards = reliable_scoring.scoring_cards


def hand_contains_type(hand_name: str, required: str | None) -> bool:
    if not required:
        return False
    contains = {
        "Pair": {"Pair", "Two Pair", "Three of a Kind", "Full House", "Four of a Kind", "Five of a Kind"},
        "Two Pair": {"Two Pair"},
        "Three of a Kind": {"Three of a Kind", "Full House", "Four of a Kind", "Five of a Kind"},
        "Straight": {"Straight", "Straight Flush"},
        "Flush": {"Flush", "Straight Flush", "Flush House", "Flush Five"},
        "Full House": {"Full House", "Flush House"},
        "Four of a Kind": {"Four of a Kind", "Five of a Kind"},
        "Straight Flush": {"Straight Flush"},
    }
    return hand_name in contains.get(required, {required})


def edition_score_bonus(edition: Any) -> tuple[int, int, float]:
    if edition == "foil":
        return 50, 0, 1.0
    if edition == "holographic":
        return 0, 10, 1.0
    if edition == "polychrome":
        return 0, 0, 1.5
    return 0, 0, 1.0



def float_or_none(value: Any) -> float | None:
    try:
        if value is None:
            return None
        return float(value)
    except (TypeError, ValueError):
        return None


def card_modifier_score(card: dict[str, Any]) -> tuple[int, int, float]:
    ability = card.get("ability") or {}
    chips = 0
    mult = 0
    x_mult = 1.0
    for key in ("bonus", "perma_bonus", "perma_h_chips"):
        chips += int_or_none(ability.get(key)) or 0
    for key in ("mult", "perma_mult", "perma_h_mult"):
        mult += int_or_none(ability.get(key)) or 0
    for key in ("x_mult", "perma_x_mult"):
        value = float_or_none(ability.get(key))
        if value and value != 1:
            x_mult *= value
    e_chips, e_mult, e_x_mult = edition_score_bonus(card.get("edition"))
    # Card seal bonuses (applied when card is scored)
    seal = card.get("seal") or ""
    if seal == "diamond":
        chips += 5  # +5 chips per Diamond seal card played
    elif seal == "blue":
        # +1 chip per card; number of cards in hand determined by caller
        pass  # handled in score_breakdown via extra param
    elif seal == "red":
        mult += 1  # +1 Mult when Red seal card is scored
    # Gold seal gives $3 end of round � not a scoring modifier
    return chips + e_chips, mult + e_mult, x_mult * e_x_mult


# ── Joker effect lookup table ──────────────────────────────────────────
# Each entry maps center_key → handler function.
# Handlers receive (joker, hand_name, cards, round_data) and return
# (extra_chips, extra_mult, extra_x_mult).
# The generic ability/config fields are applied AFTER the handler.


def _joker_banner(joker, hand_name, cards, round_data):
    """Banner: +chips per discard used this round."""
    ability = joker.get("ability") or {}
    config = joker.get("center_config") or joker.get("config") or {}
    extra = int_or_none(ability.get("extra")) or int_or_none(config.get("extra")) or 0
    discards_left = int_or_none((round_data or {}).get("discards_left")) or 0
    return extra * discards_left, 0, 1.0


def _joker_raised_fist(joker, hand_name, cards, round_data):
    """Raised Fist: +2 mult × min rank of all cards in hand."""
    try:
        visible = [c for c in cards if c.get("base") and int(c["base"].get("rank_id") or c["base"].get("id") or 0) > 0]
        if visible:
            ranks = [int(c["base"].get("rank_id") or c["base"].get("id") or 0) for c in visible]
            return 0, 2 * min(ranks), 1.0
    except Exception:
        pass
    return 0, 0, 1.0


def _joker_hanging_chad(joker, hand_name, cards, round_data):
    """Hanging Chad: retriggers first scoring card's nominal chip value."""
    config = joker.get("center_config") or joker.get("config") or {}
    extra = int_or_none(config.get("extra")) or 0
    if extra > 0 and cards:
        first_chips = int(cards[0].get("base", {}).get("nominal") or 0)
        return extra * first_chips, 0, 1.0
    return 0, 0, 1.0


def _joker_mystic_summit(joker, hand_name, cards, round_data):
    """Mystic Summit: +15 mult when 0 discards remaining."""
    discards_left = int_or_none((round_data or {}).get("discards_left")) or 0
    if discards_left <= 0:
        return 0, 15, 1.0
    return 0, 0, 1.0


def _joker_vampire(joker, hand_name, cards, round_data):
    """Vampire: uses display Xmult from config/ability (tracks enhanced cards)."""
    ability = joker.get("ability") or {}
    config = joker.get("center_config") or joker.get("config") or {}
    display_xmult = float_or_none(ability.get("x_mult")) or float_or_none(config.get("Xmult")) or 1.0
    return 0, 0, display_xmult if display_xmult > 1.0 else 1.0


def _joker_triboulet(joker, hand_name, cards, round_data):
    """Triboulet: each King/Queen scoring card gives x2 mult."""
    try:
        _hn = reliable_scoring.classify_cards(cards)
        _scoring = reliable_scoring.scoring_cards(cards, _hn)
        face_ids = {12, 13}  # Q=12, K=13
        face_count = sum(1 for c in _scoring if int(c.get("base", {}).get("rank_id") or c.get("base", {}).get("id") or 0) in face_ids)
        if face_count > 0:
            return 0, 0, 2 ** face_count
    except Exception:
        pass
    return 0, 0, 1.0


_JOKER_HANDLERS: dict[str, callable] = {
    "j_banner": _joker_banner,
    "j_raised_fist": _joker_raised_fist,
    "j_hanging_chad": _joker_hanging_chad,
    "j_mystic_summit": _joker_mystic_summit,
    "j_vampire": _joker_vampire,
    "j_triboulet": _joker_triboulet,
}


def joker_score_estimate(
    jokers: list[dict[str, Any]],
    hand_name: str,
    cards_for_jokers: list[dict[str, Any]] | None = None,
    round_data: dict[str, Any] | None = None,
    *,
    full_hand: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """Estimate joker effects for a given hand.

    Parameters
    ----------
    jokers : list of joker dicts from policy-state
    hand_name : poker hand type string
    cards_for_jokers : optional full card list (not just scoring cards) for
        jokers that reference non-scoring cards (Raised Fist, etc.)
    round_data : optional run data for context (discards_left, hands_left, etc.)
    """
    chips = 0
    mult = 0
    x_mult = 1.0
    sources = []
    cards = full_hand if full_hand is not None else cards_for_jokers

    for joker in jokers:
        if joker.get("debuffed"):
            continue
        ability = joker.get("ability") or {}
        config = joker.get("center_config") or joker.get("config") or {}
        center_key = str(joker.get("center_key") or "")
        name = joker.get("name") or center_key
        j_chips = 0
        j_mult = 0
        j_x_mult = 1.0

        # Apply joker-specific effect from lookup table
        handler = _JOKER_HANDLERS.get(center_key)
        if handler:
            hc, hm, hx = handler(joker, hand_name, cards or [], round_data)
            j_chips += hc
            j_mult += hm
            j_x_mult *= hx

        required_type = config.get("type") or ability.get("type")
        if int_or_none(ability.get("t_chips")) and (not required_type or hand_contains_type(hand_name, str(required_type))):
            j_chips += int_or_none(ability.get("t_chips")) or 0
        if int_or_none(ability.get("t_mult")) and (not required_type or hand_contains_type(hand_name, str(required_type))):
            j_mult += int_or_none(ability.get("t_mult")) or 0

        for key in ("chips", "h_chips"):
            j_chips += int_or_none(ability.get(key)) or 0
        for key in ("mult", "h_mult"):
            j_mult += int_or_none(ability.get(key)) or 0
        for key in ("x_mult", "h_x_mult"):
            value = float_or_none(ability.get(key))
            if value and value != 1:
                j_x_mult *= value

        e_chips, e_mult, e_x_mult = edition_score_bonus(joker.get("edition"))
        j_chips += e_chips
        j_mult += e_mult
        j_x_mult *= e_x_mult

        if j_chips or j_mult or j_x_mult != 1:
            sources.append({"name": name, "chips": j_chips, "mult": j_mult, "x_mult": round(j_x_mult, 3)})
        chips += j_chips
        mult += j_mult
        x_mult *= j_x_mult
    return {"chips": chips, "mult": mult, "x_mult": round(x_mult, 3), "sources": sources}
def score_breakdown(
    cards: list[dict[str, Any]],
    *,
    round_data: dict[str, Any] | None = None,
    poker_hand_values: dict[str, Any] | None = None,
    jokers: list[dict[str, Any]] | None = None,
    full_hand: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    hand_name = classify_cards(cards)
    scoring = scoring_cards(cards, hand_name)
    try:
        hand_value = reliable_scoring.hand_level_value(hand_name, poker_hand_values)
    except reliable_scoring.HandValueUnavailable as exc:
        return {
            "hand_name": hand_name,
            "hand_key": hand_name,
            "hand_value_available": False,
            "hand_value_error": str(exc),
            "hand_value_source": (poker_hand_values or {}).get("source"),
            "estimated_score": None,
        }
    # Blind debuffs: adjust hand level for estimation
    # The Arm (bl_arm) decreases level of played poker hand by 1
    blind_key = ((round_data or {}).get("blind") or {}).get("key", "")
    level_debuff = 0
    if blind_key == "bl_arm":
        level_debuff = 1  # The Arm reduces hand level by 1

    # Use live hand_value["chips"] / ["mult"] directly (already at current level)
    # Only apply blind debuff adjustment when The Arm is active
    effective_level = int(hand_value.get("level", 1)) - level_debuff
    if level_debuff > 0:
        s_chips = int(hand_value.get("base_chips") or hand_value.get("s_chips", 0))
        l_chips = int(hand_value.get("chips_per_level") or hand_value.get("l_chips", 0))
        s_mult = int(hand_value.get("base_mult") or hand_value.get("s_mult", 0))
        l_mult = int(hand_value.get("mult_per_level") or hand_value.get("l_mult", 0))
        base_chips = max(0, s_chips + l_chips * (max(1, effective_level) - 1))
        base_mult = max(1, s_mult + l_mult * (max(1, effective_level) - 1))
    else:
        base_chips = int(hand_value["chips"])
        base_mult = int(hand_value["mult"])
    _debuff_info = {"blind_key": blind_key, "level_debuff": level_debuff, "effective_level": effective_level} if level_debuff > 0 else {}

    card_chips = sum(int(card["base"].get("nominal") or 0) for card in scoring if not card.get("debuffed", False))
    modifier_chips = 0
    modifier_mult = 0
    modifier_x_mult = 1.0
    # Blue seal: +1 chip per blue-seal card played (count all cards, not just scoring)
    blue_seal_count = sum(1 for c in cards if c.get("seal") == "blue")

    # Vampire strips enhancements from non-base cards during play, removing their ability.bonus.
    # Pre-strip enhancement bonuses so estimates match actual game behavior.
    vampire_present = any(
        (j.get("center_key") or "") == "j_vampire" and not j.get("debuffed", False)
        for j in (jokers or [])
    )

    for card in scoring:
        # Temporarily strip enhancement bonus if Vampire is present
        _stripped = None
        if vampire_present:
            center_key = card.get("center_key") or ""
            ability = card.get("ability") or {}
            old_bonus = int_or_none(ability.get("bonus")) or 0
            if old_bonus > 0 and center_key != "c_base":
                _stripped = old_bonus
                ability["bonus"] = 0

        c_chips, c_mult, c_x_mult = card_modifier_score(card)

        # Restore stripped bonus for next iteration safety
        if _stripped is not None:
            ability["bonus"] = _stripped
        modifier_chips += c_chips
        modifier_mult += c_mult
        modifier_x_mult *= c_x_mult
    # Add blue seal chips (after card modifiers but before joker mult)
    modifier_chips += blue_seal_count
    joker_estimate = joker_score_estimate(jokers or [], hand_name, cards, round_data, full_hand=full_hand or cards)
    total_chips = base_chips + card_chips + modifier_chips + int(joker_estimate["chips"])
    total_mult = base_mult + modifier_mult + int(joker_estimate["mult"])
    total_x_mult = modifier_x_mult * float(joker_estimate["x_mult"])
    return {
        "hand_name": hand_name,
        "hand_key": hand_name,
        "display_name": hand_value.get("display_name") or hand_name,
        "hand_level": hand_value.get("level"),
            "effective_hand_level": max(1, effective_level),
        "hand_value_available": True,
        "hand_value_source": poker_hand_values.get("source"),
        "hand_value_schema": poker_hand_values.get("schema"),
        "base_chips": base_chips,
        "base_mult": base_mult,
        "card_chips": card_chips,
        "modifier_chips": modifier_chips,
        "modifier_mult": modifier_mult,
        "modifier_x_mult": round(modifier_x_mult, 3),
        "joker_chips": joker_estimate["chips"],
        "joker_mult": joker_estimate["mult"],
        "joker_x_mult": joker_estimate["x_mult"],
        "joker_sources": joker_estimate["sources"],
        "total_chips": total_chips,
        "total_mult": total_mult,
        "total_x_mult": round(total_x_mult, 3),
        "estimated_score": int(total_chips * total_mult * total_x_mult),
        "scoring_card_indices": [int(card["index"]) for card in scoring],
    }


def ranked_play_candidates(
    hand: list[dict[str, Any]],
    limit: int = 40,
    *,
    round_data: dict[str, Any] | None = None,
    poker_hand_values: dict[str, Any] | None = None,
    jokers: list[dict[str, Any]] | None = None,
) -> list[dict[str, Any]]:
    if limit <= 0:
        return []
    candidates: list[dict[str, Any]] = []
    max_count = min(5, len(hand))
    for count in range(1, max_count + 1):
        for combo in itertools.combinations(hand, count):
            combo_cards = list(combo)
            breakdown = score_breakdown(
                combo_cards,
                round_data=round_data,
                poker_hand_values=poker_hand_values,
                jokers=jokers,
                # Held-in-hand effects trigger from cards remaining after the
                # submitted cards leave the hand (for example Raised Fist).
                full_hand=[card for card in hand if card not in combo_cards],
            )
            hand_name = str(breakdown["hand_name"])
            candidates.append(
                {
                    "action": "play",
                    "cards": [int(card["index"]) for card in combo_cards],
                    "card_refs": [f"{int(card['index'])}_{card_instance_id(card)}" for card in combo_cards],
                    "card_names": [card_label(card) for card in combo_cards],
                    "hand_name": hand_name,
                    "estimated_score": breakdown.get("estimated_score"),
                    "score_breakdown": breakdown,
                    "scoring_cards": breakdown.get("scoring_card_indices", []),
                    "scoring_card_names": [card_label(c) for c in combo_cards if int(c["index"]) in breakdown.get("scoring_card_indices", [])],
                    "command": hand_card_command("play", combo_cards),
                }
            )
    candidates.sort(
        key=lambda item: (
            int(item.get("estimated_score") if item.get("estimated_score") is not None else -1),
            len(item["scoring_cards"]),
            -len(item["cards"]),
        ),
        reverse=True,
    )
    return candidates[:limit]


def discard_candidate_score(hand: list[dict[str, Any]], discarded_indices: set[int]) -> tuple[int, int, int]:
    kept = [card for card in hand if int(card["index"]) not in discarded_indices]
    if not kept:
        return (0, 0, 0)

    straight_keep = best_straight_window_keep(kept)
    suit_counts: dict[str, int] = {}
    rank_counts: dict[int, int] = {}
    for card in kept:
        base = card.get("base") or {}
        suit_counts[str(base.get("suit"))] = suit_counts.get(str(base.get("suit")), 0) + 1
        rank = int(base.get("rank_id") or base.get("id") or 0)
        rank_counts[rank] = rank_counts.get(rank, 0) + 1

    best_flush = max(suit_counts.values(), default=0)
    pair_pressure = sum(count * count for count in rank_counts.values())
    high_card_sum = sum(int((card.get("base") or {}).get("nominal") or 0) for card in kept)
    return (max(len(straight_keep), best_flush), pair_pressure, high_card_sum)


def move_joker_actions(jokers: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Generate legal move_joker actions for reordering joker slots."""
    actions: list[dict[str, Any]] = []
    if len(jokers) < 2:
        return actions
    for joker in jokers:
        from_index = int(joker["index"])
        for to_index in range(1, len(jokers) + 1):
            if to_index == from_index:
                continue
            instance_id = card_instance_id(joker)
            actions.append({
                "action": "move_joker",
                "from_index": from_index,
                "to_index": to_index,
                "card_ref": f"{from_index}_{instance_id}",
                "joker": compact_card(joker),
                "command": {
                    "action": "move_joker",
                    "from_index": from_index,
                    "to_index": to_index,
                    "card_id": instance_id,
                },
            })
    return actions

def ranked_discard_candidates(hand: list[dict[str, Any]], limit: int = 25) -> list[dict[str, Any]]:
    candidates: list[dict[str, Any]] = []
    max_count = min(5, len(hand))
    for count in range(1, max_count + 1):
        for combo in itertools.combinations(hand, count):
            combo_cards = sorted(list(combo), key=lambda card: int(card["index"]))
            indices = {int(card["index"]) for card in combo_cards}
            score = discard_candidate_score(hand, indices)
            candidates.append(
                {
                    "action": "discard",
                    "cards": sorted(indices),
                    "card_refs": [f"{int(card['index'])}_{card_instance_id(card)}" for card in combo_cards],
                    "card_names": [card_label(card) for card in combo_cards],
                    "draw_heuristic": {
                        "shape_score": score[0],
                        "pair_pressure": score[1],
                        "kept_nominal": score[2],
                    },
                    "command": hand_card_command("discard", combo_cards),
                }
            )
    candidates.sort(
        key=lambda item: (
            item["draw_heuristic"]["shape_score"],
            item["draw_heuristic"]["pair_pressure"],
            item["draw_heuristic"]["kept_nominal"],
            len(item["cards"]),
        ),
        reverse=True,
    )
    heuristic = choose_discard(hand)
    if heuristic:
        heuristic_set = set(heuristic)
        for candidate in candidates:
            if set(candidate["cards"]) == heuristic_set:
                candidate["heuristic_pick"] = True
                break
    return candidates[:limit]


def best_straight_window_keep(hand: list[dict[str, Any]]) -> set[int]:
    best_cards: list[dict[str, Any]] = []
    best_key = (-1, -1)
    for start in range(1, 11):
        ranks = set(range(start, start + 5))
        cards: list[dict[str, Any]] = []
        for card in hand:
            rank = int(card["base"].get("rank_id") or card["base"].get("id") or 0)
            rank_options = {rank}
            if rank == 14:
                rank_options.add(1)
            if rank_options & ranks:
                cards.append(card)
        key = (len(cards), sum(int(card["base"].get("nominal") or 0) for card in cards))
        if key > best_key:
            best_key = key
            best_cards = cards
    return {int(card["index"]) for card in best_cards}


def choose_discard(hand: list[dict[str, Any]]) -> list[int]:
    keep = best_straight_window_keep(hand)
    if len(keep) < 5:
        ranked = sorted(
            hand,
            key=lambda card: int(card["base"].get("nominal") or 0),
            reverse=True,
        )
        for card in ranked:
            keep.add(int(card["index"]))
            if len(keep) >= 5:
                break
    discards = [
        int(card["index"])
        for card in sorted(hand, key=lambda card: int(card["base"].get("nominal") or 0))
        if int(card["index"]) not in keep
    ]
    return discards[:5]


def ui_has(data: dict[str, Any], *, ui_id: str | None = None, button: str | None = None) -> bool:
    for node in data.get("ui") or []:
        if ui_id is not None and str(node.get("id")) != ui_id:
            continue
        if button is not None and str(node.get("button")) != button:
            continue
        if node.get("disabled"):
            continue
        return True
    return False


def legal_actions_for_observation(
    data: dict[str, Any],
    play_limit: int = 40,
    discard_limit: int = 25,
    target_limit: int = 60,
) -> list[dict[str, Any]]:
    game = data.get("game") or {}
    state = game.get("state_name")
    ready = data.get("ready") or {}
    round_data = data.get("round") or {}
    areas = data.get("areas") or {}
    actions: list[dict[str, Any]] = []

    if ready.get("tutorial_complete") is False:
        actions.append(
            {
                "action": "skip_tutorial",
                "reason": "tutorial is incomplete",
                "command": {"action": "skip_tutorial"},
            }
        )
        return actions

    if state in {"SPLASH", "MENU"} and ui_has(data, button="start_setup_run"):
        if ready.get("current_setup") and ready.get("current_setup") != "New Run":
            actions.append(
                {
                    "action": "setup_new_run",
                    "reason": f"current setup is {ready.get('current_setup')}",
                    "command": {"action": "setup_new_run"},
                }
            )
        else:
            actions.append(
                {
                    "action": "start_setup_run",
                    "reason": "New Run setup is ready",
                    "command": {"action": "ui_click", "button": "start_setup_run"},
                }
            )
        return actions

    if state in {"SPLASH", "MENU"}:
        if state == "MENU" and not ready.get("main_menu_ui_present"):
            # If we have a saved game and tutorial is complete, allow direct start_run
            if (
                ready.get("saved_game_present")
                and str(ready.get("saved_game_seed") or "") != ALLOWED_SEED
            ):
                actions.append(
                    {
                        "action": "setup_new_run",
                        "reason": "saved run uses a disallowed seed; open a protected fresh-run setup",
                        "command": {"action": "setup_new_run"},
                    }
                )
            elif ready.get("saved_game_present") and ready.get("tutorial_complete"):
                actions.append(
                    {
                        "action": "start_run",
                        "reason": "MENU state with saved game present and tutorial complete; use Lua bridge start_run directly",
                        "command": {"action": "start_run"},
                    }
                )
            else:
                actions.append(
                    {
                        "action": "ensure_menu_ui",
                        "reason": "main menu state without tracked menu UI",
                        "command": {"action": "ensure_menu_ui"},
                    }
                )
        # When overlay_menu_present with seeded run setup, allow start_run with seed
        if ready.get("overlay_menu_present") and ui_has(data, ui_id="run_select_seeded_input"):
            actions.append(
                {
                    "action": "start_run",
                    "reason": "MENU state with overlay menu open (seeded run setup)",
                    "command": {"action": "start_run", "seed": ALLOWED_SEED},
                }
            )
        if ui_has(data, ui_id="main_menu_play"):
            actions.append(
                {
                    "action": "open_run_setup",
                    "reason": "main menu Play button is available",
                    "command": {"action": "ui_click", "ui_id": "main_menu_play"},
                }
            )
        return actions


    if state == "GAME_OVER":
        if ui_has(data, button="continue_unlock"):
            actions.append(
                {
                    "action": "dismiss_unlock_overlay",
                    "command": {"action": "ui_click", "button": "continue_unlock"},
                }
            )
        if ui_has(data, button="start_setup_run"):
            actions.append(
                {
                    "action": "start_setup_run",
                    "reason": "run setup is open after game over",
                    "command": {"action": "ui_click", "button": "start_setup_run"},
                }
            )
        if ui_has(data, button="notify_then_setup_run"):
            actions.append(
                {
                    "action": "new_run_after_game_over",
                    "command": {"action": "ui_click", "button": "notify_then_setup_run"},
                }
            )
        # When overlay menu is present with seeded run setup, allow start_run
        if ready.get("overlay_menu_present") and ui_has(data, ui_id="run_select_seeded_input"):
            actions.append(
                {
                    "action": "start_run",
                    "reason": "GAME_OVER with overlay menu open (seeded run setup)",
                    "command": {"action": "start_run", "seed": ALLOWED_SEED},
                }
            )
        return actions

    if state == "BLIND_SELECT" and ready.get("blind_select_ready"):
        choices = (round_data.get("blind_choices") or {})
        boss_choice = choices.get("Boss") or {}
        if can_reroll_boss_from_run(round_data) and boss_choice.get("key"):
            actions.append(
                {
                    "action": "reroll_boss",
                    "cost": 10,
                    "boss_before": boss_choice,
                    "command": {"action": "reroll_boss"},
                }
            )
        for blind_type in ("Small", "Big", "Boss"):
            choice = choices.get(blind_type) or {}
            if choice.get("state") == "Select":
                actions.append(
                    {
                        "action": "select_blind",
                        "blind": blind_type,
                        "blind_name": choice.get("name"),
                        "chips_mult": choice.get("mult"),
                        "reward": choice.get("dollars"),
                        "boss": choice.get("boss"),
                        "command": {"action": "ui_click", "ui_id": "select_blind_button"},
                    }
                )
                if blind_type != "Boss":
                    actions.append(
                        {
                            "action": "skip_blind",
                            "blind": blind_type,
                            "tag": choice.get("tag"),
                            "tag_name": choice.get("tag_name"),
                            "tag_effect": choice.get("tag_effect"),
                            "tag_config": choice.get("tag_config"),
                            "command": {"action": "skip_blind"},
                        }
                    )
                break
        return actions

    if state == "ROUND_EVAL" and ready.get("round_eval_ready"):
        if ui_has(data, ui_id="cash_out_button"):
            actions.append(
                {
                    "action": "cash_out",
                    "dollars_after_round": round_data.get("dollars"),
                    "command": {"action": "ui_click", "ui_id": "cash_out_button"},
                }
            )
        return actions

    if state == "SHOP" and ready.get("shop_ready"):
        dollars = int(round_data.get("dollars") or 0)
        for area_name in ("shop_jokers", "shop_vouchers", "shop_booster"):
            for card in areas.get(area_name) or []:
                cost = int(card.get("cost") or 0)
                affordable = cost <= dollars
                if affordable:
                    actions.append(
                        {
                            "action": "buy",
                            "area": area_name,
                            "index": card.get("index"),
                            "card_ref": card_ref(card),
                            "card": compact_card(card),
                            "affordable": True,
                            "command": card_command("buy", area_name, card),
                        }
                    )
        for card in areas.get("consumeables") or []:
            actions.extend(
                use_actions_for_card(
                    area_name="consumeables",
                    card=card,
                    hand=areas.get("hand") or [],
                    target_limit=max(0, target_limit),
                )
            )
        actions.extend(sell_actions_for_area("consumeables", areas.get("consumeables") or []))
        actions.extend(sell_actions_for_area("jokers", areas.get("jokers") or []))
        actions.extend(move_card_actions("jokers", areas.get("jokers") or []))
        actions.extend(move_joker_actions(areas.get("jokers") or []))
        if ui_has(data, ui_id="next_round_button"):
            actions.append(
                {
                    "action": "next_round",
                    "command": {"action": "ui_click", "ui_id": "next_round_button"},
                }
            )
        if dollars >= int(round_data.get("reroll_cost") or 0):
            actions.append(
                {
                    "action": "reroll_shop",
                    "cost": round_data.get("reroll_cost"),
                    "command": {"action": "reroll_shop"},
                }
            )
        return actions

    if state in {"TAROT_PACK", "PLANET_PACK", "SPECTRAL_PACK", "STANDARD_PACK", "BUFFOON_PACK", "SMODS_BOOSTER_OPENED"}:
        hand = areas.get("hand") or []
        for card in areas.get("pack_cards") or []:
            actions.extend(
                use_actions_for_card(
                    area_name="pack_cards",
                    card=card,
                    hand=hand,
                    target_limit=max(0, target_limit),
                )
            )
        actions.append({"action": "skip_booster", "command": {"action": "skip_booster"}})
        return actions

    if state == "SELECTING_HAND":
        hand = areas.get("hand") or []
        if hand:
            if int(round_data.get("hands_left") or 0) > 0:
                actions.extend(
                    ranked_play_candidates(
                        hand,
                        play_limit,
                        round_data=round_data,
                        poker_hand_values=data.get("poker_hands"),
                        jokers=areas.get("jokers") or [],
                    )
                )
            if int(round_data.get("discards_left") or 0) > 0:
                actions.extend(ranked_discard_candidates(hand, discard_limit))
            actions.extend(move_card_actions("hand", hand))
        for area_name in ("consumeables", "jokers"):
            for card in areas.get(area_name) or []:
                if area_name == "consumeables":
                    actions.extend(
                        use_actions_for_card(
                            area_name=area_name,
                            card=card,
                            hand=hand,
                            target_limit=max(0, target_limit),
                        )
                    )
                actions.extend(sell_actions_for_area(area_name, [card]))
        actions.extend(move_card_actions("jokers", areas.get("jokers") or []))
        actions.extend(move_joker_actions(areas.get("jokers") or []))
        return actions

    return actions


def action_id_part(value: Any) -> str:
    text = str(value).strip().lower()
    cleaned = []
    for char in text:
        if char.isalnum():
            cleaned.append(char)
        elif char in {"_", "-"}:
            cleaned.append(char)
        else:
            cleaned.append("_")
    return "".join(cleaned).strip("_") or "none"


def base_policy_action_id(action: dict[str, Any], ordinal: int) -> str:
    action_name = action_id_part(action.get("action") or "action")
    command = action.get("command") or {}

    if action_name in {"play", "discard"}:
        cards = action.get("cards") or command.get("cards") or []
        card_refs = action.get("card_refs")
        if card_refs:
            return f"{action_name}:{'-'.join(action_id_part(ref) for ref in card_refs)}"
        return f"{action_name}:{'-'.join(str(int(card)) for card in cards)}"
    if action_name in {"buy", "sell", "use"}:
        area = action.get("area") or command.get("area") or "area"
        index = action.get("index") or command.get("index") or "index"
        card = action.get("card") or {}
        card_ref_value = action.get("card_ref")
        if not card_ref_value and isinstance(card, dict) and card.get("instance_id") is not None:
            card_ref_value = f"{card.get('index')}_{card.get('instance_id')}"
        base_card = card_ref_value or index
        base = f"{action_name}:{action_id_part(area)}:{action_id_part(base_card)}"
        if action_name == "use":
            target_refs = action.get("target_refs")
            if target_refs:
                return f"{base}:on:{'-'.join(action_id_part(ref) for ref in target_refs)}"
            targets = action.get("targets") or command.get("targets") or []
            if targets:
                return f"{base}:on:{'-'.join(str(int(target)) for target in targets)}"
        return base
    if action_name == "move_card":
        area = action.get("area") or command.get("area") or "area"
        card = action.get("card_ref") or action.get("from_index") or command.get("from_index") or "card"
        to_index = action.get("to_index") or command.get("to_index") or "to"
        return f"move_card:{action_id_part(area)}:{action_id_part(card)}:to:{action_id_part(str(to_index))}"

    if action_name == "move_joker":
        from_idx = action.get("from_index") or command.get("from_index") or "?"
        to_idx = action.get("to_index") or command.get("to_index") or "?"
        return f"move_joker:{action_id_part(str(from_idx))}:to:{action_id_part(str(to_idx))}"
    if action_name in {"select_blind", "skip_blind"}:
        return f"{action_name}:{action_id_part(action.get('blind') or 'current')}"
    if action_name in {
        "skip_tutorial",
        "setup_new_run",
        "ensure_menu_ui",
        "open_run_setup",
        "start_setup_run",
        "cash_out",
        "next_round",
        "reroll_shop",
        "reroll_boss",
        "skip_booster",
        "dismiss_unlock_overlay",
        "new_run_after_game_over",
    }:
        return action_name

    command_action = command.get("action")
    if command_action:
        return f"{action_name}:{action_id_part(command_action)}:{ordinal}"
    return f"{action_name}:{ordinal}"


def annotate_policy_actions(actions: list[dict[str, Any]]) -> list[dict[str, Any]]:
    counts: dict[str, int] = {}
    annotated: list[dict[str, Any]] = []
    for ordinal, action in enumerate(actions, 1):
        copied = dict(action)
        base_id = base_policy_action_id(copied, ordinal)
        count = counts.get(base_id, 0) + 1
        counts[base_id] = count
        copied["id"] = base_id if count == 1 else f"{base_id}:{count}"
        annotated.append(copied)
    return annotated


def round_voucher_keys(round_data: dict[str, Any]) -> set[str]:
    return {
        str(item.get("key"))
        for item in round_data.get("used_vouchers") or []
        if item.get("key")
    }


def interest_cap_from_round(round_data: dict[str, Any]) -> int:
    vouchers = round_voucher_keys(round_data)
    if "v_money_tree" in vouchers:
        return 20
    if "v_seed_money" in vouchers:
        return 10
    return 5


def interest_metrics(round_data: dict[str, Any]) -> dict[str, Any]:
    dollars = int_or_none(round_data.get("dollars")) or 0
    cap = interest_cap_from_round(round_data)
    full_interest_floor = cap * 5
    current_interest = max(0, min(cap, dollars // 5))
    current_floor = current_interest * 5
    next_floor = min(full_interest_floor, (current_interest + 1) * 5)
    return {
        "dollars": dollars,
        "interest_cap": cap,
        "current_interest": current_interest,
        "full_interest_floor": full_interest_floor,
        "current_interest_floor": current_floor,
        "dollars_to_next_interest": max(0, next_floor - dollars) if current_interest < cap else 0,
        "spendable_without_losing_current_interest": max(0, dollars - current_floor),
        "spendable_without_losing_full_interest": max(0, dollars - full_interest_floor),
    }


def score_pressure_metrics(round_data: dict[str, Any], blind: dict[str, Any], best_play: dict[str, Any] | None) -> dict[str, Any]:
    chips = int_or_none(round_data.get("chips")) or 0
    required = int_or_none(blind.get("chips")) or 0
    remaining = max(0, required - chips)
    best_score = int_or_none((best_play or {}).get("estimated_score")) or 0
    hands_left = int_or_none(round_data.get("hands_left")) or 0
    return {
        "chips": chips,
        "blind_chips_required": required,
        "chips_remaining": remaining,
        "hands_left": hands_left,
        "best_play_estimated_score": best_score,
        "best_play_clears_blind": required > 0 and chips + best_score >= required,
        "best_play_surplus": chips + best_score - required if required else None,
        "estimated_best_plays_needed": ((remaining + max(1, best_score) - 1) // max(1, best_score)) if remaining else 0,
    }


def slot_metrics(areas: dict[str, Any]) -> dict[str, Any]:
    slots = areas.get("state") or {}
    return {
        name: {
            "count": (slots.get(name) or {}).get("count"),
            "limit": (slots.get(name) or {}).get("limit"),
            "open": max(
                0,
                (int_or_none((slots.get(name) or {}).get("limit")) or 0)
                - (int_or_none((slots.get(name) or {}).get("count")) or 0),
            ),
        }
        for name in ("jokers", "consumeables")
    }


def strategy_context(round_data: dict[str, Any], blind: dict[str, Any], areas: dict[str, Any], best_play: dict[str, Any] | None) -> dict[str, Any]:
    ante = int_or_none(round_data.get("ante")) or 0
    return {
        "economy": interest_metrics(round_data),
        "score_pressure": score_pressure_metrics(round_data, blind, best_play),
        "slots": slot_metrics(areas),
        "most_played_poker_hand": round_data.get("most_played_poker_hand"),
        "run_phase": "early" if ante <= 2 else "mid" if ante <= 5 else "late",
        "joker_order_hint": ["utility/economy", "chips", "add_mult", "multiply_mult"],
    }


def decision_checks(
    round_data: dict[str, Any],
    blind: dict[str, Any],
    areas: dict[str, Any],
    legal_actions: list[dict[str, Any]],
) -> dict[str, Any]:
    """Force decision-time review of effects agents commonly overlook."""
    def matching(action_name: str, area: str = "") -> list[dict[str, Any]]:
        return [
            {"action_id": action.get("id"), "card": action.get("card") or action.get("joker")}
            for action in legal_actions
            if action.get("action") == action_name and (not area or action.get("area") == area)
        ]

    owned_consumables = [compact_card(card) for card in areas.get("consumeables") or []]
    shop_consumables = [
        action for action in matching("buy")
        if str((action.get("card") or {}).get("set") or "") in {"Tarot", "Planet", "Spectral"}
    ]
    consumables = []
    for card in owned_consumables:
        index = card.get("index")
        actions = [
            entry for entry in matching("use", "consumeables") + matching("sell", "consumeables")
            if (entry.get("card") or {}).get("index") == index
        ]
        consumables.append({
            "card": card,
            "priority": consumable_priority(card, {"run": round_data}),
            "legal_actions": actions,
            "must_evaluate_before_exit": True,
        })

    choices = round_data.get("blind_choices") or {}
    boss_choice = choices.get("Boss") or choices.get("boss") or {}
    current_is_boss = bool(blind.get("boss"))
    debuffed_cards = [compact_card(card) for card in areas.get("hand") or [] if card.get("debuffed")]
    debuffed_jokers = [compact_card(card) for card in areas.get("jokers") or [] if card.get("debuffed")]
    return {
        "consumables": {
            "required": bool(owned_consumables or shop_consumables),
            "instruction": "Evaluate every owned use/sell action and every shop consumable purchase before exiting or advancing.",
            "owned": consumables,
            "shop_purchase_actions": shop_consumables,
        },
        "ordering": {
            "required_before_close_play": bool(areas.get("hand") and (areas.get("jokers") or [])),
            "instruction": "Evaluate hand and Joker trigger order when a scoring effect can depend on sequence; do not move cards by default, but do not dismiss legal reorder actions.",
            "hand_order": [compact_card(card) for card in areas.get("hand") or []],
            "joker_order": [compact_card(card) for card in areas.get("jokers") or []],
            "move_card_actions": matching("move_card"),
            "move_joker_actions": matching("move_joker"),
            "estimate_caveat": "Play estimates may not model every ordering interaction; verify relevant ordering when margin is tight.",
        },
        "boss_debuff": {
            "required": current_is_boss or bool(boss_choice),
            "instruction": "Before selecting or playing a Boss Blind, inspect its live effect, lookup_rule details, debuffed cards/Jokers, and legal boss-reroll actions.",
            "current_blind": {
                "name": blind.get("name"), "key": blind.get("key"), "boss": current_is_boss,
                "effect": blind.get("effect"), "disabled": blind.get("disabled"),
            },
            "upcoming_boss": boss_choice,
            "debuffed_cards": debuffed_cards,
            "debuffed_jokers": debuffed_jokers,
            "select_actions": matching("select_blind"),
            "reroll_actions": matching("reroll_boss"),
        },
    }


def strategy_card_from_action(action: dict[str, Any]) -> dict[str, Any]:
    card = action.get("card")
    return card if isinstance(card, dict) else {}


def booster_pack_eval(
    card: dict[str, Any],
    *,
    reserve: int,
    run_phase: str,
    joker_slot_open: bool,
    spends_interest_buffer: bool,
) -> dict[str, Any]:
    """Attach compact decision context to a booster purchase.

    Pack contents remain unknown until purchase. This intentionally reports
    opportunity and economy facts instead of inventing an expected value.
    """
    key = str(card.get("center_key") or "")
    pack_kind = (
        "joker" if "buffoon" in key else
        "planet" if "celestial" in key else
        "tarot" if "arcana" in key else
        "spectral" if "spectral" in key else
        "playing_card" if "standard" in key else
        "unknown"
    )
    return {
        "kind": pack_kind,
        "contents_unknown": True,
        "reserve_after_purchase": reserve,
        "run_phase": run_phase,
        "joker_slot_open": joker_slot_open,
        "spends_interest_buffer": spends_interest_buffer,
        "slot_warning": pack_kind == "joker" and not joker_slot_open,
    }


def consumable_priority(card: dict[str, Any], context: dict[str, Any]) -> dict[str, Any]:
    """Describe consumable fit without pretending to solve target selection."""
    card_set = str(card.get("set") or (card.get("ability") or {}).get("set") or "")
    effect = card.get("effect") or {}
    name = card.get("name") or effect.get("name") or card.get("center_key")
    return {
        "name": name,
        "kind": card_set.lower() or "unknown",
        "most_played_hand": ((context.get("run") or {}).get("most_played_poker_hand")),
        "requires_target_evaluation": card_set in {"Tarot", "Spectral"},
        "effect": effect.get("text") if isinstance(effect, dict) else None,
    }


def action_economy_annotation(action: dict[str, Any], context: dict[str, Any]) -> dict[str, Any]:
    economy = context.get("economy") or {}
    dollars = int_or_none(economy.get("dollars")) or 0
    current_interest = int_or_none(economy.get("current_interest")) or 0
    card = strategy_card_from_action(action)
    command = action.get("command") or {}
    action_name = action.get("action")
    amount = 0
    if action_name == "buy":
        amount = -(int_or_none(card.get("cost")) or 0)
    elif action_name == "reroll_shop":
        amount = -(int_or_none(action.get("cost")) or 0)
    elif action_name == "sell":
        amount = int_or_none(card.get("sell_cost")) or 0
    after = dollars + amount
    cap = int_or_none(economy.get("interest_cap")) or 5
    interest_after = max(0, min(cap, after // 5))
    return {
        "dollars_after": after,
        "interest_after": interest_after,
        "interest_delta": interest_after - current_interest,
        "spends_interest_buffer": amount < 0 and abs(amount) > (int_or_none(economy.get("spendable_without_losing_current_interest")) or 0),
        "source": command.get("action"),
    }


def action_strategy_annotation(action: dict[str, Any], context: dict[str, Any]) -> dict[str, Any]:
    annotation: dict[str, Any] = {}
    action_name = action.get("action")
    card = strategy_card_from_action(action)
    card_set = str(card.get("set") or "")
    center_key = str(card.get("center_key") or "")

    if action_name in {"buy", "sell", "reroll_shop"}:
        annotation["economy"] = action_economy_annotation(action, context)

    if action_name == "buy":
        annotation["fills_slot"] = "jokers" if card_set == "Joker" else "consumeables" if card_set in {"Tarot", "Planet", "Spectral"} else None
        if card_set in {"Tarot", "Planet", "Spectral"}:
            annotation["consumable_priority"] = consumable_priority(card, {"run": {"most_played_poker_hand": context.get("most_played_poker_hand")}})
        if action.get("area") == "shop_vouchers":
            annotation["voucher"] = center_key
        if action.get("area") == "shop_booster":
            annotation["booster"] = center_key
            economy = annotation.get("economy") or {}
            slots = context.get("slots") or {}
            joker_slots = slots.get("jokers") or {}
            annotation["pack"] = booster_pack_eval(
                card,
                reserve=int_or_none(economy.get("dollars_after")) or 0,
                run_phase=str(context.get("run_phase") or ""),
                joker_slot_open=(int_or_none(joker_slots.get("open")) or 0) > 0,
                spends_interest_buffer=bool(economy.get("spends_interest_buffer")),
            )

    if action_name in {"use", "sell"} and action.get("area") == "consumeables":
        annotation["consumable_priority"] = consumable_priority(card, {"run": {"most_played_poker_hand": context.get("most_played_poker_hand")}})

    if action_name == "play":
        pressure = context.get("score_pressure") or {}
        estimated = int_or_none(action.get("estimated_score")) or 0
        required_remaining = int_or_none(pressure.get("chips_remaining")) or 0
        breakdown = action.get("score_breakdown") or {}
        annotation["score"] = {
            "estimated_score": estimated,
            "clears_blind": required_remaining > 0 and estimated >= required_remaining,
            "surplus_over_remaining": estimated - required_remaining if required_remaining else None,
            "total_chips": breakdown.get("total_chips"),
            "total_mult": breakdown.get("total_mult"),
            "total_x_mult": breakdown.get("total_x_mult"),
            "joker_sources": breakdown.get("joker_sources") or [],
        }

    if action_name == "discard":
        annotation["draw"] = action.get("draw_heuristic")

    if action_name == "skip_blind":
        tag_key = str(action.get("tag") or "")
        phase = str(context.get("run_phase") or "")
        priority = {
            "tag_investment": 90,
            "tag_coupon": 82,
            "tag_juggle": 65,
            "tag_handy": 55,
            "tag_garbage": 55,
            "tag_voucher": 50,
            "tag_charm": 48,
            "tag_meteor": 48,
            "tag_ethereal": 48,
            "tag_buffoon": 48,
            "tag_negative": 80 if phase in {"mid", "late"} else 45,
            "tag_polychrome": 75 if phase in {"mid", "late"} else 42,
            "tag_foil": 58,
            "tag_holo": 58,
        }.get(tag_key, 30)
        annotation["skip_tag"] = {
            "tag": tag_key or None,
            "priority": priority,
            "phase": phase,
        }

    return annotation


def annotate_actions_with_strategy(actions: list[dict[str, Any]], context: dict[str, Any]) -> list[dict[str, Any]]:
    annotated = []
    for action in actions:
        copied = dict(action)
        strategy = action_strategy_annotation(copied, context)
        if strategy:
            copied["strategy"] = strategy
        annotated.append(copied)
    return annotated



def build_policy_state(
    data: dict[str, Any],
    play_limit: int = 40,
    discard_limit: int = 25,
    target_limit: int = 60,
) -> dict[str, Any]:
    game = data.get("game") or {}
    ready = data.get("ready") or {}
    round_data = data.get("round") or {}
    blind = data.get("blind") or {}
    areas = data.get("areas") or {}
    hand = areas.get("hand") or []
    poker_hand_values = data.get("poker_hands") or {}
    best_play = None
    jokers = areas.get("jokers") or []
    if hand:
        # ranked_play_candidates already scores every combo via score_breakdown,
        # so we avoid a second full enumeration by using its top entry as best_play.
        candidates = ranked_play_candidates(
            hand,
            play_limit,
            round_data=round_data,
            poker_hand_values=poker_hand_values,
            jokers=jokers,
        )
        if candidates:
            top = candidates[0]
            best_play = {
                "cards": top["cards"],
                "hand_name": top["hand_name"],
                "estimated_score": top["estimated_score"],
                "score_breakdown": top.get("score_breakdown"),
                "command": top["command"],
            }

    strategy = strategy_context(round_data, blind, areas, best_play)
    legal_actions = annotate_actions_with_strategy(
        annotate_policy_actions(
            legal_actions_for_observation(data, play_limit, discard_limit, target_limit)
        ),
        strategy,
    )
    strategy["decision_checks"] = decision_checks(round_data, blind, areas, legal_actions)
    return {
        "schema": POLICY_SCHEMA,
        "bridge": data.get("bridge"),
        "game": {
            "state": game.get("state_name"),
            "stage": game.get("stage_name"),
            "speed": game.get("speed"),
        },
        "ready": {
            "state_complete": ready.get("state_complete"),
            "controller_locked": ready.get("controller_locked"),
            "event_queue_empty": ready.get("event_queue_empty"),
            "event_queue_total": (ready.get("event_queues") or {}).get("total"),
            "tutorial_complete": ready.get("tutorial_complete"),
            "overlay_tutorial_present": ready.get("overlay_tutorial_present"),
            "current_setup": ready.get("current_setup"),
            "profile": ready.get("profile"),
            "saved_game_present": ready.get("saved_game_present"),
            "saved_game_loaded": ready.get("saved_game_loaded"),
            "saved_game_seed": ready.get("saved_game_seed"),
        },
        "run": {
            "seed": round_data.get("seed"),
            "ante": round_data.get("ante"),
            "round": round_data.get("round"),
            "stake": round_data.get("stake"),
            "seeded": round_data.get("seeded"),
            "won": round_data.get("won"),
            "dollars": round_data.get("dollars"),
            "bankrupt_at": round_data.get("bankrupt_at"),
            "chips": round_data.get("chips"),
            "hands_left": round_data.get("hands_left"),
            "discards_left": round_data.get("discards_left"),
            "hands_played": round_data.get("hands_played"),
            "discards_used": round_data.get("discards_used"),
            "hands_played_total": round_data.get("hands_played_total"),
            "boss_rerolled": round_data.get("boss_rerolled"),
            "pack_choices": round_data.get("pack_choices"),
            "pack_size": round_data.get("pack_size"),
            "skips": round_data.get("skips"),
            "unused_discards": round_data.get("unused_discards"),
            "reroll_cost": round_data.get("reroll_cost"),
            "free_rerolls": round_data.get("free_rerolls"),
            "blind_on_deck": round_data.get("blind_on_deck"),
            "most_played_poker_hand": round_data.get("most_played_poker_hand"),
            "blind": {
                "name": blind.get("name"),
                "key": blind.get("key"),
                "chips_required": blind.get("chips"),
                "reward": blind.get("dollars"),
                "boss": blind.get("boss"),
                "disabled": blind.get("disabled"),
                "effect": blind.get("effect"),
            },
            "blind_choices": round_data.get("blind_choices"),
            "current_hand": round_data.get("current_hand"),
            "current_voucher": round_data.get("current_voucher"),
            "used_vouchers": round_data.get("used_vouchers") or [],
            "active_tags": round_data.get("active_tags") or [],
            "starting_params": round_data.get("starting_params"),
            "modifiers": round_data.get("modifiers"),
            "pool_flags": round_data.get("pool_flags"),
            "round_scores": round_data.get("round_scores"),
            "poker_hands": round_data.get("hands"),
        },
        "poker_hand_values": poker_hand_values,
        "areas": {
            "hand": [compact_card(card) for card in hand],
            "jokers": [compact_card(card) for card in areas.get("jokers") or []],
            "consumeables": [compact_card(card) for card in areas.get("consumeables") or []],
            "shop_jokers": [compact_card(card) for card in areas.get("shop_jokers") or []],
            "shop_vouchers": [compact_card(card) for card in areas.get("shop_vouchers") or []],
            "shop_booster": [compact_card(card) for card in areas.get("shop_booster") or []],
            "pack_cards": [compact_card(card) for card in areas.get("pack_cards") or []],
            "deck_count": areas.get("deck_count"),
            "discard_count": areas.get("discard_count"),
            "deck_remaining": areas.get("deck_summary"),
            "discard_pile": areas.get("discard_summary"),
            "discard": [compact_card(card) for card in areas.get("discard") or []],
            "slots": areas.get("state"),
        },
        "strategy": strategy,
        "hand_analysis": {
            "best_play": best_play,
            "play_candidates": candidates if hand else [],
            "discard_candidates": ranked_discard_candidates(hand, discard_limit) if hand else [],
        },
        "legal_actions": legal_actions,
        "play_history": (data.get("bridge") or {}).get("play_history") or [],
        "autoplay_suggestion": None,
    }


AVAILABLE_FIELDS = [
    "state", "chips", "dollars", "hands_left", "discards_left",
    "hand", "jokers", "blind", "legal_actions", "decision_id",
    "best_play", "deck", "discard", "highlighted", "pack_cards",
    "shop", "all",
]



def _extract_field(data: dict[str, Any], policy_state: dict[str, Any] | None, field: str) -> Any:
    """Extract a single field from observation data (and optional policy state) for query/batch use."""
    round_data = data.get("round") or {}
    areas = data.get("areas") or {}
    game = data.get("game") or {}
    blind = data.get("blind") or {}

    if field == "state":
        return game.get("state_name", "?")
    if field == "chips":
        return round_data.get("chips", 0)
    if field == "dollars":
        return round_data.get("dollars", 0)
    if field == "hands_left":
        return round_data.get("hands_left", 0)
    if field == "discards_left":
        return round_data.get("discards_left", 0)
    if field == "hand":
        return compact_hand(areas.get("hand") or [])
    if field == "jokers":
        return [j.get("name") or j.get("center_key", "?") for j in areas.get("jokers") or []]
    if field == "blind":
        return blind.get("name", "?")
    if field == "decision_id":
        return policy_state.get("decision_id", "?") if policy_state else "?"
    if field == "best_play":
        bp = ((policy_state or {}).get("hand_analysis") or {}).get("best_play")
        return {"hand_name": bp.get("hand_name"), "estimated_score": bp.get("estimated_score")} if bp else None
    if field == "all":
        bp = ((policy_state or {}).get("hand_analysis") or {}).get("best_play")
        return {
            "state": game.get("state_name"),
            "seed": round_data.get("seed"),
            "ante": round_data.get("ante"),
            "round": round_data.get("round"),
            "blind": blind.get("name"),
            "chips": round_data.get("chips", 0),
            "hands_left": round_data.get("hands_left", 0),
            "discards_left": round_data.get("discards_left", 0),
            "dollars": round_data.get("dollars", 0),
            "best_play": {"hand_name": bp.get("hand_name"), "estimated_score": bp.get("estimated_score")} if bp else None,
        }
    raise ValueError(f"unknown field: {field}")
def compact_hand(hand: list[dict]) -> list[str]:
    result = []
    for card in hand:
        base = card.get("base")
        if isinstance(base, dict):
            name = base.get("name", "?")
            suit = base.get("suit", "?")
            result.append(f"{name} {suit}")
        else:
            result.append("[?]")
    return result


def cmd_query(args: argparse.Namespace) -> int:
    """Field-level queries from observation without saving raw dumps."""
    if not OBSERVATION_PATH.exists():
        print(f"{OBSERVATION_PATH} does not exist yet.", file=sys.stderr)
        return 2

    data = read_json(OBSERVATION_PATH)
    field = args.field
    as_json = getattr(args, "json", False)
    _quiet = getattr(args, "quiet", False)

    def qprint(*a, **kw):
        if not _quiet:
            print(*a, **kw)

    if field is None:
        qprint("Available fields: " + ", ".join(AVAILABLE_FIELDS))
        return 0

    if field in ("state", "chips", "dollars", "hands_left", "discards_left"):
        val = _extract_field(data, None, field)
        qprint(val)
        return 0

    if field == "hand":
        areas = data.get("areas") or {}
        hand = areas.get("hand") or []
        if as_json:
            qprint(json.dumps(hand))
            return 0
        for card_name in compact_hand(hand):
            qprint(card_name)
        return 0

    if field == "jokers":
        areas = data.get("areas") or {}
        jokers = areas.get("jokers") or []
        for j in jokers:
            name = j.get("name") or j.get("center_key", "?")
            edition = j.get("edition") or ""
            line = name
            if edition:
                line += f" ({edition})"
            qprint(line)
        return 0

    if field == "blind":
        blind = data.get("blind") or {}
        chips = blind.get("chips", "?")
        name = blind.get("name", "?")
        effect = blind.get("effect", "")
        qprint(f"{name} - {chips} chips" + (f" [{effect}]" if effect else ""))
        return 0

    if field == "all":
        if as_json:
            policy = enhance_policy_state(
                build_policy_state(data, play_limit=0, discard_limit=0, target_limit=0), data
            )
            bp = (policy.get("hand_analysis") or {}).get("best_play")
            qprint(json.dumps({
                "state": data.get("game", {}).get("state_name"),
                "seed": data.get("round", {}).get("seed"),
                "ante": data.get("round", {}).get("ante"),
                "round": data.get("round", {}).get("round"),
                "blind": data.get("blind", {}).get("name"),
                "chips": data.get("round", {}).get("chips", 0),
                "hands_left": data.get("round", {}).get("hands_left", 0),
                "discards_left": data.get("round", {}).get("discards_left", 0),
                "dollars": data.get("round", {}).get("dollars", 0),
                "best_play": {"hand_name": bp.get("hand_name"), "estimated_score": bp.get("estimated_score")} if bp else None,
            }, indent=2))
            return 0
        game = data.get("game") or {}
        round_data = data.get("round") or {}
        blind = data.get("blind") or {}
        areas = data.get("areas") or {}
        qprint(f"State: {game.get('state_name', '?')}")
        qprint(f"Seed: {round_data.get('seed', '?')}")
        qprint(f"Ante {round_data.get('ante', '?')}, Round {round_data.get('round', '?')}")
        qprint(f"Blind: {blind.get('name', '?')} ({blind.get('chips', '?')} chips)")
        qprint(f"Chips: {round_data.get('chips', 0)} | Hands: {round_data.get('hands_left', 0)} | Discards: {round_data.get('discards_left', 0)} | $: {round_data.get('dollars', 0)}")
        hand = areas.get("hand") or []
        if hand:
            qprint(f"Hand: {' '.join(compact_hand(hand))}")
        jokers = areas.get("jokers") or []
        if jokers:
            qprint(f"Jokers: {', '.join(j.get('name') or j.get('center_key', '?') for j in jokers)}")
        game_state = game.get("state_name")
        is_decision = game_state in {"BLIND_SELECT", "SELECTING_HAND", "SHOP", "TAROT_PACK", "PLANET_PACK", "SPECTRAL_PACK", "STANDARD_PACK", "BUFFOON_PACK"}
        qprint(f"Decision point: {is_decision}")
        return 0

    policy = enhance_policy_state(build_policy_state(data), data)

    if field == "legal_actions":
        actions = policy.get("legal_actions") or []
        if as_json:
            qprint(json.dumps({"count": len(actions), "actions": actions}, indent=2))
            return 0
        qprint(_group_actions(actions))
        return 0

    if field == "decision_id":
        qprint(policy.get("decision_id", "?"))
        return 0

    if field == "best_play":
        best = (policy.get("hand_analysis") or {}).get("best_play")
        if not best:
            qprint("No best play available")
            return 1
        if as_json:
            qprint(json.dumps(best, indent=2))
            return 0
        qprint(f"Hand: {best.get('hand_name', '?')}")
        qprint(f"Score: {best.get('estimated_score', '?')}")
        cards = best.get("card_names", [])
        qprint(f"Cards: {', '.join(cards)}")
        return 0

    if field == "deck":
        areas = data.get("areas") or {}
        deck = areas.get("deck") or []
        deck_count = areas.get("deck_count", len(deck))
        discard = areas.get("discard") or []
        discard_count = areas.get("discard_count", len(discard))
        deck_summary = areas.get("deck_summary") or {}
        discard_summary = areas.get("discard_summary") or {}
        if as_json:
            qprint(json.dumps({
                "deck_count": deck_count,
                "discard_count": discard_count,
                "deck_summary": deck_summary,
                "discard_summary": discard_summary,
                "discard_cards": [
                    {"name": c.get("base", {}).get("name", "?"), "suit": c.get("base", {}).get("suit", "?"), "debuffed": c.get("debuffed")}
                    for c in discard
                ],
            }, indent=2))
            return 0
        qprint(f"Deck: {deck_count} cards remaining")
        qprint(f"Discard: {discard_count} cards")
        if deck_summary.get("count", 0) > 0:
            by_rank = deck_summary.get("by_rank", [])
            by_suit = deck_summary.get("by_suit", [])
            if by_rank:
                qprint("By rank:", ", ".join(f"{r}:{c}" for r, c in by_rank))
            if by_suit:
                qprint("By suit:", ", ".join(f"{s}:{c}" for s, c in by_suit))
        if discard:
            for c in discard[:10]:
                base = c.get("base")
                if isinstance(base, dict):
                    qprint(f"  Discard: {base.get('name', '?')} {base.get('suit', '?')}")
        return 0

    if field == "discard":
        areas = data.get("areas") or {}
        discard = areas.get("discard") or []
        if as_json:
            qprint(json.dumps(discard))
            return 0
        qprint(f"Discard pile: {len(discard)} cards")
        for c in discard:
            base = c.get("base")
            if isinstance(base, dict):
                name = base.get("name", "?")
                suit = base.get("suit", "?")
                enh = base.get("enhancement", "")
                seal = base.get("seal", "")
                line = f"{name} {suit}"
                if enh:
                    line += f" [{enh}]"
                if seal:
                    line += f" ({seal})"
                debuffed = " [debuffed]" if c.get("debuffed") else ""
                qprint(f"  {line}{debuffed}")
        return 0

    if field == "highlighted":
        areas = data.get("areas") or {}
        hi = areas.get("hand_highlighted") or []
        hand = areas.get("hand") or []
        if not hi:
            hi = [c for c in hand if c.get("highlighted")]
        if as_json:
            qprint(json.dumps(hi))
            return 0
        if not hi:
            qprint("No cards highlighted")
            return 0
        for c in hi:
            base = c.get("base")
            if isinstance(base, dict):
                qprint(base.get("name", "?"))
        return 0

    if field == "pack_cards":
        areas = data.get("areas") or {}
        cards = areas.get("pack_cards") or []
        if as_json:
            qprint(json.dumps(cards))
            return 0
        qprint(f"Pack cards: {len(cards)}")
        for c in cards:
            base = c.get("base")
            if isinstance(base, dict):
                name = base.get("name", "?")
                suit = base.get("suit", "?")
                enh = base.get("enhancement", "")
                line = f"{name} {suit}"
                if enh:
                    line += f" [{enh}]"
                qprint(f"  {line}")
        return 0

    if field == "shop":
        areas = data.get("areas") or {}
        jokers = areas.get("shop_jokers") or []
        vouchers = areas.get("shop_vouchers") or []
        booster = areas.get("shop_booster") or {}
        if as_json:
            qprint(json.dumps({
                "shop_jokers": jokers,
                "shop_vouchers": vouchers,
                "shop_booster": booster,
            }, indent=2))
            return 0
        qprint(f"Shop jokers: {len(jokers)}")
        for j in jokers:
            name = j.get("name") or j.get("center_key", "?")
            cost = j.get("cost", "?")
            edition = j.get("edition", "")
            line = f"{name} (${cost})"
            if edition:
                line += f" ({edition})"
            qprint(f"  {line}")
        qprint(f"Shop vouchers: {len(vouchers)}")
        for v in vouchers:
            name = v.get("name") or v.get("center_key", "?")
            cost = v.get("cost", "?")
            qprint(f"  {name} (${cost})")
        if booster:
            bname = booster.get("name") or booster.get("center_key", "?")
            qprint(f"Booster: {bname}")
        return 0

    print(f"Unknown query field: {field}", file=sys.stderr)
    print(f"Available: {', '.join(AVAILABLE_FIELDS)}", file=sys.stderr)
    return 1

def _format_policy_state_text(ps: dict[str, Any]) -> str:
    """Compact text representation of policy state — no raw JSON dump."""
    game = ps.get("game") or {}
    run = ps.get("run") or {}
    areas = ps.get("areas") or {}
    blind = run.get("blind") or {}
    hand_analysis = ps.get("hand_analysis") or {}

    lines = []
    lines.append(f"State: {game.get('state', '?')}")
    lines.append(f"Seed: {run.get('seed', '?')} | Ante {run.get('ante', '?')}, Round {run.get('round', '?')}")
    lines.append(f"Blind: {blind.get('name', '?')} ({blind.get('chips_required', '?')} chips)")
    lines.append(f"Chips: {run.get('chips', 0)} | Hands: {run.get('hands_left', 0)} | Discards: {run.get('discards_left', 0)} | $: {run.get('dollars', 0)}")

    hand = areas.get("hand") or []
    if hand:
        card_names = [f"{c.get('name', '?')} {c.get('suit', '?')}" for c in hand]
        lines.append(f"Hand: {' '.join(card_names)}")

    jokers = areas.get("jokers") or []
    if jokers:
        joker_names = [j.get("name", "?") for j in jokers]
        lines.append(f"Jokers: {', '.join(joker_names)}")

    consumeables = areas.get("consumeables") or []
    if consumeables:
        cons = [c.get("name", "?") for c in consumeables]
        lines.append(f"Consumables: {', '.join(cons)}")

    is_decision = game.get("state") in {"BLIND_SELECT", "SELECTING_HAND", "SHOP", "TAROT_PACK", "PLANET_PACK", "SPECTRAL_PACK", "STANDARD_PACK", "BUFFOON_PACK"}
    lines.append(f"Decision point: {is_decision}")

    if is_decision:
        actions = ps.get("legal_actions") or []
        for group_line in _group_actions(actions).splitlines():
            lines.append(group_line)

    best = hand_analysis.get("best_play")
    if best:
        lines.append(f"Best play: {best.get('hand_name', '?')} est={best.get('estimated_score', '?')} cards={best.get('card_names', [])}")

    return "\n".join(lines)


def cmd_policy_state(args: argparse.Namespace) -> int:
    if not OBSERVATION_PATH.exists():
        print(f"{OBSERVATION_PATH} does not exist yet. Launch Balatro with the mod installed.", file=sys.stderr)
        return 2
    _quiet = getattr(args, "quiet", False)
    data = read_json(OBSERVATION_PATH)
    policy_state = enhance_policy_state(
        build_policy_state(
            data,
            play_limit=max(0, args.play_limit),
            discard_limit=max(0, args.discard_limit),
            target_limit=max(0, args.target_limit),
        ),
        data,
    )
    if getattr(args, "batch_fields", None):
        fields = [f.strip() for f in args.batch_fields.split(",") if f.strip()]
        if not fields:
            print("batch requires --fields comma-separated list", file=sys.stderr)
            return 1
        result = {}
        for f in fields:
            try:
                result[f] = _extract_field(data, policy_state, f)
            except ValueError:
                result[f"error"] = f"unknown batch field: {f}"
        if not _quiet:
            print(json.dumps(result, indent=2, sort_keys=True))
        return 0

    if args.field:
        val = _resolve_field(policy_state, args.field)
        if isinstance(val, (dict, list)):
            if not _quiet:
                print(json.dumps(val, indent=2, sort_keys=True))
        else:
            if not _quiet:
                print(val)
        return 0
    if args.json:
        if not _quiet:
            print(json.dumps(policy_state, indent=2, sort_keys=True))
        return 0
    else:
        if not _quiet:
            print(_format_policy_state_text(policy_state))
        return 0



def cmd_hand_values(args: argparse.Namespace) -> int:
    if not OBSERVATION_PATH.exists():
        print(json.dumps({"ok": False, "message": "observation missing"}), file=sys.stderr)
        return 2
    observation = read_json(OBSERVATION_PATH)
    contract = observation.get("poker_hands") or {}
    _quiet = getattr(args, "quiet", False)

    def qprint(*a, **kw):
        if not _quiet:
            print(*a, **kw)

    if contract.get("schema") != "balatro-poker-hand-values/v1":
        print(
            json.dumps(
                {
                    "ok": False,
                    "message": "canonical poker_hands contract missing or unsupported",
                    "schema": contract.get("schema"),
                }
            ),
            file=sys.stderr,
        )
        return 2
    selected = None
    if args.hand:
        key = reliable_scoring.resolve_hand_key(args.hand)
        selected = (contract.get("values") or {}).get(key) if key else None
        if not selected:
            print(json.dumps({"ok": False, "message": f"unknown poker hand: {args.hand}"}), file=sys.stderr)
            return 2
    if args.json:
        payload = (
            {
                "schema": contract.get("schema"),
                "source": contract.get("source"),
                "source_seed": contract.get("source_seed"),
                "valid_for_scoring": contract.get("valid_for_scoring"),
                "value": selected,
            }
            if selected
            else contract
        )
        qprint(json.dumps(payload, indent=2, sort_keys=True))
        return 0 if contract.get("valid_for_scoring") else 1
    else:
        qprint(
            f"source={contract.get('source')} valid_for_scoring={bool(contract.get('valid_for_scoring'))} "
            f"seed={contract.get('source_seed')}"""
        )
        rows = [selected] if selected else reliable_scoring.ordered_hand_values(contract)
        for value in rows:
            qprint(
                f"{value.get('key')}: level {value.get('level')}, "
                f"{value.get('chips')} chips x {value.get('mult')} mult, played {value.get('played')}"""
            )
    return 0 if contract.get("valid_for_scoring") else 1



def policy_state_fingerprint(policy_state: dict[str, Any]) -> str:
    areas = policy_state.get("areas") or {}

    def area_refs(area_name: str) -> list[tuple[Any, Any, Any]]:
        return [
            (card.get("index"), card.get("instance_id"), card.get("center_key") or card.get("card_key"))
            for card in areas.get(area_name) or []
        ]

    payload = {
        "game": policy_state.get("game"),
        "ready": {
            key: (policy_state.get("ready") or {}).get(key)
            for key in (
                "tutorial_complete",
                "overlay_tutorial_present",
                "current_setup",
                "saved_game_present",
                "saved_game_seed",
            )
        },
        "run": policy_state.get("run"),
        "legal_ids": [action.get("id") for action in policy_state.get("legal_actions") or []],
        "areas": {
            "hand": area_refs("hand"),
            "jokers": area_refs("jokers"),
            "consumeables": area_refs("consumeables"),
            "shop_jokers": area_refs("shop_jokers"),
            "shop_vouchers": area_refs("shop_vouchers"),
            "shop_booster": area_refs("shop_booster"),
            "pack_cards": area_refs("pack_cards"),
            "discard": area_refs("discard"),
            "deck_count": areas.get("deck_count"),
            "discard_count": areas.get("discard_count"),
            "deck_remaining": areas.get("deck_remaining"),
            "discard_pile": areas.get("discard_pile"),
            "slots": areas.get("slots"),
        },
    }
    return json.dumps(payload, sort_keys=True, separators=(",", ":"))


TRANSIENT_POLICY_STATES = {"-1", "HAND_PLAYED", "DRAW_TO_HAND", "NEW_ROUND"}
BOOSTER_PACK_STATES = {"TAROT_PACK", "PLANET_PACK", "SPECTRAL_PACK", "STANDARD_PACK", "BUFFOON_PACK"}


def policy_game_state(policy_state: dict[str, Any]) -> str | None:
    state = (policy_state.get("game") or {}).get("state")
    return None if state is None else str(state)


def policy_area_refs(policy_state: dict[str, Any], area_name: str) -> list[tuple[Any, Any, Any]]:
    areas = policy_state.get("areas") or {}
    return [
        (card.get("index"), card.get("instance_id"), card.get("center_key") or card.get("card_key"))
        for card in areas.get(area_name) or []
    ]


def run_field_changed(before: dict[str, Any], after: dict[str, Any], *keys: str) -> bool:
    before_run = before.get("run") or {}
    after_run = after.get("run") or {}
    return any(before_run.get(key) != after_run.get(key) for key in keys)


def policy_shop_inventory_ready(policy_state: dict[str, Any]) -> bool:
    areas = policy_state.get("areas") or {}
    return bool(areas.get("shop_jokers") or areas.get("shop_vouchers") or areas.get("shop_booster"))



def can_reroll_boss_from_run(round_data: dict[str, Any]) -> bool:
    dollars = int(round_data.get("dollars") or 0)
    bankrupt_at = int(round_data.get("bankrupt_at") or 0)
    used_vouchers = {
        str(item.get("key"))
        for item in round_data.get("used_vouchers") or []
        if item.get("key")
    }
    has_retcon = "v_retcon" in used_vouchers
    has_directors_cut = "v_directors_cut" in used_vouchers
    return dollars - bankrupt_at >= 10 and (has_retcon or (has_directors_cut and not round_data.get("boss_rerolled")))


def policy_pack_choices(policy_state: dict[str, Any]) -> int:
    return int_or_none((policy_state.get("run") or {}).get("pack_choices")) or 0


def is_next_policy_state_ready(
    *,
    before_policy_state: dict[str, Any],
    current_policy_state: dict[str, Any],
    selected_action: dict[str, Any],
    before_fingerprint: str,
) -> bool:
    legal_actions = current_policy_state.get("legal_actions") or []
    if not legal_actions:
        return False

    state = policy_game_state(current_policy_state)
    before_state = policy_game_state(before_policy_state)
    if state in TRANSIENT_POLICY_STATES:
        return False

    action = str(selected_action.get("action") or "")
    changed = policy_state_fingerprint(current_policy_state) != before_fingerprint

    if action == "start_setup_run":
        return state not in {"SPLASH", "MENU"}
    if action == "dismiss_unlock_overlay":
        return changed and state == before_state
    if action == "select_blind":
        return state != before_state and state not in {"BLIND_SELECT"}
    if action == "reroll_boss":
        ready = current_policy_state.get("ready") or {}
        return state == "BLIND_SELECT" and not ready.get("controller_locked") and changed
    if action == "cash_out":
        return state == "SHOP" and policy_shop_inventory_ready(current_policy_state)
    if action == "buy" and selected_action.get("area") == "shop_booster":
        areas = current_policy_state.get("areas") or {}
        return state in BOOSTER_PACK_STATES and bool(areas.get("pack_cards")) and policy_pack_choices(current_policy_state) > 0
    if action == "use" and before_state in BOOSTER_PACK_STATES:
        if state not in BOOSTER_PACK_STATES:
            return True
        before_choices = policy_pack_choices(before_policy_state)
        current_choices = policy_pack_choices(current_policy_state)
        if before_choices <= 1:
            return False
        areas = current_policy_state.get("areas") or {}
        return (
            0 < current_choices < before_choices
            and bool(areas.get("pack_cards"))
            and policy_area_refs(before_policy_state, "pack_cards") != policy_area_refs(current_policy_state, "pack_cards")
        )
    if action == "next_round":
        return state == "BLIND_SELECT" and any(
            item.get("action") in {"select_blind", "skip_blind"} for item in legal_actions
        )
    if action == "play":
        if state != before_state:
            return True
        return state == "SELECTING_HAND" and run_field_changed(
            before_policy_state,
            current_policy_state,
            "chips",
            "hands_left",
            "hands_played",
        )
    if action == "discard":
        if state != before_state:
            return True
        return (
            state == "SELECTING_HAND"
            and run_field_changed(before_policy_state, current_policy_state, "discards_left", "discards_used")
            and policy_area_refs(before_policy_state, "hand") != policy_area_refs(current_policy_state, "hand")
        )
    if action == "move_card":
        area_name = str(selected_action.get("area") or "")
        return (
            state == before_state
            and bool(area_name)
            and policy_area_refs(before_policy_state, area_name) != policy_area_refs(current_policy_state, area_name)
        )
    if action == "move_joker":
        return (
            state == before_state
            and policy_area_refs(before_policy_state, "jokers") != policy_area_refs(current_policy_state, "jokers")
        )
    if action == "skip_booster":
        return state not in BOOSTER_PACK_STATES

    return changed


def load_policy_state(play_limit: int, discard_limit: int, target_limit: int = 60) -> dict[str, Any]:
    if not OBSERVATION_PATH.exists():
        raise FileNotFoundError(f"{OBSERVATION_PATH} does not exist yet. Launch Balatro with the mod installed.")
    data = read_json(OBSERVATION_PATH)
    return enhance_policy_state(
        build_policy_state(
            data,
            play_limit=max(0, play_limit),
            discard_limit=max(0, discard_limit),
            target_limit=max(0, target_limit),
        ),
        data,
    )


def resolve_policy_action(
    action_id: str,
    play_limit: int,
    discard_limit: int,
    target_limit: int = 60,
) -> tuple[dict[str, Any], dict[str, Any] | None]:
    policy_state = load_policy_state(play_limit, discard_limit, target_limit)
    actions = policy_state.get("legal_actions") or []
    selected = next((action for action in actions if action.get("id") == action_id), None)
    return policy_state, selected


def illegal_policy_action_result(policy_state: dict[str, Any], action_id: str) -> dict[str, Any]:
    return {
        "ok": False,
        "message": f"policy action id not legal in current state: {action_id}",
        "state": (policy_state.get("game") or {}).get("state"),
        "available_ids": [action.get("id") for action in policy_state.get("legal_actions") or []],
    }


def wait_for_next_policy_state(
    *,
    command_id: int | str,
    before_policy_state: dict[str, Any],
    selected_action: dict[str, Any],
    play_limit: int,
    discard_limit: int,
    target_limit: int,
    timeout: float,
    interval: float = 0.1,
) -> dict[str, Any] | None:
    before_fingerprint = policy_state_fingerprint(before_policy_state)
    deadline = time.time() + timeout
    while time.time() < deadline:
        if OBSERVATION_PATH.exists():
            try:
                policy_state = load_policy_state(play_limit, discard_limit, target_limit)
            except Exception:
                policy_state = None
            if policy_state:
                bridge = policy_state.get("bridge") or {}
                if (
                    str(bridge.get("last_command_id")) == str(command_id)
                    and is_next_policy_state_ready(
                        before_policy_state=before_policy_state,
                        current_policy_state=policy_state,
                        selected_action=selected_action,
                        before_fingerprint=before_fingerprint,
                    )
                ):
                    return policy_state
        time.sleep(interval)
    return None


def execute_policy_action(
    *,
    action_id: str,
    expected_decision_id: str,
    play_limit: int,
    discard_limit: int,
    target_limit: int = 60,
    wait_response: bool,
    response_timeout: float,
    wait_next_state: bool = False,
    settle_timeout: float = 12.0,
) -> tuple[int, dict[str, Any]]:
    try:
        policy_state, selected = resolve_policy_action(action_id, play_limit, discard_limit, target_limit)
    except FileNotFoundError as exc:
        return 2, {"ok": False, "message": str(exc)}

    current_decision_id = str(policy_state.get("decision_id") or "")
    try:
        validate_decision_id(policy_state, expected_decision_id)
    except ValueError:
        return 1, {
            "ok": False,
            "message": "stale or missing decision_id",
            "expected": current_decision_id,
            "provided": expected_decision_id,
        }

    if selected is None:
        return 1, illegal_policy_action_result(policy_state, action_id)

    command = selected.get("command")
    if not isinstance(command, dict):
        return 1, {"ok": False, "message": f"policy action {action_id} has no executable command"}

    command = dict(command)
    command["id"] = next_command_id()
    observation = read_json(OBSERVATION_PATH)
    try:
        guard_command(command, observation)
    except SafetyError as exc:
        return 2, {"ok": False, "message": str(exc), "decision_id": current_decision_id}
    write_command(command, echo=False)
    result: dict[str, Any] = {
        "ok": True,
        "decision_id": current_decision_id,
        "before_observation_id": (policy_state.get("bridge") or {}).get("observation_id"),
        "state": (policy_state.get("game") or {}).get("state"),
        "selected_action": selected,
        "command": command,
    }

    if wait_response or wait_next_state:
        response = wait_for_response(command.get("id"), response_timeout)
        if response is None:
            result["response_timeout"] = True
            return 1, result
        result["response"] = response

    if wait_next_state:
        next_policy_state = wait_for_next_policy_state(
            command_id=command["id"],
            before_policy_state=policy_state,
            selected_action=selected,
            play_limit=play_limit,
            discard_limit=discard_limit,
            target_limit=target_limit,
            timeout=settle_timeout,
        )
        if next_policy_state is None:
            result["next_state_timeout"] = True
            return 1, result
        result["next_policy_state"] = next_policy_state
        latest_observation = read_json(OBSERVATION_PATH)
        persisted = checkpoint(latest_observation, kind="action")
        result["event_id"] = persisted.get("event_id")
        result["after_observation_id"] = (
            (persisted.get("current_run") or {}).get("identity") or {}
        ).get("observation_id")
        result["acknowledgement"] = {
            "ok": bool((result.get("response") or {}).get("ok")),
            "response_seq": (result.get("response") or {}).get("response_seq"),
        }

    return 0, result


def cmd_policy_step(args: argparse.Namespace) -> int:
    quiet = getattr(args, "quiet", False)
    code, result = execute_policy_action(
        action_id=args.action_id,
        expected_decision_id=args.decision_id,
        play_limit=args.play_limit,
        discard_limit=args.discard_limit,
        target_limit=args.target_limit,
        wait_response=True,
        response_timeout=args.ack_timeout,
        wait_next_state=True,
        settle_timeout=args.settle_timeout,
    )
    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True), file=sys.stderr if code else sys.stdout)
    elif not quiet:
        print(f"ok={result.get('ok', False)} state={result.get('state', '')} decision_id={result.get('decision_id', '')}")
    return code



def cmd_add_lesson(args: argparse.Namespace) -> int:
    """Add a lesson to policies.db."""
    try:
        import sqlite3 as _sq
        from balatro_agent.config import POLICIES_DB
        conn = _sq.connect(str(POLICIES_DB))
        c = conn.cursor()
        lesson_text = " ".join(args.lesson)
        c.execute(
            "INSERT INTO lessons (category, lesson, source, confidence) VALUES (?,?,?,?)",
            (args.category, lesson_text, args.source, args.confidence),
        )
        conn.commit()
        print(f"Lesson added: [{args.source}] [{args.category}]")
        conn.close()
    except (KeyError, ValueError, _sq.IntegrityError) as exc:
        print(json.dumps({"ok": False, "message": str(exc)}), file=sys.stderr)
        return 1
    return 0


def cmd_status(args: argparse.Namespace) -> int:
    _quiet = getattr(args, "quiet", False)
    payload = status_payload(read_observation())
    if args.field:
        val = _resolve_field(payload, args.field)
        if not _quiet:
            print(val)
        return 0
    if args.json:
        if not _quiet:
            print(json.dumps(payload, indent=2, sort_keys=True))
        return 0 if payload.get("safe_for_mutation") else 1
    else:
        if not _quiet:
            print(f"safe_for_mutation={payload['safe_for_mutation']}")
            print(f"processes={len(payload['processes'])} seed={payload.get('seed')} age={payload.get('observation_age_seconds')}")
            for problem in payload.get("problems") or []:
                print(f"problem: {problem}")
    return 0 if payload.get("safe_for_mutation") else 1


def cmd_checkpoint(args: argparse.Namespace) -> int:
    observation = read_observation()
    if not observation:
        print(json.dumps({"ok": False, "message": "observation missing"}), file=sys.stderr)
        return 2
    result = checkpoint(observation, kind=args.kind)
    print(json.dumps({"ok": True, **result}, indent=2, sort_keys=True))
    return 0


def cmd_strategy_record(args: argparse.Namespace) -> int:
    try:
        result = add_strategy_evidence(args.rule, args.outcome, args.event_id, args.note)
    except (KeyError, ValueError) as exc:
        print(json.dumps({"ok": False, "message": str(exc)}), file=sys.stderr)
        return 1
    print(json.dumps({"ok": True, **result}, indent=2, sort_keys=True))
    return 0


def cmd_strategy_add(args: argparse.Namespace) -> int:
    try:
        conditions = json.loads(args.conditions)
        if not isinstance(conditions, dict):
            raise ValueError("conditions must be a JSON object")
        result = add_strategy_rule(
            args.rule,
            args.kind,
            conditions,
            args.directive,
            absolute=args.absolute,
        )
    except (json.JSONDecodeError, ValueError) as exc:
        print(json.dumps({"ok": False, "message": str(exc)}), file=sys.stderr)
        return 1
    print(json.dumps({"ok": True, **result}, indent=2, sort_keys=True))
    return 0


def cmd_advance_safe(args: argparse.Namespace) -> int:
    completed: list[dict[str, Any]] = []
    for _ in range(max(1, args.steps)):
        try:
            policy_state = load_policy_state(args.play_limit, args.discard_limit, args.target_limit)
        except FileNotFoundError as exc:
            print(json.dumps({"ok": False, "message": str(exc)}), file=sys.stderr)
            return 2
        state = policy_game_state(policy_state)
        if is_decision_state(state):
            break
        selected = select_safe_transition(policy_state)
        if not selected:
            break
        code, result = execute_policy_action(
            action_id=str(selected.get("id")),
            expected_decision_id=str(policy_state.get("decision_id") or ""),
            play_limit=args.play_limit,
            discard_limit=args.discard_limit,
            target_limit=args.target_limit,
            wait_response=True,
            response_timeout=args.ack_timeout,
            wait_next_state=True,
            settle_timeout=args.settle_timeout,
        )
        completed.append(
            {
                "action_id": selected.get("id"),
                "event_id": result.get("event_id"),
                "ok": code == 0,
                "after_observation_id": result.get("after_observation_id"),
            }
        )
        if code:
            print(json.dumps({"ok": False, "steps": completed, "error": result}, indent=2), file=sys.stderr)
            return code
    final = load_policy_state(args.play_limit, args.discard_limit, args.target_limit)
    if not getattr(args, "quiet", False):
        print(
            json.dumps(
                {
                    "ok": True,
                    "steps": completed,
                    "stopped_at": policy_game_state(final),
                    "decision_id": final.get("decision_id"),
                },
                indent=2,
                sort_keys=True,
            )
        )
    return 0



def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="cmd", required=True)



    query = subparsers.add_parser("query", help="Query specific fields from observation")
    query.add_argument("field", nargs="?", choices=AVAILABLE_FIELDS, default=None, help="Field to query")
    query.add_argument("--json", action="store_true", help="Output as JSON (for structured access)")
    query.add_argument("--quiet", action="store_true", help="Suppress all output and return 0 on success")
    query.set_defaults(func=cmd_query)
    add_lesson = subparsers.add_parser("add-lesson", help="Add a lesson to the policies database")
    add_lesson.add_argument("--category", required=True, help="Lesson category (e.g. mechanics, controller)")
    add_lesson.add_argument("--source", default="user", help="Lesson source (user, system, verified)")
    add_lesson.add_argument("--confidence", type=float, default=0.5, help="Initial confidence score")
    add_lesson.add_argument("lesson", nargs="+", help="Lesson text")
    add_lesson.set_defaults(func=cmd_add_lesson)

    status = subparsers.add_parser("status", help="Report process, bridge, observation, seed, and resumable state safety")

    status.add_argument("--json", action="store_true", help="Print machine-readable status")

    status.add_argument("--field", default=None, help="Return a single field value")
    status.add_argument("--quiet", action="store_true", help="Suppress all output and return 0 on success")
    status.set_defaults(func=cmd_status)

    checkpoint_parser = subparsers.add_parser("checkpoint", help="Reconcile live observation into canonical state and generated Markdown")
    checkpoint_parser.add_argument("--kind", default="checkpoint", help="Event kind for this reconciliation")
    checkpoint_parser.set_defaults(func=cmd_checkpoint)

    strategy_record = subparsers.add_parser("strategy-record", help="Attach an existing event as strategy evidence")
    strategy_record.add_argument("--rule", required=True, help="Strategy rule id")
    strategy_record.add_argument("--outcome", required=True, choices=["support", "contradict", "inconclusive"])
    strategy_record.add_argument("--event-id", required=True, help="Existing events.jsonl event id")
    strategy_record.add_argument("--note", required=True)
    strategy_record.set_defaults(func=cmd_strategy_record)

    strategy_add = subparsers.add_parser("strategy-add", help="Add a conditioned candidate strategy rule")
    strategy_add.add_argument("--rule", required=True, help="Stable strategy rule id")
    strategy_add.add_argument("--kind", required=True, choices=["strategy", "mechanic", "hand", "blind", "shop", "economy"])
    strategy_add.add_argument("--conditions", required=True, help="JSON object describing exact applicability")
    strategy_add.add_argument("--directive", required=True)
    strategy_add.add_argument("--absolute", action="store_true")
    strategy_add.set_defaults(func=cmd_strategy_add)


    raw_command = subparsers.add_parser("command", help="Write a raw JSON command object")
    raw_command.add_argument("json_command")
    raw_command.add_argument("--wait", action="store_true", help="Wait for the matching response id")
    raw_command.add_argument("--timeout", type=float, default=5.0)
    raw_command.add_argument("--quiet", action="store_true", help="Suppress verbose output")
    raw_command.set_defaults(func=cmd_command)

    action = subparsers.add_parser("action", help="Write a typed command")
    action.add_argument(
        "action",
        choices=[
            "observe",
            "skip_tutorial",
            "setup_new_run",
            "start_run",
            "select_blind",
            "skip_blind",
            "play",
            "discard",
            "buy",
            "buy_and_use",
            "use",
            "sell",
            "reroll_shop",
            "next_round",
            "cash_out",
            "skip_booster",
            "reroll_boss",
            "ui_click",
            "ensure_menu_ui",
            "speed",
            "sort_hand",
            "move_card",
        ],
    )
    action.add_argument("values", nargs="*")
    action.add_argument("--decision-id", help="Current decision_id from policy-state; required for mutations")
    action.add_argument("--wait", action="store_true", help="Wait for the matching response id")
    action.add_argument("--timeout", type=float, default=5.0)
    action.add_argument("--quiet", action="store_true", help="Suppress verbose output")
    action.set_defaults(func=cmd_action)

    wait = subparsers.add_parser("wait", help="Wait for observation JSON, optionally in one state")
    wait.add_argument("--state", help="State name such as SELECTING_HAND or SHOP")
    wait.add_argument("--ready", help="Ready flag such as blind_select_ready, shop_ready, or round_eval_ready")
    wait.add_argument("--timeout", type=float, default=10.0)
    wait.add_argument("--interval", type=float, default=0.2)
    wait.add_argument("--json", action="store_true", help="Print full observation JSON instead of compact text")
    wait.add_argument("--quiet", action="store_true", help="Return silently on success")
    wait.set_defaults(func=cmd_wait)

    policy_state = subparsers.add_parser("policy-state", help="Print compact LLM-ready state and legal actions")
    policy_state.add_argument("--play-limit", type=int, default=30, help="Maximum ranked play candidates to include")
    policy_state.add_argument("--discard-limit", type=int, default=200, help="Maximum ranked discard candidates to include")
    policy_state.add_argument("--target-limit", type=int, default=60, help="Maximum target combinations per targeted use card")
    policy_state.add_argument("--json", action="store_true", help="Print full structured JSON instead of compact text")
    policy_state.add_argument("--field", default=None, help="Return a single field value")
    policy_state.add_argument("--quiet", action="store_true", help="Suppress all output and return 0 on success")
    policy_state.add_argument("--fields", dest="batch_fields", default=None, help="Comma-separated fields for batch query")
    policy_state.set_defaults(func=cmd_policy_state)

    hand_values = subparsers.add_parser("hand-values", help="Print exact Run Info poker-hand values")
    hand_values.add_argument("hand", nargs="?", help="Optional hand name; Royal Flush maps to Straight Flush")
    hand_values.add_argument("--json", action="store_true", help="Print machine-readable canonical values")
    hand_values.add_argument("--quiet", action="store_true", help="Suppress all output; return 0 on success")
    hand_values.set_defaults(func=cmd_hand_values)

    policy_action = subparsers.add_parser("policy-action", help="Execute a currently legal policy action by id")
    policy_action.add_argument("action_id", help="Action id from policy-state legal_actions[].id")
    policy_action.add_argument("--decision-id", required=True, help="Current decision_id from policy-state")
    policy_action.add_argument("--play-limit", type=int, default=30, help="Maximum ranked play candidates to consider")
    policy_action.add_argument("--discard-limit", type=int, default=200, help="Maximum ranked discard candidates to consider")
    policy_action.add_argument("--target-limit", type=int, default=60, help="Maximum target combinations per targeted use card")
    policy_action.add_argument("--wait", action="store_true", help="Wait for the matching bridge response")
    policy_action.add_argument("--timeout", type=float, default=5.0)
    policy_action.add_argument("--json", action="store_true", help="Print full result JSON (includes response and next_policy_state)")
    policy_action.add_argument("--quiet", action="store_true", help="Suppress non-error output")
    policy_action.set_defaults(func=cmd_policy_action)

    policy_step = subparsers.add_parser("policy-step", help="Execute a legal policy action and return the next policy state")
    policy_step.add_argument("action_id", help="Action id from policy-state legal_actions[].id")
    policy_step.add_argument("--decision-id", required=True, help="Current decision_id from policy-state")
    policy_step.add_argument("--play-limit", type=int, default=30, help="Maximum ranked play candidates to consider")
    policy_step.add_argument("--discard-limit", type=int, default=200, help="Maximum ranked discard candidates to consider")
    policy_step.add_argument("--target-limit", type=int, default=60, help="Maximum target combinations per targeted use card")
    policy_step.add_argument("--ack-timeout", type=float, default=5.0, help="Seconds to wait for the bridge ack")
    policy_step.add_argument("--settle-timeout", type=float, default=12.0, help="Seconds to wait for the next decision state")
    policy_step.add_argument("--json", action="store_true", help="Print full result JSON (includes response and next_policy_state)")
    policy_step.add_argument("--quiet", action="store_true", help="Suppress non-error output")
    policy_step.set_defaults(func=cmd_policy_step)

    advance_safe = subparsers.add_parser(
        "advance-safe",
        help="Advance only non-strategic confirmed transitions and stop at next AI decision",
    )
    advance_safe.add_argument("--steps", type=int, default=10)
    advance_safe.add_argument("--play-limit", type=int, default=30)
    advance_safe.add_argument("--discard-limit", type=int, default=200)
    advance_safe.add_argument("--target-limit", type=int, default=60)
    advance_safe.add_argument("--ack-timeout", type=float, default=5.0)
    advance_safe.add_argument("--settle-timeout", type=float, default=12.0)
    advance_safe.add_argument("--quiet", action="store_true", help="Suppress success output")
    advance_safe.set_defaults(func=cmd_advance_safe)

    return parser


def main() -> int:
    capability = os.environ.get("BALATRO_MCP_CAPABILITY", "")
    capability_file = os.environ.get("BALATRO_MCP_CAPABILITY_FILE", "")
    try:
        with open(capability_file, "r", encoding="utf-8") as handle:
            expected = handle.read().strip()
    except OSError:
        expected = ""
    if not capability or not expected or capability != expected:
        print(
            json.dumps({"ok": False, "message": "controller is private; use the balatro MCP tools"}),
            file=sys.stderr,
        )
        return 2
    parser = build_parser()
    args = parser.parse_args()
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())




# Backward compat alias � cmd_policy_action delegates to cmd_policy_step
cmd_policy_action = cmd_policy_step

