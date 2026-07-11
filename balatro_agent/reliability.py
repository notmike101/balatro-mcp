from __future__ import annotations

import hashlib
import json
import sqlite3
import sys
from contextlib import contextmanager
from typing import Any

from . import ALLOWED_SEED, POLICY_SCHEMA, SCHEMA_VERSION
from .config import (
    CURRENT_RUN_PATH,
    EVENTS_PATH,
    EXPECTED_BRIDGE_VERSION,
    OBJECTIVE,
    OBSERVATION_PATH,
    POLICIES_DB,
    SESSION_MARKDOWN_PATH,
    STRATEGY_STATE_PATH,
)
from .runtime import balatro_processes, observation_age, observation_seed
from . import rendering as reliable_rendering
from .storage import append_jsonl, atomic_write_json, atomic_write_text, load_json, new_event_id, utc_timestamp

# Migration: policies.db is now the source of truth for strategy rules/lessons.
# These helpers bridge reliability.py to policies.db so that balatroctl.py
# strategy-record and strategy-add commands persist to both locations.

def ensure_policy_db() -> None:
    """Create the small runtime schema needed by a fresh MCP checkout."""
    POLICIES_DB.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(POLICIES_DB))
    try:
        conn.executescript(
            """
            CREATE TABLE IF NOT EXISTS game_state (
                run_id TEXT PRIMARY KEY, seed TEXT NOT NULL, ante INTEGER,
                round_num INTEGER DEFAULT 0, status TEXT, chips_current INTEGER,
                chips_required INTEGER, hands_left INTEGER, discards_left INTEGER,
                dollars INTEGER, jokers TEXT, consumeables TEXT,
                most_played_poker_hand TEXT, blind_key TEXT, blind_on_deck TEXT,
                updated_at TEXT, last_action TEXT
            );
            CREATE TABLE IF NOT EXISTS strategy_rules (
                id INTEGER PRIMARY KEY, rule_id TEXT NOT NULL UNIQUE,
                category TEXT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'candidate', confidence REAL DEFAULT 0.5,
                conditions TEXT, related_rules TEXT, created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS strategy_evidence (
                id INTEGER PRIMARY KEY, rule_id TEXT NOT NULL, event_id TEXT NOT NULL,
                outcome TEXT NOT NULL, note TEXT, created_at TEXT DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS lessons (
                id INTEGER PRIMARY KEY, category TEXT NOT NULL, lesson TEXT NOT NULL,
                source TEXT, confidence REAL DEFAULT 0.5, related_rules TEXT,
                created_at TEXT DEFAULT (datetime('now'))
            );
            """
        )
        conn.commit()
    finally:
        conn.close()

@contextmanager
def _db():
    """Context manager for policies.db connections with safe cleanup."""
    ensure_policy_db()
    conn = sqlite3.connect(str(POLICIES_DB))
    try:
        yield conn
        conn.commit()
    except Exception as exc:
        print(f"[reliability] DB persist error: {exc}", file=sys.stderr)
        conn.rollback()
    finally:
        conn.close()


def _persist_rule_to_db(rule_id, kind, conditions, directive, absolute=False):
    """Persist a strategy rule to policies.db (source of truth)."""
    with _db() as conn:
        c = conn.cursor()
        existing = c.execute("SELECT rule_id FROM strategy_rules WHERE rule_id=?", (rule_id,)).fetchone()
        if not existing:
            c.execute(
                "INSERT INTO strategy_rules (rule_id, category, title, body, status) VALUES (?,?,?,?,?)",
                (rule_id, kind, rule_id, directive, "candidate"),
            )


def _persist_evidence_to_db(rule_id, outcome, event_id, note):
    """Persist strategy evidence to policies.db (source of truth)."""
    with _db() as conn:
        c = conn.cursor()
        c.execute(
            "INSERT INTO strategy_evidence (rule_id, outcome, event_id, note) VALUES (?,?,?,?)",
            (rule_id, outcome, event_id, note),
        )
        if outcome == "contradict":
            c.execute("UPDATE strategy_rules SET status='rejected' WHERE rule_id=?", (rule_id,))
        elif outcome == "support":
            has_contradiction = any(
                e[0] == "contradict" for e in c.execute(
                    "SELECT outcome FROM strategy_evidence WHERE rule_id=? AND outcome='contradict'", (rule_id,)
                ).fetchall()
            )
            if not has_contradiction:
                c.execute("UPDATE strategy_rules SET status='source_verified' WHERE rule_id=?", (rule_id,))
