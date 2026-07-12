# Balatro MCP Project Guide

This repository maintains the Rust MCP server, backend, Lovely bridge, and rules tooling. It does not play the game directly.

## MCP runtime interaction

- Gameplay agents must call the configured `balatro` MCP server/tools directly.
- If the `balatro` tools are not exposed in the current session, stop and
  report an MCP host/configuration problem. Do not substitute the server
  executable's CLI, spawn unmanaged server copies, or write a custom MCP
  client.
- Never create helper scripts, command files, observation files, or test
  harnesses in `D:\balatro-desktop` or elsewhere to emulate MCP usage.
- `D:\balatro-desktop\AGENTS.md` is the runtime-side contract. Keep gameplay
  interaction and runtime files out of this repository unless explicitly
  requested.

## Architecture

- `src/main.rs` starts the stdio MCP server.
- `src/tools.rs` defines the MCP server, tools, resources, and handlers.
- `src/backend/` contains policy, scoring, observation, IPC, runtime safety, replay, and Rust-owned state logic.
- `mod/codex_agent.lua` and `mod/lovely.toml` define the Lovely bridge.
- `src/rules.rs` and `rules/balatro_src/` provide the Rust rules database importer and query service.

There are no Python, Node.js, or JavaScript subprocesses. Runtime files and the Balatro installation live separately in `D:\balatro-desktop`.

## Adding or changing tools

1. Add or update the `#[tool]` method in `src/tools.rs`.
2. Route game logic through the Rust backend and preserve mutation serialization.
3. Sanitize game results so commands and face-down card identities never escape.
4. Return the standard `envelope()` shape with structured errors.
5. Update server instructions or guide topics when agent behavior changes.
6. Add deterministic success and failure tests for the route.

## Safety requirements

- Mutations require exactly one Balatro process, a fresh bridge observation, the expected bridge version, and seed `2K9H9HN`.
- Runtime startup is external. `ensure_runtime` verifies state and never launches Balatro.
- Use the exact current `decision_id` and legal `action_id` for every action. When `legal_actions_truncated` is true, page `get_decision` with `action_offset`; use the legal `play_selected`/`discard_selected` actions with 1-based `card_indices` for arbitrary hand positions.
- `observe` is read-only state and `wait_for_state` only confirms state; neither returns actionable legal actions or a decision ID. Call `get_decision` afterward. In `GAME_OVER`, `from_game_over` is a `ui_click`; `return_to_menu` is also a valid `safe_transition`.
- Treat `exact_score` and `estimated_score` separately; unsupported effects must remain visible in `unsupported_effects`.
- Strategy, lessons, estimation feedback, current-run state, and event history are Rust-owned MCP capabilities backed by `agent/rust_state.db`.
- Preserve hidden-card sanitization and never expose arbitrary filesystem contents.

## Validation

Run `cargo fmt --check`, `cargo test --all-targets`, and `cargo build --release`. Use deterministic fakes for process, filesystem, IPC, and rules-database boundaries; live game checks are separate smoke tests.

## Repository workflow

Keep `D:\balatro-mcp` and `D:\balatro-desktop` changes separate. Work on a descriptive branch, commit intentionally, push it, and open a draft PR into `main`. Never commit directly to `main`.
