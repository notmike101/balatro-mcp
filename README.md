# Balatro MCP

Safe Windows stdio MCP for playing Balatro through legal policy actions. The server is Rust, built with the official [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) SDK. Policy, replay, runtime safety, observation handling, and IPC are implemented in the Rust backend.

This repository intentionally excludes Balatro binaries, saves, Lovely logs, local databases, captured observations, credentials, and Balatro source/data files. You need a legitimate local Balatro installation.

The server uses the standard MCP `tools/list` and `tools/call` methods. The `tools: {}` object in an `initialize` capabilities response means that tool support is enabled; tool definitions are returned by `tools/list`. `callTool` is not an MCP method.

## Install

1. Clone this repository into a code directory such as `D:\balatro-mcp`. Keep the game installation and its mutable runtime files in a separate directory such as `D:\balatro-desktop`.
2. Copy `mod/codex_agent.lua` and `mod/lovely.toml` into your Lovely mod directory. Restart the game and use `runtime_diagnostics` to confirm the bridge loaded.
3. Install Rust. Copy the Lua source files from your own legitimate installation into `rules/balatro_src`, then run `cargo run --release -- rules build` to build `data/balatro.sqlite`.
4. Run `cargo build --release`.

Configure an MCP client:

```json
{
  "mcpServers": {
    "balatro": {
      "command": "D:\\path\\to\\Balatro\\target\\release\\balatro-mcp.exe",
      "env": {
        "BALATRO_MCP_ROOT": "D:\\balatro-mcp",
        "BALATRO_RUNTIME_ROOT": "C:\\Users\\me\\AppData\\Roaming\\Balatro"
      }
    }
  }
}
```

## Agent use

Begin with `game_status`; use `start_new_run(confirm_override=true)` to always start a new seeded run, after confirming that any existing save may be replaced. Then call `get_decision`, inspect `replay_context`, `active_directives`, and `decision_checks.consumables`, execute only a legal `action_id` paired with that exact `decision_id`, and verify with `observe`. Consumables may be used during active hand play and other non-terminal states, not only in SHOP or BLIND_SELECT; use `target_indices` for targeted consumables. If `stale_decision` is returned, use the fresh decision ID and legal action set in the error response, then retry. `observe` is read-only and does not return a decision ID or legal actions; `wait_for_state` only confirms state, reports whether actions are currently available, and returns `next_step: "get_decision"`, so call `get_decision` afterward. If `legal_actions_truncated` is true, call `get_decision` again with `action_offset` set to `legal_actions_next_offset` and an explicit `action_limit`; repeat until `legal_actions_next_offset` is null. The same pagination contract applies after filtering with `action_type`. In ROUND_EVAL use `proceed_round`, then use `next_round` in SHOP. For arbitrary hand positions use the legal `play_selected` or `discard_selected` action with 1-based `card_indices`; `score_hand` inputs and `score_analysis.scoring_cards` use the same 1-based convention, and omitted indices score the live highlight or best five-card subset. Use `hand_values` for the live poker-hand contract. Query matching replays before each blind, log the clear/failure immediately after resolution, and use `strategy_state`, `strategy_add_rule`, `strategy_record_evidence`, and `lesson_add` to preserve durable learning. Record a live checkpoint with `run_state(kind=checkpoint)` before reading `event_history`, which is newest explicit checkpoint first. In GAME_OVER, `from_game_over` is a `ui_click`, while `return_to_menu` is a `safe_transition`. Use `score_hand` for exact-contract or explicitly estimated scoring, and use `lookup_rule` for unfamiliar effects. `runtime_diagnostics` safely returns a capped latest Lovely-log tail and the latest MCP panic-log tail from `agent/mcp_crash.log`; `ensure_runtime` verifies an externally started Balatro process and never launches the game.

The MCP exposes no raw controller commands, coordinates, arbitrary filesystem reads, database rebuilds, or face-down-card identities. See [`AGENTS.md`](AGENTS.md) for the strict gameplay workflow.

## Shared runtime and recovery

Multiple configured MCP sessions may remain connected, but mutations of the
shared Balatro runtime are serialized by `agent/mcp_runtime.lock` across
processes. A `mutation_busy` result means another mutation is still in
progress: wait, reread `game_status`/`get_decision`, and never retry an old
action. After a successful bridge action, `take_action` may include
`data.decision_record.stored=false`; this is a nonfatal audit warning and the
action has already been applied.

For a deliberate persistence reset, stop stale `balatro-mcp.exe` processes
first, then run the release binary with the configured runtime root:

```powershell
$env:BALATRO_RUNTIME_ROOT = "C:\Users\me\AppData\Roaming\Balatro"
& "D:\balatro-mcp\target\release\balatro-mcp.exe" state reset --confirm
```

The command refuses missing confirmation or an owned runtime lock, and archives
both `rust_state.db` and `replays.db` (including SQLite sidecars) under a UTC
timestamped `agent\archive-state-reset-*` directory. It does not delete the
archives or start Balatro. Stop stale MCP processes before reset and before a
release rebuild. `runtime_diagnostics` reports capped database sizes, row
counts, largest snapshots/notes, and lock availability.

The static rules database is managed by the Rust binary. Use `balatro-mcp rules build`, `balatro-mcp rules lookup`, `balatro-mcp rules list`, and `balatro-mcp rules stats` for local maintenance and inspection.
