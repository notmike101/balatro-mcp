# Balatro Agent Quick Reference

Use only the `balatro` MCP server. Do not run controller, replay, database, IPC, PowerShell, Python, Node, UI, or filesystem commands for gameplay.

Startup: read `agent/SESSION_STATE.md`; call `game_status`; if unhealthy call `runtime_diagnostics`; call `get_decision`; query matching `query_replays` before selecting a blind.

Decision loop: `game_status` → `get_decision` → `take_action(action_id, decision_id)` → `observe(section="all")`.

Use `lookup_rule` for unfamiliar static effects and `rules_overview`/`balatro://guide/{topic}` for concepts. Face-down cards remain unknown. Reread `get_decision` after every mutation.
