# Balatro MCP

Safe Windows stdio MCP for playing Balatro through legal policy actions. The server is Rust, built with the official [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) SDK. Policy, replay, runtime safety, observation handling, and IPC are implemented in the Rust backend.

This repository intentionally excludes Balatro binaries, saves, Lovely logs, local databases, captured observations, credentials, and Balatro source/data files. You need a legitimate local Balatro installation.

## Install

1. Clone this repository into a code directory such as `D:\balatro-mcp`. Keep the game installation and its mutable runtime files in a separate directory such as `D:\balatro-desktop`.
2. Copy `mod/codex_agent.lua` and `mod/lovely.toml` into your Lovely mod directory. Restart the game and use `runtime_diagnostics` to confirm the bridge loaded.
3. Install Node.js 22.5+ and Rust. Build the knowledge database from your own local game source as documented in [`tools/balatro-info-db/README.md`](tools/balatro-info-db/README.md).
4. Run `cargo build --release`.

Configure an MCP client:

```json
{
  "mcpServers": {
    "balatro": {
      "command": "D:\\path\\to\\Balatro\\target\\release\\balatro-mcp.exe",
      "env": {
        "BALATRO_MCP_ROOT": "D:\\balatro-mcp",
        "BALATRO_RUNTIME_ROOT": "D:\\balatro-desktop"
      }
    }
  }
}
```

## Agent use

Begin with `game_status`, then `get_decision`. Execute only a legal `action_id` paired with that exact `decision_id`, then verify with `observe`. Query matching replays before each blind and use `lookup_rule` for unfamiliar effects. `runtime_diagnostics` safely returns a capped latest Lovely-log tail; `ensure_runtime` verifies an externally started Balatro process and never launches the game.

The MCP exposes no raw controller commands, coordinates, arbitrary filesystem reads, database rebuilds, or face-down-card identities. See [`AGENTS.md`](AGENTS.md) for the strict gameplay workflow.
