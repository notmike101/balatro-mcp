from __future__ import annotations

from copy import deepcopy
from typing import Any

from . import ALLOWED_SEED, SCHEMA_VERSION
from .config import OBJECTIVE
from .storage import utc_timestamp


def _rule(rule_id: str, directive: str, conditions: dict[str, Any], source: str) -> dict[str, Any]:
    return {
        "id": rule_id,
        "kind": "mechanic",
        "conditions": conditions,
        "directive": directive,
        "absolute": True,
        "status": "source_verified",
        "evidence": [{"kind": "source", "reference": source}],
        "contradictions": [],
        "last_validated": utc_timestamp(),
    }


def default_strategy() -> dict[str, Any]:
    return {
        "schema": f"balatro-agent-strategy/v{SCHEMA_VERSION}",
        "seed": ALLOWED_SEED,
        "objective": OBJECTIVE,
        "updated_at": utc_timestamp(),
        "active_build_plan": [
            "No evidence-backed seed build yet. Evaluate live shop and score pressure at each checkpoint.",
            "Prefer score coverage before speculative economy; preserve interest only when current scoring is safe.",
        ],
        "ante_playbook": {},
        "rules": [
            _rule(
                "psychic-five-card-minimum",
                "Against The Psychic, every played hand must contain at least five cards.",
                {"blind_key": "bl_psychic"},
                "balatro_src/game.lua:280",
            ),
            _rule(
                "psychic-boss-not-skippable",
                "Boss Blind cannot be skipped; prepare for The Psychic or reroll it only when a boss-reroll effect is available.",
                {"blind_type": "Boss"},
                "balatro_src/functions/UI_definitions.lua:1461-1477",
            ),
            _rule(
                "half-joker-condition",
                "Half Joker adds 20 Mult only when three or fewer cards are played; do not treat any score as guaranteed.",
                {"joker_key": "j_half"},
                "balatro_src/game.lua:384; balatro_src/card.lua:3672",
            ),
            _rule(
                "drunkard-effect",
                "Drunkard adds one discard and provides no score multiplier.",
                {"joker_key": "j_drunkard"},
                "balatro_src/game.lua:460",
            ),
            _rule(
                "burglar-contextual",
                "Burglar adds three hands and removes discards; evaluate it against current hand plan instead of rejecting categorically.",
                {"joker_key": "j_burglar"},
                "balatro_src/game.lua:417; balatro_src/card.lua:2522",
            ),
            _rule(
                "hand-levels-require-effects",
                "Playing a poker hand does not inherently level it; use live hand levels and explicit upgrade effects.",
                {},
                "balatro_src/functions/common_events.lua:464-491",
            ),
            _rule(
                "skip-reward-is-tag",
                "Skipping Small or Big Blind grants its displayed tag and skips that blind's shop; do not assume a cash reward.",
                {"action": "skip_blind"},
                "balatro_src/functions/button_callbacks.lua:2740-2777",
            ),
            _rule(
                "live-values-authoritative",
                "Use canonical poker_hand_values for exact Run Info hand level, chips, and mult; never substitute static tables when valid values exist.",
                {},
                "CodexAutomation poker_hands schema balatro-poker-hand-values/v1",
            ),
        ],
        "run_history": [],
    }


def find_rule(strategy: dict[str, Any], rule_id: str) -> dict[str, Any]:
    for rule in strategy.get("rules") or []:
        if rule.get("id") == rule_id:
            return rule
    raise KeyError(f"unknown strategy rule: {rule_id}")


def record_evidence(
    strategy: dict[str, Any],
    rule_id: str,
    outcome: str,
    event_id: str,
    note: str,
) -> dict[str, Any]:
    if outcome not in {"support", "contradict", "inconclusive"}:
        raise ValueError("outcome must be support, contradict, or inconclusive")
    updated = deepcopy(strategy)
    rule = find_rule(updated, rule_id)
    item = {"kind": "event", "event_id": event_id, "outcome": outcome, "note": note, "at": utc_timestamp()}
    if outcome == "contradict":
        rule.setdefault("contradictions", []).append(item)
    else:
        rule.setdefault("evidence", []).append(item)
    if rule.get("status") != "source_verified":
        supports = sum(1 for evidence in rule.get("evidence") or [] if evidence.get("outcome") == "support")
        contradictions = len(rule.get("contradictions") or [])
        if contradictions >= (1 if rule.get("absolute") else 2):
            rule["status"] = "rejected"
        elif supports >= 3:
            rule["status"] = "high_confidence"
        elif supports >= 2:
            rule["status"] = "supported"
        elif supports >= 1:
            rule["status"] = "observed"
        else:
            rule["status"] = "candidate"
    rule["last_validated"] = utc_timestamp()
    updated["updated_at"] = utc_timestamp()
    return updated


def active_directives(strategy: dict[str, Any], observation: dict[str, Any]) -> list[dict[str, str]]:
    round_data = observation.get("round") or {}
    blind = observation.get("blind") or {}
    joker_keys = {str(card.get("center_key")) for card in ((observation.get("areas") or {}).get("jokers") or [])}
    action_state = str((observation.get("game") or {}).get("state_name") or "")
    context = {
        "ante": round_data.get("ante"),
        "state": action_state,
        "blind_key": blind.get("key"),
        "blind_name": blind.get("name"),
        "blind_type": round_data.get("blind_on_deck"),
        "dollars": round_data.get("dollars"),
        "most_played_poker_hand": round_data.get("most_played_poker_hand"),
    }
    active: list[dict[str, str]] = []
    for rule in strategy.get("rules") or []:
        if rule.get("status") == "rejected":
            continue
        conditions = rule.get("conditions") or {}
        matches = True
        for key, expected in conditions.items():
            if key == "joker_key":
                matches = str(expected) in joker_keys
            elif key == "action" and expected == "skip_blind":
                matches = action_state == "BLIND_SELECT"
            elif key == "min_dollars":
                matches = round_data.get("dollars") is not None and int(round_data.get("dollars")) >= int(expected)
            elif key == "max_dollars":
                matches = round_data.get("dollars") is not None and int(round_data.get("dollars")) <= int(expected)
            elif key in context:
                matches = context[key] == expected
            else:
                matches = False
            if not matches:
                break
        if not matches:
            continue
        active.append({"id": str(rule["id"]), "directive": str(rule["directive"]), "status": str(rule["status"])})
    return active
