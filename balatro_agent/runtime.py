from __future__ import annotations

import csv
import io
import subprocess
import time
from pathlib import Path
from typing import Any

from . import ALLOWED_SEED
from .config import EXPECTED_BRIDGE_VERSION, MAX_OBSERVATION_AGE_SECONDS, OBSERVATION_PATH


class SafetyError(RuntimeError):
    pass


def balatro_processes() -> list[dict[str, Any]]:
    completed = subprocess.run(
        ["tasklist", "/FI", "IMAGENAME eq Balatro.exe", "/FO", "CSV", "/NH"],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        return []
    rows: list[dict[str, Any]] = []
    for row in csv.reader(io.StringIO(completed.stdout)):
        if len(row) < 2 or row[0].lower() != "balatro.exe":
            continue
        try:
            pid = int(row[1])
        except ValueError:
            continue
        rows.append({"pid": pid, "name": row[0]})
    return rows


def observation_age(path: Path = OBSERVATION_PATH, now: float | None = None) -> float | None:
    if not path.exists():
        return None
    current = time.time() if now is None else now
    return max(0.0, current - path.stat().st_mtime)


def observation_seed(observation: dict[str, Any]) -> str | None:
    round_data = observation.get("round") or {}
    ready = observation.get("ready") or {}
    seed = round_data.get("seed") or ready.get("saved_game_seed")
    return str(seed) if seed else None


def validate_seed(observation: dict[str, Any], requested_seed: str | None = None) -> None:
    if requested_seed and requested_seed != ALLOWED_SEED:
        raise SafetyError(f"seed rejected: {requested_seed}; only {ALLOWED_SEED} is allowed")
    live_seed = observation_seed(observation)
    if live_seed and live_seed != ALLOWED_SEED:
        raise SafetyError(f"live or saved seed rejected: {live_seed}; expected {ALLOWED_SEED}")


def validate_runtime(
    observation: dict[str, Any],
    *,
    processes: list[dict[str, Any]] | None = None,
    age_seconds: float | None = None,
    require_version: bool = True,
    allow_seed_mismatch: bool = False,
) -> None:
    found = balatro_processes() if processes is None else processes
    if len(found) != 1:
        raise SafetyError(f"expected exactly one Balatro.exe process; found {len(found)}")
    age = observation_age() if age_seconds is None else age_seconds
    if age is None or age > MAX_OBSERVATION_AGE_SECONDS:
        raise SafetyError(f"observation stale or missing: age={age}")
    bridge = observation.get("bridge") or {}
    if not bridge.get("loaded"):
        raise SafetyError("CodexAutomation bridge not loaded")
    if require_version and bridge.get("version") != EXPECTED_BRIDGE_VERSION:
        raise SafetyError(
            f"bridge version {bridge.get('version')} loaded; restart Balatro to load {EXPECTED_BRIDGE_VERSION}"
        )
    if not bridge.get("session_id"):
        raise SafetyError("bridge session_id missing")
    if not allow_seed_mismatch:
        validate_seed(observation)


def guard_command(command: dict[str, Any], observation: dict[str, Any]) -> None:
    action = str(command.get("action") or command.get("type") or "")
    requested_seed = command.get("seed")
    ready = observation.get("ready") or {}
    live_seed = observation_seed(observation)
    wrong_seed_recovery = (
        live_seed not in {None, "", ALLOWED_SEED}
        and (
            action == "setup_new_run"
            or (action == "ui_click" and command.get("ui_id") == "main_menu_play")
            or (
                action == "start_run"
                and str(requested_seed or "") == ALLOWED_SEED
                and ready.get("current_setup") == "New Run"
            )
        )
    )
    if not wrong_seed_recovery:
        validate_seed(observation, str(requested_seed) if requested_seed else None)
    if ready.get("saved_game_present"):
        if action == "setup_new_run":
            if str(ready.get("saved_game_seed") or "") == ALLOWED_SEED:
                raise SafetyError("saved run exists; resume it instead of starting a new run")
        if (
            action == "start_run"
            and requested_seed
            and str(ready.get("saved_game_seed") or "") == ALLOWED_SEED
        ):
            raise SafetyError("saved run exists; resume it without supplying a seed")
    validate_runtime(observation, allow_seed_mismatch=wrong_seed_recovery)