from .strategy import active_directives, default_strategy, record_evidence


class StaleDecisionError(ValueError):
    pass


def validate_decision_id(policy_state: dict[str, Any], provided: str | None) -> str:
    expected = str(policy_state.get("decision_id") or "")
    if provided and provided != expected:
        raise StaleDecisionError(f"stale or missing decision_id; expected {expected}")
    return expected


def observation_id(observation: dict[str, Any]) -> str:
    bridge = observation.get("bridge") or {}
    session_id = str(bridge.get("session_id") or "legacy")
    sequence = bridge.get("observation_seq")
    if sequence is not None:
        return f"{session_id}:{sequence}"
    relevant = {
        "game": observation.get("game"),
        "round": observation.get("round"),
        "blind": observation.get("blind"),
        "last_command_id": bridge.get("last_command_id"),
    }
    digest = hashlib.sha256(json.dumps(relevant, sort_keys=True, default=str).encode()).hexdigest()[:16]
    return f"legacy:{digest}"


def decision_id(policy_state: dict[str, Any]) -> str:
    run = policy_state.get("run") or {}
    areas = policy_state.get("areas") or {}
    card_areas = {}
    for area_name in ("hand", "jokers", "consumeables", "shop_jokers", "shop_vouchers", "shop_booster", "pack_cards"):
        card_areas[area_name] = [
            (card.get("index"), card.get("instance_id") or card.get("id"), card.get("center_key"))
            for card in areas.get(area_name) or []
        ]
    poker_hand_values = policy_state.get("poker_hand_values") or {}
    hand_value_fingerprint = {
        key: (
            value.get("level"),
            value.get("chips"),
            value.get("mult"),
            value.get("played"),
            value.get("played_this_round"),
        )
        for key, value in (poker_hand_values.get("values") or {}).items()
        if isinstance(value, dict)
    }
    payload = {
        "session_id": (policy_state.get("bridge") or {}).get("session_id"),
        "state": policy_state.get("game"),
        "run": {
            key: run.get(key)
            for key in (
                "seed",
                "ante",
                "round",
                "dollars",
                "chips",
                "hands_left",
                "discards_left",
                "blind_on_deck",
                "pack_choices",
                "reroll_cost",
                "free_rerolls",
                "boss_rerolled",
            )
        },
        "blind": run.get("blind"),
        "areas": card_areas,
        "poker_hand_values": {
            "schema": poker_hand_values.get("schema"),
            "source": poker_hand_values.get("source"),
            "valid_for_scoring": poker_hand_values.get("valid_for_scoring"),
            "values": hand_value_fingerprint,
        },
        "legal_ids": [action.get("id") for action in policy_state.get("legal_actions") or []],
    }
    return "dec-" + hashlib.sha256(json.dumps(payload, sort_keys=True, default=str).encode()).hexdigest()[:20]


def load_strategy() -> dict[str, Any]:
    """Load strategy rules from policies.db (source of truth), falling back to default."""
    import sqlite3 as _sq
    try:
        ensure_policy_db()
        conn = _sq.connect(str(POLICIES_DB))
        c = conn.cursor()
        rules = []
        for rid, cat, title, body, status, conf, cond in c.execute(
            'SELECT rule_id, category, title, body, status, confidence, conditions FROM strategy_rules'
        ):
            rules.append({
                'id': rid,
                'kind': cat,
                'conditions': json.loads(cond) if cond else {},
                'directive': body,
                'absolute': False,
                'status': status,
                'last_validated': None,
                'contradictions': [],
                'evidence': [{'kind': 'db', 'reference': rid}],
            })
        conn.close()
    except Exception:
        rules = []

    if not rules:
        strategy = default_strategy()
        return strategy

    return {
        'schema': f'balatro-agent-strategy/v{SCHEMA_VERSION}',
        'seed': ALLOWED_SEED,
        'objective': OBJECTIVE,
        'updated_at': utc_timestamp(),
        'active_build_plan': ['No active plan recorded.'],
        'ante_playbook': {},
        'rules': rules,
        'run_history': [],
    }



