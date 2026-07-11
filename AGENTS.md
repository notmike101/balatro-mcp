# Balatro MCP Project Guide

This repository maintains the Rust MCP server, backend, Lovely bridge, and rules tooling. It does not play the game directly.

## Architecture

- `src/main.rs` starts the stdio MCP server.
- `src/tools.rs` defines the MCP server, tools, resources, and handlers.
- `src/backend/` contains policy, scoring, observation, IPC, runtime safety, replay, and Rust-owned state logic.
- `mod/codex_agent.lua` and `mod/lovely.toml` define the Lovely bridge.
- `tools/balatro-info-db/` is the vendored Node.js rules database.

There are no Python entrypoints or Python subprocesses. Runtime files and the Balatro installation live separately in `D:\balatro-desktop`.

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
- Use the exact current `decision_id` and legal `action_id` for every action.
- Treat `exact_score` and `estimated_score` separately; unsupported effects must remain visible in `unsupported_effects`.
- Strategy, lessons, estimation feedback, current-run state, and event history are Rust-owned MCP capabilities backed by `agent/rust_state.db`.
- Preserve hidden-card sanitization and never expose arbitrary filesystem contents.

## Validation

Run `cargo fmt --check`, `cargo test --all-targets`, and `cargo build --release`. Use deterministic fakes for process, filesystem, IPC, and Node boundaries; live game checks are separate smoke tests.

## Repository workflow

Keep `D:\balatro-mcp` and `D:\balatro-desktop` changes separate. Work on a descriptive branch, commit intentionally, push it, and open a draft PR into `main`. Never commit directly to `main`.
