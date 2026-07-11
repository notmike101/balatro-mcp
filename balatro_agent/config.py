from __future__ import annotations

import os
from pathlib import Path

from . import ALLOWED_SEED

ROOT = Path(__file__).resolve().parent.parent
# Source code lives in the MCP repository; mutable game session files belong
# to the separate Balatro runtime directory.  The Rust server sets this value
# for every private backend child.
RUNTIME_ROOT = Path(os.environ.get("BALATRO_RUNTIME_ROOT", ROOT))
AGENT_DIR = RUNTIME_ROOT / "agent"
STATE_DIR = AGENT_DIR / "state"
LOG_DIR = AGENT_DIR / "logs"
CURRENT_RUN_PATH = STATE_DIR / "current_run.json"
STRATEGY_STATE_PATH = STATE_DIR / "strategy.json"
EVENTS_PATH = LOG_DIR / "events.jsonl"
SESSION_MARKDOWN_PATH = AGENT_DIR / "SESSION_STATE.md"
POLICIES_DB = AGENT_DIR / "policies.db"  # Source of truth for strategy rules/lessons
# The single source of truth is now queryable via python agent/policies.py
# STRATEGY_MARKDOWN_PATH removed: file no longer exists

APPDATA = Path(os.environ.get("APPDATA", Path.home() / "AppData" / "Roaming"))
BALATRO_DIR = APPDATA / "Balatro"
COMMAND_PATH = BALATRO_DIR / "codex_command.lua"
OBSERVATION_PATH = BALATRO_DIR / "codex_observation.json"
RESPONSE_PATH = BALATRO_DIR / "codex_response.json"

MAX_OBSERVATION_AGE_SECONDS = 2.0
EXPECTED_BRIDGE_VERSION = "0.6.0"

OBJECTIVE = (
    f"Reach and complete ante 8 on seed {ALLOWED_SEED}; then continue the same run "
    "without starting another run."
)
