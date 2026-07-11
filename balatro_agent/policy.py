from __future__ import annotations

from typing import Any

DECISION_STATES = {
    "BLIND_SELECT",
    "SELECTING_HAND",
    "SHOP",
    "GAME_OVER",
    "TAROT_PACK",
    "PLANET_PACK",
    "SPECTRAL_PACK",
    "STANDARD_PACK",
    "BUFFOON_PACK",
}

SAFE_TRANSITION_ACTIONS = {
    "skip_tutorial",
    "dismiss_unlock_overlay",
    "ensure_menu_ui",
    "cash_out",
}


def is_decision_state(state: str | None) -> bool:
    return str(state or "") in DECISION_STATES


def select_safe_transition(policy_state: dict[str, Any]) -> dict[str, Any] | None:
    state = (policy_state.get("game") or {}).get("state")
    if is_decision_state(str(state) if state is not None else None):
        return None
    return next(
        (
            action
            for action in policy_state.get("legal_actions") or []
            if action.get("action") in SAFE_TRANSITION_ACTIONS
        ),
        None,
    )

