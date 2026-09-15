# Mekiki examples

Run these scripts in number order. They cover representative parts of the
Rhai API without writing files, launching a shell, or depending on fixed pixel
coordinates.

```powershell
cargo build --release -p mekiki-scripting --bin mekiki
target\release\mekiki.exe run examples\01-observe.rhai
```

| Script | Level | What it demonstrates | Prerequisite |
|---|---|---|---|
| `01-observe.rhai` | Basic | Windows, screens, regions, coordinate locators, settings | None |
| `02-calculator-ui.rhai` | Intermediate | UI Automation IDs, click, OCR, `expect` | Windows Calculator open in standard mode |
| `03-calculator-input.rhai` | Advanced | `expect_window`, functions, arrays, HWND-fixed `Region.press` | Windows Calculator open and frontmost in standard mode |

The Calculator examples use language-independent UI Automation IDs. They work
with any Calculator display language and only change the displayed calculation;
they do not save data. The advanced example scopes keyboard input through the
topmost `ApplicationFrameHost.exe` window, so Calculator must be frontmost when
the script starts.

Image matching is intentionally not represented by a bundled screenshot.
Screenshots become stale with DPI, theme, and application changes. Use the IDE's
capture and match-preview workflow to create an image for the application being
automated, then follow the [script guide](../docs/script-guide.md)
and the generated [API reference](../api/generated/rhai-api.md).

The examples do not attempt to cover every API. Hardware-independent behavior
is covered more reliably by `crates/core/tests/pipeline.rs` and
`crates/scripting/tests/script.rs`.
