# Balatro MCP Project Guide

Agents working on this repository are maintaining the MCP server, controller, bridge, and tooling — **not playing the game**. The game-playing agent uses `AGENTS.md` in the runtime directory (`D:\balatro-desktop`) for that.

## Architecture

- **Rust MCP server** — `src/main.rs` (~950 lines). Single binary, stdio transport, `rmcp` SDK. Tools, resources, and handlers all live here.
- **Python controller** — `balatro_agent/` directory. Policy engine, scoring, estimation, IPC bridge interaction, and `decision_checks` generation. The Rust server calls Python via `controller()` helper; direct Python invocations reject without the capability file.
- **Lovely bridge** — `mod/codex_agent.lua` + `mod/lovely.toml`. Copied into Balatro's mod directory; writes observation data to the runtime root.
- **Info DB** — `tools/balatro-info-db/`. Vendored rules database built from local Balatro source.

## Runtime layout

- `D:\balatro-mcp` — this repo (source only, no binaries/saves/logs)
- `D:\balatro-desktop` — runtime directory: game state, session files, logs, capability files. Separate Git repo.

## Rust server

- `src/main.rs` is the entire server. Tools are registered via `#[tool]` attributes on `Server` impl methods.
- `tool_router` macro generates the MCP tool registry. `tool_handler` macro wires the handler.
- `with_instructions()` sets the MCP server-level instructions sent to agents.
- `guide()` function returns topic summaries; `GUIDE_TOPICS` lists available topics.
- `sanitizes()` strips `command` fields and face-down card ranks from tool results.
- `envelope()` wraps results with `ok`, `decision_id`, `legal_actions`, and `error` fields.
- Tests are inline in `mod tests` — run with `cargo test`.

## Python controller

- `controller.py` — policy engine, action generation, `decision_checks`, `slot_metrics`, `score_pressure_metrics`, `consumable_priority`, `booster_pack_eval`, `move_card_actions`, `move_joker_actions`.
- `scoring.py` — hand scoring, best-play analysis.
- `policy.py` — policy state management, replay querying.
- `strategy.py` — strategy rules and evidence.
- `runtime.py` — runtime health checks.
- `ipc.py` — bridge communication.
- `rendering.py` — output formatting.
- `reliability.py` — reliability tracking.
- `estimation_feedback.py` — estimator refinement.
- `storage.py` — persistent storage.
- `config.py` — configuration.
- `__init__.py` — package init.

## Adding a tool

1. Add a method to `Server` impl with `#[tool(description = "...")]` attribute.
2. Call `self.controller()` or `self.status()` to get data from Python.
3. Sanitize results with `sanitizes()` before returning.
4. Wrap with `envelope(ok, data, error_code, error_msg)`.
5. If the tool returns game state, ensure face-down cards are hidden.
6. Update `with_instructions()` if the tool is part of the core flow.
7. Update `guide()` if the tool has a static topic.
8. Run `cargo test` and `cargo build --release`.

## Updating `decision_checks`

The `decision_checks()` function in `controller.py` (around line 1410) returns a dict that the agent treats as mandatory. Each section should have:
- `required`: boolean indicating when this check applies
- `instruction`: clear directive for the agent
- Data fields: the actual state the agent needs

New sections must be referenced in `main.rs` tool descriptions, server instructions, and/or guide topics so agents know to examine them.

## Mod bridge

- `mod/codex_agent.lua` — reads observation state from Balatro and writes to the runtime directory.
- `mod/lovely.toml` — Lovely mod manifest.
- Changes require copying both files into the game's mod directory and restarting.

## Info DB

- `tools/balatro-info-db/README.md` — build instructions from local Balatro source.
- Used by `lookup_rule` and `list_rules` tools.

## Repository discipline

- `D:\balatro-mcp` and `D:\balatro-desktop` are separate Git repos. Preserve unrelated changes.
- Use CodeGraph before grep or direct code reads when `.codegraph/` exists.
- Keep the Rust server minimal — all game logic lives in Python.
- The Python controller rejects direct invocation without a capability from the Rust process. Do not bypass this.
- **All changes must go through a branch-and-PR workflow.** Never commit directly to `main`. Create a descriptive branch, make your changes, open a draft PR to merge into `main`, and request review before merging.
