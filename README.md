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

Begin with `game_status`, then use `get_decision` and execute one current legal action with its `decision_id`. The default decision is a small action-loop snapshot; call `decision_context(section="recall|replay|checks|scoring")` only when deeper analysis is needed, or use `detail="full"` for the legacy expanded response. Use exact 1-based `card_indices` and `target_indices`, refresh after `stale_decision` or `mutation_busy`, and page when `legal_actions_truncated` is true. `observe` and `wait_for_state` are read-only; call `get_decision` afterward for actions. In `ROUND_EVAL` use `proceed_round`, then `next_round` in `SHOP`. Never infer hidden cards. Use `hand_values`, `score_hand`, `lookup_rule`, replay tools, and durable state tools deliberately when their detail is needed.

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
