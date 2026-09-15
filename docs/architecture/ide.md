# IDE Architecture

The Mekiki IDE is a Tauri application with a CodeMirror 6 editor. This document
records maintenance-sensitive choices that are not obvious from the UI.

## Engine ownership

The automation engine lives on a dedicated worker thread. Rhai state contains
non-`Send` values, and capture/GPU resources should be reused between commands.
The UI communicates with that worker rather than constructing an engine for
each operation.

Engine initialization failure must not prevent the editor from opening. The IDE
can still edit scripts and report the initialization problem.

Pause, stop, timeout, and the emergency shortcut share interruption state with
the running engine. The stop path must not wait behind the job it is intended to
interrupt. Cleanup always releases pressed mouse buttons and modifier keys.

## Taskbar run indicator

While a script runs, the IDE window is usually hidden behind the automated
application, so the taskbar button is the only IDE chrome still in view. The
frontend reports every run-state transition through one command, and
`ide/src-tauri/src/run_indicator.rs` decides how the OS draws it. That module
is the single place that may know about platform presentation. Today it uses
Tauri's taskbar progress bar in indeterminate/paused mode, which is a clear
signal on Windows and degrades to a plain "busy" bar on macOS and Linux. A
platform-idiomatic presentation, such as a dock badge, must be added there and
not in `main.js`. Indicator failures are logged and never abort the run.

## Tauri release behavior

The `custom-protocol` feature is required for release builds. Removing it can
make a packaged application attempt to load the development server. Tauri
capabilities and icon resources are also part of the package contract and must
be verified in a release build, not only under the development server.

## Editor integration

Image literals are identified by the Rhai parser, not a regular expression.
This prevents strings in comments and unrelated string literals from becoming
widgets.

Inline image widgets use CodeMirror atomic ranges so the source literal behaves
as one cursor unit. `WidgetType.eq()` must remain implemented to avoid needless
DOM replacement. CodeMirror renders the visible viewport, but rebuilding every
decoration by scanning the whole document is still linear in document size and
should be measured for very large scripts.

Execution highlighting is driven by Rhai progress callbacks. The callback must
not return `Continue` in a way that changes normal evaluation semantics. The
editor treats step state as transient UI state and clears it after completion or
interruption.

Keyboard shortcuts are installed at the window level so they work when focus is
outside CodeMirror. Composition events must be respected so Enter and other
keys are not intercepted while an IME is active.

## Localization

UI strings live in locale resources. Japanese is the fallback catalog; English
must provide every shipped key. CodeMirror's own phrases are configured through
`EditorState.phrases`, while application commands, tooltips, dialogs, logs, and
backend errors use Mekiki resources or English backend text as appropriate.

Placeholder names are an interface between code and translations. Tests should
check key parity and placeholder parity, not exact sentence structure. English
copy should follow familiar UI terminology rather than mirror Japanese word
order.