def next_decision(observation: dict[str, Any]) -> str:
    state = str((observation.get("game") or {}).get("state_name") or "UNKNOWN")
    return {
        "MENU": "Resume saved run through legal menu action; never start a different seed.",
        "BLIND_SELECT": "AI chooses play versus displayed skip tag.",
        "SELECTING_HAND": "AI chooses legal play or discard using live score pressure and active directives.",
        "ROUND_EVAL": "Safe transition may cash out after confirmed win.",
        "SHOP": "AI evaluates purchases, sales, rerolls, and exit.",
        "GAME_OVER": "Record result. Do not start another run without confirming no resumable save exists.",
    }.get(state, f"Wait for stable supported state; current state {state}.")


def current_run_from_observation(observation: dict[str, Any]) -> dict[str, Any]:
    bridge = observation.get("bridge") or {}
    round_data = observation.get("round") or {}
    blind = observation.get("blind") or {}
    areas = observation.get("areas") or {}
    ready = observation.get("ready") or {}
    state = str((observation.get("game") or {}).get("state_name") or "UNKNOWN")
    saved_not_loaded = bool(
        ready.get("saved_game_present")
        and not ready.get("saved_game_loaded")
        and state in {"MENU", "SPLASH"}
    )
    seed = observation_seed(observation)
    session_id = str(bridge.get("session_id") or "legacy")
    prior = load_current_run_from_db()
    prior_identity = prior.get("identity") or {}
    run_id = prior_identity.get("run_id")
    if not run_id or prior_identity.get("seed") != seed:
        run_id = f"run-{session_id[:12]}-{seed or ALLOWED_SEED}"
    return {
        "schema": f"balatro-agent-current-run/v{SCHEMA_VERSION}",
        "objective": OBJECTIVE,
        "updated_at": utc_timestamp(),
        "identity": {
            "run_id": run_id,
            "seed": seed,
            "bridge_session": session_id,
            "observation_id": observation_id(observation),
        },
        "status": "SAVED_MENU" if saved_not_loaded else state,
        "ante": None if saved_not_loaded else round_data.get("ante"),
        "round": None if saved_not_loaded else round_data.get("round"),
        "blind_on_deck": None if saved_not_loaded else round_data.get("blind_on_deck"),
        "blind": {
            "key": None if saved_not_loaded else blind.get("key"),
            "name": None if saved_not_loaded else blind.get("name"),
            "chips_required": None if saved_not_loaded else blind.get("chips"),
            "boss": None if saved_not_loaded else blind.get("boss"),
        },
        "resources": {
            "chips": None if saved_not_loaded else round_data.get("chips"),
            "hands_left": None if saved_not_loaded else round_data.get("hands_left"),
            "discards_left": None if saved_not_loaded else round_data.get("discards_left"),
            "dollars": None if saved_not_loaded else round_data.get("dollars"),
        },
        "build": {
            "jokers": [] if saved_not_loaded else [card.get("name") or card.get("center_key") for card in areas.get("jokers") or []],
            "consumeables": [] if saved_not_loaded else [card.get("name") or card.get("center_key") for card in areas.get("consumeables") or []],
            "most_played_poker_hand": None if saved_not_loaded else round_data.get("most_played_poker_hand"),
        },
        "last_action": bridge.get("last_response"),
        "next_decision": next_decision(observation),
        "observation_age_seconds": observation_age(),
    }


def event_summary(observation: dict[str, Any], current: dict[str, Any], event_id: str, kind: str) -> dict[str, Any]:
    return {
        "schema": f"balatro-agent-event/v{SCHEMA_VERSION}",
        "event_id": event_id,
        "kind": kind,
        "at": utc_timestamp(),
        "run_id": (current.get("identity") or {}).get("run_id"),
        "observation_id": (current.get("identity") or {}).get("observation_id"),
        "state": current.get("status"),
        "ante": current.get("ante"),
        "blind": current.get("blind"),
        "resources": current.get("resources"),
        "last_response": (observation.get("bridge") or {}).get("last_response"),
    }


