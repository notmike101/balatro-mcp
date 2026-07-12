# Rust Migration

The Python runtime has been removed. The Rust MCP server owns policy state, exact/estimated scoring, action generation, decision checks, IPC, runtime safety, observation handling, strategy state, lessons, estimation feedback, and SQLite replay logging.

## Backend modules

- `src/backend/policy.rs` — legal actions, policy state, scoring estimates, and decision checks.
- `src/backend/ipc.rs` — file-based Lua bridge commands and response handling.
- `src/backend/runtime.rs` — process, seed, bridge, and observation safety checks.
- `src/backend/observation.rs` — compact observation views.
- `src/backend/replay.rs` — SQLite replay storage and formatting.
- `src/backend/scoring.rs` — typed hand classification, live-contract scoring, modifiers, and estimate metadata.
- `src/backend/state.rs` — fresh Rust-owned current-run, event, strategy, lesson, and estimation storage.

## Server behavior

- MCP routes call the Rust backend directly.
- Rules routes query the Rust-owned SQLite information database directly.
- Runtime startup is external; `ensure_runtime` verifies process safety and never launches the game.
- No Python subprocesses or capability files are used.
- The public contract is `balatro-mcp/envelope/v3` and `balatro-mcp/policy/v3`.

## Validation

1. `cargo fmt --check`
2. `cargo test --all-targets`
3. `cargo build --release`
4. Verify MCP routes through the server and confirm no Python runtime artifacts remain.

For local coverage, install `cargo-llvm-cov` and run `cargo llvm-cov --all-targets --summary-only`. Live process/game interaction remains a separate smoke-test boundary.
