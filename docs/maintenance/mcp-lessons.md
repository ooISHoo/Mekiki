# MCP Operational Lessons

This is the stable maintenance summary extracted from the dated agent-test
rounds. Chronology belongs in Git history; these constraints remain relevant to
the current server.

## Interruption and concurrency

The engine, Rhai host, watchdog, `stop` tool, and emergency shortcut must share
one interrupt object. A previous split left long-running scripts unstoppable
while every component appeared to signal correctly. Protocol tests cover stop,
timeout, and recovery after interruption.

Engine tools run one at a time. MCP dispatch is concurrent, but queuing screen
actions creates stale operations that execute after the desktop changes. `stop`
is the deliberate exception and bypasses the queue.

The handover log's ID allocation and append are one critical section. JSON Lines
kept the file syntactically valid during a race while assigning duplicate IDs,
which made the bug easy to miss.

## Observation and desktop ownership

UIA-only discovery must remain available when capture is unavailable. Status
separates an inaccessible interactive desktop from a valid desktop containing
no matching windows.

Only one action-capable server owns a Windows-session desktop lease. Additional
servers become observers. `--takeover` bypasses this protection only for an
explicit diagnostic session and does not terminate the lease owner.

Capture status includes the active backend and the most recent capture metadata.
This is necessary for diagnosing systems that silently changed from DXGI to GDI.

## Focus and dialogs

All raw input names its destination. A stale confirmation dialog once interpreted
letters from intended text as button accelerators while every tool returned
success. Modal-owner detection and mandatory scopes turn that class of failure
into an actionable error.

Do not use a generic Close or OK button as proof that the intended application
started. Wait for window identity or an application-specific element.

## Application launch

`launch` accepts only exact absolute executable paths configured with repeated
`--allow-launch` options. It performs no shell expansion and no PATH lookup.
Startup synchronization uses `wait_window` or Rhai `expect_window`, not a fixed
delay or a Run-dialog workaround.

On Windows 11, launching `cmd.exe` through the Run dialog can reuse an existing
Windows Terminal process. Automation tests that need an isolated console should
use an explicitly allowed executable and verify its window identity.

## OpenOffice observations

OpenOffice tests require an isolated user profile. Stale lock files and the
startup center can otherwise change behavior between runs.

The disappearance of a Save dialog does not prove that file writing completed.
Verify a durable postcondition such as the updated file, cleared modified marker,
or application-ready state. Separating save and close into independently
verified steps was more reliable than adding an arbitrary sleep.

The OpenOffice accessibility provider may expose descendants and value patterns
inconsistently. Tests should distinguish provider capability from Mekiki lookup
failures and use ODF-level validation for saved document content.

## Process exit

Ending the process from inside (the tray's Quit) is not the same as the host
closing stdin. tokio's stdin is an uncancellable blocking read on a helper
thread, and dropping the runtime waits for that read, so the process lingers
with its tray icon already gone. `main` detaches the runtime with
`shutdown_background` before returning. The protocol tests end every server by
closing stdin and therefore cannot catch a regression here; the tray path
needs a manual check.

Quit must also work before the host completes the initialize handshake, since
`serve` does not return until then. The quit signal is raced against the
handshake and only afterwards attached to the session's cancellation token.

## Protocol invariants

- stdout contains JSON-RPC and nothing else.
- Malformed or invalid tool input fails loudly with actionable English text.
- Tool schemas, the human reference, and registered handlers stay in sync.
- A timed-out script releases input and does not poison the next request.
- Observation-only mode does not expose action capabilities as usable tools.

