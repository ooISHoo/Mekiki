# Mekiki MCP startup guide

Installing Mekiki does not start, register, or authorize the MCP server. The
installer does not add a service, scheduled task, startup entry, or AI-client
configuration.

## Required confirmation

An AI agent must not start `mekiki-mcp.exe` merely because it is installed or
discoverable. It may prepare to start the server only after the user explicitly
asks to use Mekiki MCP. Before starting it or adding it to an MCP client, the
agent must show:

- the absolute path to `mekiki-mcp.exe`;
- the writable `--base` directory;
- whether the server is observation-only or can control the desktop;
- every executable allowed through `--allow-launch`;
- whether the client configuration would persist and auto-launch it later.

The agent must then ask a direct yes/no question and wait for an affirmative
answer in the current conversation. Installation and an earlier approval are
not approval for a new server start.

`--takeover` bypasses the desktop lease. Do not use it unless the user requests
that exact diagnostic behavior and confirms it separately.

## Locate the installed server

The server is installed next to the Mekiki application. A typical per-user
installation is under `%LOCALAPPDATA%`; an administrator-selected installation
may be under `%ProgramFiles%`.

Before proposing a command, locate the exact executable and display the result:

```powershell
Get-ChildItem $env:LOCALAPPDATA,$env:ProgramFiles `
  -Filter mekiki-mcp.exe -Recurse -ErrorAction SilentlyContinue
```

Do not start every result. Resolve ambiguity with the user.

## Command templates

Safest, observation-only mode:

```powershell
& "C:\absolute\path\mekiki-mcp.exe" `
  --base "C:\Users\name\Documents\MekikiWork" `
  --observe
```

Desktop-control mode, only after the required confirmation:

```powershell
& "C:\absolute\path\mekiki-mcp.exe" `
  --base "C:\Users\name\Documents\MekikiWork"
```

Do not add `--allow-launch` unless a specific executable is needed. It accepts
an exact absolute path and may be repeated. Do not use a shell as a broad
launcher.

MCP clients normally launch a stdio server from their configuration. Treat
writing that configuration as a separate persistent change: show the exact
configuration and ask before writing it. Prefer a disabled or session-scoped
entry when the client supports one.

See `docs/mcp-reference.md` for all options and operational constraints.
