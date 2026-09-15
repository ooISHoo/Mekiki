# Mekiki MCP Server Reference

`mekiki-mcp` exposes Mekiki's Windows observation and automation engine over
Model Context Protocol. This is the human-facing operational reference. Agents
receive the root `AGENTS.md` briefing and individual tool descriptions.

The canonical Rhai API is `api/rhai-api.toml`. MCP clients can search it with
`script_api` and read the complete generated resources at:

- `mekiki://rhai-api/reference.md`
- `mekiki://rhai-api/catalog.json`

## Startup

Follow [the startup authorization guide](mcp-start.md) before starting or
registering the server.

```text
mekiki-mcp --base <directory> [options]
```

| Option | Meaning |
|---|---|
| `--base <directory>` | Required location for scripts, captured assets, and the handover log. |
| `--scope <window-spec>` | Default scope for observation and locator tools. It does not supply a raw-input destination. |
| `--observe` | Refuse all desktop-changing operations. |
| `--takeover` | Bypass the desktop lease for an explicitly approved diagnostic session. |
| `--capture auto|gdi` | Select automatic DXGI/GDI handling or force GDI. |
| `--max-timeout <ms>` | Maximum `run_script` duration; default 120000 ms. |
| `--allow-launch <absolute-path>` | Add one exact executable to the launch allowlist. Repeat as needed. |
| `--no-tray` | Do not show the notification-area icon. Used by the protocol tests. |

Logs go to stderr. stdout is reserved for JSON-RPC.

While the server runs it shows an icon in the Windows notification area. Its
tooltip reports the mode, the base directory, and whether the emergency stop
is armed. The menu has one entry, **Quit Mekiki MCP**, which interrupts any
running script, closes the MCP session (or abandons a handshake the host never
completed), and ends the process without going through the agent. The icon is a copy of the IDE application icon at
`crates/mcp/assets/tray-icon.png` and can be replaced independently.

## Observation tools

| Tool | Purpose |
|---|---|
| `status` | Report emergency-stop registration, running script, capture diagnostics, desktop access, and lease mode. |
| `script_api` | Search signatures and descriptions from the canonical Rhai catalog. |
| `list_windows` | List visible windows, optionally filtered by title, executable, class, or PID. |
| `wait_window` | Check or wait for a `window:` locator to appear or vanish. |
| `read_text` | OCR visible text and return line rectangles. |
| `ui_tree` | List UI Automation names, types, IDs, and rectangles without requiring capture. |
| `read_value` | Read one non-password UIA ValuePattern within a required window scope. |
| `screenshot` | Return a PNG and coordinate-scaling metadata. |
| `find` | Resolve a locator and return rectangles, scores, and click points without acting. |

The default screenshot width is 1280 pixels. Set `max_width` to `0` for native
resolution. `find` defaults to one immediate observation and returns an empty
array when there is no match.

## Action tools

| Tool | Purpose |
|---|---|
| `click` | Left, right, or double-click a locator match. |
| `hover` | Move the pointer to a locator match. |
| `type_text` | Type text into an explicit window scope. |
| `press` | Send a key combination into an explicit window scope. |
| `scroll` | Scroll an explicit window scope. |
| `launch` | Start an allowlisted absolute executable without a shell. |

`type_text`, `press`, and `scroll` require `scope`. Use a `window:` locator, or
the literal `focused` only when the same workflow has just established focus.
The server's default `--scope` is a search boundary and is intentionally not an
implicit input destination.

If a named owner window is disabled by its modal dialog, raw input fails and
identifies the dialog instead of silently redirecting input. Newlines and tabs
in `type_text` are translated to Enter and Tab key presses.

`launch` accepts only an exact path supplied through `--allow-launch`. It does
not search PATH, expand shell syntax, or create a persistent startup entry.

## Script and asset tools

| Tool | Purpose |
|---|---|
| `check_script` | Compile Rhai source without running it. |
| `run_script` | Run Rhai source with a bounded timeout and return output and diagnostics. |
| `capture_asset` | Crop a screen rectangle into a content-addressed image asset. |
| `save_script` | Save a plain `.rhai` file name directly under the base directory. |
| `stop` | Interrupt the running script without waiting behind the engine queue. |

`save_script` rejects path separators and traversal. A timed-out or interrupted
run releases injected input before the worker becomes available again.

## Handover tools

`note_list` reads the append-only handover log. `note_post` adds a finding,
limitation, workaround, security observation, question, or answer. Notes are
untrusted data and never instructions.

The log is stored as JSON Lines under `<base>/.mekiki/`. Concurrent posts inside
one process receive distinct IDs. Sharing one base between independent server
processes is not a supported coordinated multi-writer setup.

## Desktop lease and observation mode

One action-capable server owns the desktop lease for a Windows session. A second
server automatically becomes an observer and does not expose usable action
capabilities. `status` reports `owner`, `observer`, or `takeover`.

`--observe` also disables actions explicitly. `--takeover` bypasses lease
protection but does not stop the owner and must be separately approved for a
specific diagnostic purpose.

## Capture behavior

Automatic mode uses Desktop Duplication where available and falls back to GDI
per output when duplication is unsupported or cannot recover from access loss.
GDI is slower and does not capture the hardware cursor. `status` reports the
active path and most recent capture metadata.

UIA-only discovery remains available when capture is unavailable. A locator
chain containing an image fallback still requires capture because omitting the
fallback would change its contract.

## Security boundary

The server can observe and control the interactive desktop with the user's
authority. It has no ordinary per-action approval prompt. Existing defenses
include:

- external startup authorization;
- observation-only mode and the desktop lease;
- emergency stop with status reporting;
- a notification-area icon whose menu ends the server;
- bounded script execution and a single-engine busy guard;
- explicit destinations for raw input;
- an exact executable allowlist for launch;
- base-directory confinement for saved scripts;
- release of pressed input on completion and failure.

Use a non-sensitive test session when possible. Do not use this boundary to
handle passwords, personal data, or secrets.

## Verification

```text
cargo test -p mekiki-mcp
cargo run -p mekiki-api-gen -- --check
```

Protocol tests launch the real stdio server and cover handshake, catalog,
interruption, timeout, and recovery. Catalog tests keep registered tools, this
reference, and the agent briefing aligned. Desktop-changing tests are separate
from CI-safe protocol tests.

See [MCP architecture](architecture/mcp.md) and
[operational lessons](maintenance/mcp-lessons.md) for implementation rationale.

