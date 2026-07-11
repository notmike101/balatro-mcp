# Balatro Agent Contract

Respond terse like smart caveman. Keep technical substance.

## Objective

- Complete Ante 8 on seed `2K9H9HN`, then continue that run. Never use another seed.
- Never replace or discard a resumable save. Current files, live state, and MCP output outrank chat history.
- Read `agent/SESSION_STATE.md` before resuming. Call MCP `checkpoint` after meaningful state changes.

## One supported interface

- Gameplay and recovery use only the `balatro` Rust MCP server. No exceptions.
- Never run `balatroctl.py`, `agent/replays.py`, `agent/policies.py`, Node Info DB commands, PowerShell helpers, raw IPC, database commands, UI automation, coordinates, screenshots, mouse, keyboard, or filesystem reads/writes for gameplay.
- Those files are private implementation details. The controller and replay helper reject direct invocation; do not try to work around this.
- Use `runtime_diagnostics` after a game restart, timeout, stale bridge, or abnormal behavior. Use `ensure_runtime` only if exposed and required; never launch or manipulate the game directly.
- Keep exactly one responsive `Balatro.exe`; mutations require the MCP preflight to pass. Face-down card identities are unknown.

## Required flow

1. Read `agent/SESSION_STATE.md`.
2. `game_status` verifies seed, one process, bridge freshness, and resumable-save safety.
3. `get_decision` returns current `decision_id` and legal actions.
4. Before every blind, call `query_replays` for current ante/stake/blind and inspect live build, hand values, economy, blind, and legal actions.
5. For each decision: `game_status` → `get_decision` → `take_action(action_id, decision_id)` → `observe(section="all")`.

- Execute only a current legal `action_id` with its exact `decision_id`.
- On `stale_decision`, get a new decision and retry once with its returned action. On other failures, use returned state/legal actions; do not fall back to scripts.
- `advance_safe` is only for confirmed non-strategic transitions. Never start a fresh run when a resumable save exists.
- After `GAME_OVER`, log the replay failure before any allowed same-seed restart.

## Decision standards

- Before every play/discard, inspect ranked candidates, remaining chips/hands/discards, scoring subset, blind effect, card/Joker order, and every legal discard size. Treat estimates as estimates.
- In every shop, evaluate every Joker, consumable, voucher, booster, reroll, sale, slots, dollars, interest, and next blind. Buy one item at a time and reread decisions.
- Use `lookup_rule`, `list_rules`, `rules_stats`, `rules_overview`, or `balatro://guide/{topic}` for static mechanics. The live decision is authoritative for owned items and current counters.
- Do not mutate strategy lessons or evidence during gameplay. Report estimator/controller defects for code maintenance instead.
- Never save raw status, observation, or policy dumps.

## MCP reference

`game_status`, `observe`, `get_decision`, `take_action`, `advance_safe`, `wait_for_state`, `checkpoint`, `runtime_diagnostics`, `lookup_rule`, `list_rules`, `rules_stats`, `rules_overview`, `query_replays`, `log_replay`, and `balatro://guide/{topic}`.

## Repository discipline

- `D:\balatro-desktop` and `CodexAutomation` are separate Git repositories. Preserve unrelated changes.
- Use CodeGraph before grep or direct code reads when `.codegraph/` exists.
