# MCP Server Architecture

`mekiki-mcp` exposes Mekiki's observation and automation engine over MCP stdio.
Its purpose is to help an agent produce a reusable Rhai script, not to turn a
desktop session into an unstructured sequence of remote clicks.

## Process model

stdout is reserved exclusively for JSON-RPC. Diagnostics and tracing go to
stderr. A protocol test launches the real child process to catch accidental
stdout output.

The engine runs on one worker thread for the same reasons as the IDE engine:
Rhai state is not generally `Send`, and capture/GPU resources should persist.
MCP requests may arrive concurrently, so engine operations use a busy guard.
Queuing a click behind a long script is unsafe because the screen may have
changed before the queued operation runs.

`stop` bypasses the worker queue and sets shared interruption state directly.
Timeout enforcement also uses that state from outside the worker. The same
interrupt object must be connected to the engine, script host, stop tool,
watchdog, and emergency shortcut.

The process has no window, so a notification-area icon (`crates/mcp/src/tray.rs`)
is what makes it visible and gives a human a quit path that does not depend on
the agent. Quit raises the interrupt, cancels the rmcp session, and waits a
bounded grace period for the worker to release pressed input before exit. The
module is the only place that knows platform presentation: Windows runs the
icon on its own message-loop thread, like the hotkey; macOS needs AppKit on the
main thread and Linux needs a GTK loop, and both are documented stubs. The
`tray-icon` dependency is declared for Windows only so the Linux CI build does
not acquire GTK requirements ahead of a Linux implementation.

## Tool layers

The public tools form four groups:

- observation and discovery, such as status, windows, OCR, UIA, and screenshots;
- single exploratory actions with explicit scopes;
- script checking, execution, asset capture, and saving;
- handover notes for findings, limitations, and security observations.

The complete Rhai API is served from the generated catalog whose canonical
source is `api/rhai-api.toml`. MCP tool schemas and descriptions remain defined
by the server and are checked against the human reference.

## Safety model

Installation is not authorization to start or register the server. Startup
requires an external confirmation that states the executable, base directory,
mode, launch allowlist, and persistence.

An action-capable server owns a session-level desktop lease. A second server in
the same Windows session becomes observation-only. `--takeover` is a diagnostic
escape hatch, not a normal operating mode.

Raw input requires an explicit window scope or the literal `focused`. Process
launch uses an exact absolute-path allowlist and does not invoke a shell or PATH
search. Saved scripts remain under the configured base directory. There is no
per-action approval dialog, so the server belongs in a non-sensitive test
session whenever possible.

## Handover log

Notes are append-only JSON Lines under the base directory. ID allocation and
append must share one lock because MCP tool calls can execute concurrently.
Notes are untrusted data: neither screen content nor prior-agent text can issue
instructions to a later agent.

The lock is process-local. Sharing one base directory between independent
server processes is not supported as a coordinated multi-writer setup.