def load_current_run_from_db(run_id=None):
    try:
        with _db() as conn:
            c = conn.cursor()
            target_run = run_id
            if not target_run:
                row = c.execute(
                    "SELECT * FROM game_state ORDER BY updated_at DESC LIMIT 1"
                ).fetchone()
            elif target_run:
                row = c.execute(
                    "SELECT * FROM game_state WHERE run_id=?", (target_run,)
                ).fetchone()
            else:
                row = None
            if row and c.description:
                cols = [d[0] for d in c.description]
                data = dict(zip(cols, row))
                return _game_state_to_dict(data)
    except Exception:
        pass
    from balatro_agent.storage import load_json as lj
    from balatro_agent.config import CURRENT_RUN_PATH
    return lj(CURRENT_RUN_PATH, {}) or {}


def save_current_run_to_db(current):
    """Persist current run state to policies.db using the _db() context manager."""
    try:
        run_id = (current.get("identity") or {}).get("run_id", "")
        seed = (current.get("identity") or {}).get("seed", "")
        ante = current.get("ante")
        status = current.get("status", "SELECTING_HAND")
        resources = current.get("resources") or {}
        build = current.get("build") or {}
        blind = current.get("blind") or {}
        updated_at = current.get("updated_at") or utc_timestamp()
        with _db() as conn:
            c = conn.cursor()
            c.execute(
                "INSERT OR REPLACE INTO game_state "
                "(run_id, seed, ante, status, chips_current, hands_left, discards_left, dollars,"
                " jokers, consumeables, most_played_poker_hand, blind_key, blind_on_deck, updated_at, last_action)"
                " VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    run_id, seed, ante, status,
                    resources.get("chips", 0),
                    resources.get("hands_left", 0),
                    resources.get("discards_left", 0),
                    resources.get("dollars", 0),
                    json.dumps(build.get("jokers", [])),
                    json.dumps(build.get("consumeables", [])),
                    build.get("most_played_poker_hand", ""),
                    blind.get("key", ""),
                    current.get("blind_on_deck", ""),
                    updated_at,
                    json.dumps(current.get("last_action", {})),
                ),
            )
            if status == "GAME_OVER":
                existing = c.execute(
                    "SELECT run_id FROM run_history WHERE run_id=?", (run_id,)
                ).fetchone()
                if not existing:
                    c.execute(
                        "INSERT INTO run_history (run_id, seed, max_ante, result, ended_at) VALUES (?,?,?,?,?)",
                        (run_id, seed, ante or 0, "game_over", updated_at),
                    )
    except Exception:
        pass

def _game_state_to_dict(data):
    import json as _json
    jokers = _json.loads(data.get("jokers", "[]")) if data.get("jokers") else []
    consumeables = _json.loads(data.get("consumeables", "[]")) if data.get("consumeables") else []
    last_action = _json.loads(data.get("last_action", "{}")) if data.get("last_action") else {}
    return {
        "run_id": data.get("run_id", ""),
        "seed": data.get("seed", ""),
        "ante": data.get("ante"),
        "status": data.get("status", "SELECTING_HAND"),
        "resources": {
            "chips": data.get("chips_current", 0),
            "hands_left": data.get("hands_left", 0),
            "discards_left": data.get("discards_left", 0),
            "dollars": data.get("dollars", 0),
        },
        "build": {
            "jokers": jokers,
            "consumeables": consumeables,
            "most_played_poker_hand": data.get("most_played_poker_hand", ""),
        },
        "blind_key": data.get("blind_key", ""),
        "blind_on_deck": data.get("blind_on_deck", ""),
        "updated_at": data.get("updated_at", ""),
        "last_action": last_action,
    }

