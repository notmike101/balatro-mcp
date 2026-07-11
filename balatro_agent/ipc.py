from __future__ import annotations

import json
import os
import time
from pathlib import Path
from typing import Any

from .config import BALATRO_DIR, COMMAND_PATH, RESPONSE_PATH


def lua_quote(value: str) -> str:
    escaped = (
        value.replace("\\", "\\\\")
        .replace('"', '\\"')
        .replace("\n", "\\n")
        .replace("\r", "\\r")
        .replace("\t", "\\t")
    )
    return '"' + escaped + '"' 


def to_lua(value: Any) -> str:
    if value is None:
        return "nil"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, str):
        return lua_quote(value)
    if isinstance(value, list):
        return "{" + ", ".join(to_lua(item) for item in value) + "}"
    if isinstance(value, dict):
        parts = []
        for key, item in value.items():
            if isinstance(key, (int, float)):
                parts.append(f"[{key}] = {to_lua(item)}")
            else:
                parts.append(f"[{lua_quote(str(key))}] = {to_lua(item)}")
        return "{" + ", ".join(parts) + "}"
    raise TypeError(f"unexpected Lua value: {type(value).__name__}")


def _resolve_field(obj, path):
    """Walk a dot-separated path into a nested dict/list. Returns None if path is invalid."""
    keys = path.split(".")
    for key in keys:
        if isinstance(obj, dict) and key in obj:
            obj = obj[key]
        else:
            return None
    return obj


def _group_actions(actions: list[dict]) -> str:
    """Group legal actions by type+hand_name and return grouped text lines.

    Shows top 3 per group sorted by estimated_score descending.
    """
    lines = []

    # Count by full key (action + hand_name) for accurate summary
    action_counts: dict[str, int] = {}
    for a in actions:
        atype = a.get("action", "?")
        hand = a.get("hand_name")
        key = atype if not hand else f"{atype} [{hand}]"
        action_counts[key] = action_counts.get(key, 0) + 1
    parts = ", ".join(f"{k}:{v}" for k, v in sorted(action_counts.items()))
    lines.append(f"Legal actions: {len(actions)} ({parts})")

    groups: dict[str, list[dict]] = {}
    for a in actions:
        key = f"{a.get('action', '?')}" + (f" [{a.get('hand_name', '')}]" if a.get("hand_name") else "")
        groups.setdefault(key, []).append(a)
    for key, group in sorted(groups.items(), key=lambda x: max(a.get("estimated_score", 0) for a in x[1]), reverse=True):
        top = sorted(group, key=lambda a: a.get("estimated_score", 0), reverse=True)[:3]
        for a in top:
            score = a.get("estimated_score", "")
            lines.append(f"  {a.get('id', '?')} -> {a.get('action', '?')} [{a.get('hand_name', '')}] est={score}")
        if len(group) > 3:
            lines.append(f"  ... and {len(group) - 3} more {key}")
    return "\n".join(lines)


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def next_command_id() -> int:
    previous = 0
    if RESPONSE_PATH.exists():
        try:
            previous = int(read_json(RESPONSE_PATH).get("id") or 0)
        except (OSError, ValueError, TypeError, json.JSONDecodeError):
            previous = 0
    return max(previous + 1, int(time.time() * 1000))


def write_command(command: dict[str, Any], *, echo: bool = True) -> dict[str, Any]:
    BALATRO_DIR.mkdir(parents=True, exist_ok=True)
    command.setdefault("id", next_command_id())
    payload = "return " + to_lua(command) + "\n"
    temporary = COMMAND_PATH.with_suffix(".lua.tmp")
    temporary.write_text(payload, encoding="utf-8")
    os.replace(temporary, COMMAND_PATH)
    if echo:
        print(json.dumps({"wrote": str(COMMAND_PATH), "command": command}, indent=2))
    return command


def wait_for_response(command_id: int | str | None, timeout: float = 5.0, interval: float = 0.05) -> dict[str, Any] | None:
    if command_id is None:
        return None
    deadline = time.time() + timeout
    while time.time() < deadline:
        if RESPONSE_PATH.exists():
            try:
                response = read_json(RESPONSE_PATH)
            except (OSError, json.JSONDecodeError):
                response = None
            if response:
                # Match by command_id (normal case)
                if str(response.get("id")) == str(command_id):
                    return response
                # Decode error: Lua wrote id=nil with _decode_error=true
                # This means the command file was malformed - return it immediately
                if response.get("_decode_error") is True:
                    return response
        time.sleep(interval)
    return None