def checkpoint(observation: dict[str, Any], *, kind: str = "checkpoint", log_event: bool = True) -> dict[str, Any]:
    strategy = load_strategy()
    previous = load_current_run_from_db()
    current = current_run_from_observation(observation)
    event_id = new_event_id()
    if log_event:
        append_jsonl(EVENTS_PATH, event_summary(observation, current, event_id, kind))
        derived_kinds: list[str] = []
        previous_state = previous.get("status")
        current_state = current.get("status")
        previous_resources = previous.get("resources") or {}
        current_resources = current.get("resources") or {}
        if previous_state in {"MENU", "SAVED_MENU"} and current_state == "BLIND_SELECT":
            derived_kinds.append("run_start")
        if previous_state == "SELECTING_HAND" and current_state == "SELECTING_HAND":
            if previous_resources.get("hands_left") != current_resources.get("hands_left"):
                derived_kinds.append("hand_result")
            elif previous_resources.get("discards_left") != current_resources.get("discards_left"):
                derived_kinds.append("discard_result")
        if previous_state == "ROUND_EVAL" and current_state == "SHOP":
            derived_kinds.append("blind_result")
        if previous_state == "SHOP" and current_state == "BLIND_SELECT":
            derived_kinds.append("shop_result")
        if previous.get("ante") is not None and current.get("ante") is not None and previous.get("ante") != current.get("ante"):
            derived_kinds.append("ante_result")
        if current_state == "GAME_OVER" and previous_state != "GAME_OVER":
            derived_kinds.append("run_result")
        for derived_kind in derived_kinds:
            derived_event = event_summary(observation, current, new_event_id(), derived_kind)
            derived_event["previous"] = {
                "state": previous_state,
                "ante": previous.get("ante"),
                "blind": previous.get("blind"),
                "resources": previous_resources,
            }
            append_jsonl(EVENTS_PATH, derived_event)
        if "run_result" in derived_kinds:
            run_id = (current.get("identity") or {}).get("run_id")
            history = strategy.setdefault("run_history", [])
            if run_id and not any(item.get("run_id") == run_id for item in history):
                history.append(
                    {
                        "run_id": run_id,
                        "seed": (current.get("identity") or {}).get("seed"),
                        "max_ante": previous.get("ante") or current.get("ante"),
                        "result": "won" if (observation.get("round") or {}).get("won") else "game_over",
                        "ended_at": utc_timestamp(),
                    }
                )
                strategy["updated_at"] = utc_timestamp()
    save_current_run_to_db(current)
    atomic_write_text(SESSION_MARKDOWN_PATH, reliable_rendering.render_session(current))
    return {"event_id": event_id if log_event else None, "current_run": current}


def add_strategy_evidence(rule_id: str, outcome: str, event_id: str, note: str) -> dict[str, Any]:
    if not EVENTS_PATH.exists() or not any(
        json.loads(line).get("event_id") == event_id
        for line in EVENTS_PATH.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ):
        raise ValueError(f"evidence event does not exist: {event_id}")
    strategy = load_strategy()
    updated = record_evidence(strategy, rule_id, outcome, event_id, note)
    evidence_event_id = new_event_id()
    append_jsonl(
        EVENTS_PATH,
        {
            "schema": f"balatro-agent-event/v{SCHEMA_VERSION}",
            "event_id": evidence_event_id,
            "kind": "strategy_evidence",
            "at": utc_timestamp(),
            "rule_id": rule_id,
            "outcome": outcome,
            "evidence_event_id": event_id,
            "note": note,
        },
    )
    # Persist evidence to policies.db (source of truth); strategy.json is deprecated
    _persist_evidence_to_db(rule_id, outcome, event_id, note)
    current = load_json(CURRENT_RUN_PATH, {}) or {}
    return {"event_id": evidence_event_id, "rule": next(rule for rule in updated["rules"] if rule["id"] == rule_id)}


def add_strategy_rule(
    rule_id: str,
    kind: str,
    conditions: dict[str, Any],
    directive: str,
    *,
    absolute: bool = False,
) -> dict[str, Any]:
    if not rule_id or not directive:
        raise ValueError("rule id and directive are required")
    strategy = load_strategy()
    if any(rule.get("id") == rule_id for rule in strategy.get("rules") or []):
        raise ValueError(f"strategy rule already exists: {rule_id}")
    rule = {
        "id": rule_id,
        "kind": kind,
        "conditions": conditions,
        "directive": directive,
        "absolute": absolute,
        "status": "candidate",
        "evidence": [],
        "contradictions": [],
        "last_validated": None,
    }
    strategy.setdefault("rules", []).append(rule)
    strategy["updated_at"] = utc_timestamp()
    event_id = new_event_id()
    append_jsonl(
        EVENTS_PATH,
        {
            "schema": f"balatro-agent-event/v{SCHEMA_VERSION}",
            "event_id": event_id,
            "kind": "strategy_rule_added",
            "at": utc_timestamp(),
            "rule": rule,
        },
    )
    # Persist rule to policies.db (source of truth); strategy.json is deprecated
    _persist_rule_to_db(rule_id, kind, conditions, directive, absolute=absolute)
    current = load_json(CURRENT_RUN_PATH, {}) or {}
    return {"event_id": event_id, "rule": rule}


def enhance_policy_state(policy_state: dict[str, Any], observation: dict[str, Any]) -> dict[str, Any]:
    enhanced = dict(policy_state)
    bridge = dict(enhanced.get("bridge") or {})
    bridge["session_id"] = (observation.get("bridge") or {}).get("session_id")
    bridge["observation_seq"] = (observation.get("bridge") or {}).get("observation_seq")
    bridge["observation_id"] = observation_id(observation)
    enhanced["bridge"] = bridge
    enhanced["schema"] = POLICY_SCHEMA
    enhanced["poker_hand_values"] = observation.get("poker_hands") or enhanced.get("poker_hand_values") or {}
    strategy = load_strategy()
    enhanced["active_directives"] = active_directives(strategy, observation)
    unsupported = []
    for joker in ((observation.get("areas") or {}).get("jokers") or []):
        ability = joker.get("ability") or {}
        if ability and not any(key in ability for key in ("chips", "h_chips", "mult", "h_mult", "x_mult", "h_x_mult", "t_chips", "t_mult")):
            unsupported.append(joker.get("name") or joker.get("center_key"))
    poker_hand_values = observation.get("poker_hands") or {}
    hand_values_valid = bool(
        poker_hand_values.get("schema") == "balatro-poker-hand-values/v1"
        and poker_hand_values.get("valid_for_scoring")
    )
    enhanced.pop("autoplay_suggestion", None)
    enhanced["estimate_quality"] = {
        "kind": "estimate",
        "exact": False,
        "hand_values_valid": hand_values_valid,
        "hand_value_source": poker_hand_values.get("source"),
        "unsupported_effects": unsupported,
        "warning": (
            f"Canonical poker hand values unavailable for scoring: source={poker_hand_values.get('source')}"
            if not hand_values_valid
            else "Scores are heuristic; active effects are not fully modeled."
            if unsupported
            else "Scores are controller estimates, not game-native previews."
        ),
    }
    for action in enhanced.get("legal_actions") or []:
        if action.get("action") == "play" and "estimated_score" in action:
            action["score_kind"] = "estimate"
    analysis = enhanced.get("hand_analysis") or {}
    if analysis.get("best_play"):
        analysis["best_play"]["score_kind"] = "estimate"
    for candidate in analysis.get("play_candidates") or []:
        candidate["score_kind"] = "estimate"
    enhanced["decision_id"] = decision_id(enhanced)
    return enhanced


def status_payload(observation: dict[str, Any] | None) -> dict[str, Any]:
    processes = balatro_processes()
    age = observation_age()
    bridge = (observation or {}).get("bridge") or {}
    seed = observation_seed(observation or {})
    problems = []
    if len(processes) != 1:
        problems.append(f"expected one Balatro.exe process; found {len(processes)}")
    if age is None or age > 2.0:
        problems.append(f"observation stale or missing: age={age}")
    if seed and seed != ALLOWED_SEED:
        problems.append(f"seed mismatch: {seed}")
    if bridge.get("version") != EXPECTED_BRIDGE_VERSION:
        problems.append(
            f"bridge restart required: loaded={bridge.get('version')} expected={EXPECTED_BRIDGE_VERSION}"
        )
    return {
        "schema": f"balatro-agent-status/v{SCHEMA_VERSION}",
        "safe_for_mutation": not problems,
        "problems": problems,
        "processes": processes,
        "observation_age_seconds": age,
        "bridge": {
            "version": bridge.get("version"),
            "session_id": bridge.get("session_id"),
            "observation_seq": bridge.get("observation_seq"),
        },
        "seed": seed,
        "current_run": load_current_run_from_db(),
    }


def read_observation() -> dict[str, Any] | None:
    return load_json(OBSERVATION_PATH)


# Re-export rendering functions for backward compatibility
from .rendering import render_session, render_strategy




