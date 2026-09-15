<!-- Generated from api/rhai-api.toml by `cargo run -p mekiki-api-gen`. DO NOT EDIT. -->

# Mekiki Rhai API

This is the complete scripting contract registered by `ScriptHost`. The API is
dynamic and therefore its functions do not appear as ordinary Rust functions.
Coordinates are physical pixels in the Windows virtual desktop, durations are
milliseconds, and operation failures stop evaluation with a Rhai runtime error
unless a return value explicitly represents absence or timeout.

## Execution model

A `Target` is a lazy description: construction does not capture the screen.
Actions resolve it, wait for it to appear, and normally wait for the matched
pixels to become stable. A `Match` is a coordinate snapshot and does not follow
later screen changes.

Prefer a window scope and an assertion over global input and fixed delays:

```rhai
let win = expect_window("exe=notepad.exe").to_appear(10_000);
win.target("ui:name=Save,type=button").click();
expect(win.target("ocr:Saved")).to_appear(5_000);
```

## Waiting for UI

Waiting for a UI part to appear is an assertion. `expect(t).to_appear(ms)`
retries at the global scan interval (`333` ms) and returns the `Match`, so the
result can be acted on without searching again:

```rhai
let m = expect(win.target("save.png")).to_appear(10_000);
m.click();
```

Every retry is a full search, so a long wait at the built-in interval keeps the
CPU or GPU busy. For waits measured in minutes, poll at your own pace:
`exists` looks exactly once, and `sleep` is interruptible.

```rhai
let t = win.target("save.png");
let waited = 0;
while !t.exists() {
    if waited >= 600_000 { throw "not found"; }
    sleep(2_000);
    waited += 2_000;
}
t.click();
```

A `ui:` locator queries UI Automation instead of matching pixels and makes
either form cheap. Scoping to a window or region shrinks the searched area.

## Locator strings

`image:path.png` and an unprefixed path use template matching.
`image:sha256:<hex>` addresses the asset store. `ocr:text` matches recognized
screen text. `ui:Save` matches an accessibility name, while the long form accepts
`name`, `id`, and `type`. Supported UI types are `button`, `checkbox`,
`combobox`, `edit`, `hyperlink`, `image`, `list`, `listitem`, `menuitem`,
`radiobutton`, `tab`, `tabitem`, `text`, `tree`, `treeitem`, and `window`;
`input`, `textbox`, `link`, `label`, `radio`, and `dropdown` are accepted
aliases, and any other type is an error.

Geometric locators are `point:x,y`, `offset:dx,dy`, and
`region:x,y,width,height`. `window:title` selects the frontmost title substring.
The structured window form accepts `title`, `title_exact`, `exe`, `class`, `pid`,
and zero-based `index`, combined with AND. `title` supports `*` and `?` wildcards;
`class` is a prefix match. Escape a comma in a value as `\,` and a backslash as
`\\`. Prefer `exe` for ordinary desktop applications. Store/UWP applications
may share a host executable and should use a title condition.

Locator prefixes are a reserved namespace. Two or more lowercase ASCII letters
before a `:` are always read as a prefix, and an unknown prefix is an error,
never an image path — so future locator kinds cannot change the meaning of an
existing script. `sha256:` is additionally reserved for content-addressed image
references. A Windows drive letter is a single character and is therefore
never mistaken for a prefix.

## Input and key names

Global input does not identify or activate a window. Prefer window or target
methods. Key combinations join modifiers and one final key with `+`. Modifiers
are `ctrl`/`control`, `shift`, `alt`, and `win`/`meta`/`cmd`. Named keys include
Enter, Tab, Escape, Space, Backspace, Delete, Insert, Home, End, Page Up/Down,
arrows, F1-F24, Caps Lock, Print Screen, letters, digits, punctuation, and numeric
keypad names. Printable keys depend on keyboard layout; use Unicode text input
for text. Line breaks and tabs in typed text are emitted as Enter and Tab.

## Applications with a software cursor

Some applications — games, remote-desktop viewers, some canvas-style tools —
hide the system pointer and draw their own cursor into their frame. Two
defaults that are right for ordinary windows work against them.

**Rechecking.** Every action searches for its target again immediately before
acting, so a click lands where the target is now, not where it was. `hover`
moves the pointer onto the target, the application then draws its cursor
there, and the recheck before `click` no longer sees the template. The
symptom is a "not found" error for something plainly on screen, usually right
after a hover or a click on the same spot. Disable rechecking for the script
and re-enable it per target where the screen really may move:

```rhai
set_recheck(false);
let ok = win.target("ok.png");
ok.hover();
ok.click();                                   // reuses the hover hit
win.target("ocr:Next").recheck(true).click(); // this one searches again
```

`resolve` always searches regardless of the setting.

**Pointer speed.** Such applications read the pointer position at their own
frame rate. A pointer moved faster than they sample it can be read late, and
the click lands where the cursor was drawn, not where it was moved. Tune
`set_move_speed` while watching the application; ordinary Windows applications
accept `0` (immediate).

**Injected input may be refused.** Anti-cheat and some kiosk software reject
synthetic mouse and keyboard input entirely, with no error reported to the
script. Verify with a short script that the application accepts injected input
before automating it.

## Defaults and settings

Defaults are similarity `0.7`, timeout `3000`, scan interval `333`, stability
checking enabled, action rechecking enabled, typing one character every `30` ms,
and pointer speed `1200` pixels per second. Similarity is copied when OCR and UI
targets are constructed and when an image is first cached. Timeout, stability,
recheck, typing, and movement settings are read when an operation runs unless a
target-level override applies.

Settings are write-only: there are no getter functions. A script that needs to
restore a value must remember what it set.

## Script output

Rhai's built-in `print(value)` and `debug(value)` are available. Their output
is collected by the host and shown in the IDE run log; they do not touch the
screen or the automation state.

## Global functions

### `screen() -> Region`

Return the primary display.

### `screen(index: int) -> Region`

Return a display by index.

**Constraints**

- Negative indices are treated as zero.

**Errors**

- Fails when the display is unavailable.

### `screen_count() -> int`

Return the number of displays.

Valid indices for `screen` are `0` to `screen_count() - 1`.

### `region(x: int, y: int, width: int, height: int) -> Region`

Create a rectangular search scope.

**Constraints**

- Width and height must be positive.

### `window(spec: string) -> Region`

Resolve the frontmost visible matching window immediately.

**Errors**

- Fails when no window matches or capture fails.

### `window_titles() -> array<string>`

Return visible window titles in front-to-back order.

### `window_exists(spec: string) -> bool`

Check once whether a visible window matches.

A missing window returns false; other capture failures remain errors.

### `expect_window(spec: string) -> WindowExpect`

Create a retrying window assertion.

Use this for application startup instead of polling `window` with sleeps.

### `target(locator: string) -> Target`

Create a lazy target scoped to the primary display.

### `find(locator: string) -> Match`

Resolve one match on the primary display and return it.

Shorthand for `target(locator).resolve()`: it waits like an action and fails after the effective timeout.

### `find_all(locator: string) -> array<Match>`

Resolve all matches on the primary display.

A miss returns an empty array after the effective target timeout.

### `expect(locator: string) -> Expect`

Create an assertion scoped to the primary display.

### `expect(target: Target) -> Expect`

Create an assertion from an already configured target.

Target modifiers such as `similar` and `timeout` are preserved.

### `mouse_x() -> int`

Return the current pointer x coordinate.

### `mouse_y() -> int`

Return the current pointer y coordinate.

### `mouse_move(dx: int, dy: int)`

Move relative to the pointer without searching or activating a window.

### `click()`

Left-click at the current pointer position.

### `right_click()`

Right-click at the current pointer position.

### `middle_click()`

Middle-click at the current pointer position.

### `type_text(text: string)`

Type Unicode text into the current keyboard focus.

**Constraints**

- Does not identify or activate a window.
- Line breaks and tabs are sent as Enter and Tab.

### `press(keys: string)`

Press a key or modifier combination in the current focus.

**Constraints**

- Does not identify or activate a window.
- Use Unicode text input for layout-independent text.

### `scroll(horizontal: int, vertical: int)`

Scroll at the current pointer position.

Positive values move right and up; negative values move left and down.

### `sleep(ms: int)`

Pause for an interruptible duration.

Negative values become zero and paused time does not consume the duration. Prefer an assertion when waiting for visible state.

### `clipboard() -> string`

Return the clipboard text.

An empty or non-text clipboard returns an empty string.

### `set_clipboard(text: string)`

Replace the clipboard contents with text.

### `set_similarity(value: float)`

Set the default match threshold for subsequently constructed needles.

Cached image patterns retain their previous threshold. Use `Target.similar` when the threshold must be explicit.

### `set_timeout(ms: int)`

Set the default action and assertion timeout.

Targets without a `timeout` override read this value when an operation runs, including targets created earlier. Negative values become zero.

### `set_auto_wait(enabled: bool)`

Enable or disable the stability check before actions.

This does not disable waiting for a target to appear.

### `set_recheck(enabled: bool)`

Control whether actions search again immediately before acting.

When disabled, a click immediately after `hover` may reuse that hit. Consumed hits are discarded. A target override takes precedence. Disable it for applications that draw their own cursor (games, remote-desktop viewers): after `hover` the drawn cursor covers the target, and the recheck before `click` then fails to find it. See the guide on software cursors.

### `set_type_chunk(characters: int)`

Set the number of Unicode characters sent per input call.

Zero sends each text segment in one call. Negative values become zero.

### `set_type_interval(ms: int)`

Set the delay before every text input chunk.

Negative values become zero. Reducing this delay may cause some applications to drop input.

### `set_click_hold(ms: int)`

Set how long a mouse button stays pressed during a click.

Applies to clicks, double clicks and drags. Defaults to 20. At 0, some applications drop the click; raise it for applications that require a longer press. Negative values become zero.

### `set_double_click_interval(ms: int)`

Set the interval between the first and second click of a double click.

Defaults to 60. It must stay shorter than the OS double-click time (500ms by default) or the clicks are treated as two singles. Negative values become zero.

### `set_move_settle(ms: int)`

Set the wait between moving the pointer and pressing a button.

Defaults to 30. Gives applications time to react to the pointer arriving, such as hover states. Negative values become zero.

### `set_move_speed(pixels_per_second: int)`

Set pointer movement speed using an integer.

Zero moves immediately. Negative values become zero. Ordinary Windows applications accept `0`. Applications that draw their own cursor sample the pointer at their frame rate; if a fast move is read late and the click lands off target, lower the speed. See the guide on software cursors.

### `set_move_speed(pixels_per_second: float)`

Set pointer movement speed using a float.

Zero moves immediately. Negative values become zero.

## Region

A rectangular search scope.

A region returned by `window` or `WindowExpect.to_appear` retains a window handle for activation and scoped keyboard input. Geometry methods return plain regions and discard that handle.

### `x: int`

Return the rectangle's left coordinate.

### `y: int`

Return the rectangle's top coordinate.

### `width: int`

Return the rectangle width.

### `height: int`

Return the rectangle height.

### `target(locator: string) -> Target`

Create a lazy target scoped to this region.

### `find(locator: string) -> Match`

Resolve one match in this region and return it.

Shorthand for `target(locator).resolve()` on this region: it waits like an action and fails after the effective timeout.

### `find_all(locator: string) -> array<Match>`

Resolve all matches in this region.

A miss returns an empty array after the effective target timeout.

### `expect(locator: string) -> Expect`

Create an assertion scoped to this region.

### `activate()`

Bring the associated window to the foreground.

**Constraints**

- Only works on a region returned directly by a window lookup.
- Does not restore a minimized window.

### `press(keys: string)`

Activate the associated window and press a key combination.

**Constraints**

- Only works on a region returned directly by a window lookup.

### `type_text(text: string)`

Activate the associated window and type into its current focus.

The method does not click before typing.

**Constraints**

- Only works on a region returned directly by a window lookup.

### `read_text() -> array<string>`

Read the visible text in this region via OCR.

Returns one string per recognized line, in reading order. To click something that was read, hand the text back as an `ocr:` target instead of aiming at line coordinates. An area below the OCR minimum yields an empty array.

### `list_ui() -> array<string>`

List accessibility elements as ready-made `ui:` locator strings.

Each entry combines the element's non-empty name, automation id, and control type, escaped for direct use with `target(...)`. Needs no screen capture, so it works where pixel-based search does not.

### `list_ui(control_type: string) -> array<string>`

List accessibility elements of one control type as `ui:` locator strings.

**Errors**

- An unknown control type is an error rather than an empty list.

### `read_value(locator: string) -> string`

Read the value of exactly one accessibility element.

The counterpart of typing: reads an edit box or similar control via UI Automation, without OCR.

**Errors**

- An ambiguous or unconstrained locator is an error.
- A password field is an error; its value is never returned.
- An element with no readable value is an error.

### `save(path: string)`

Capture this region and save it as a PNG.

For evidence and debugging. A relative path resolves against the process working directory.

### `grow(pixels: int) -> Region`

Expand every side, or shrink for a negative value.

### `offset(dx: int, dy: int) -> Region`

Translate the rectangle.

### `above(height: int) -> Region`

Return an adjacent rectangle above this region.

**Constraints**

- Negative dimensions become zero.

### `below(height: int) -> Region`

Return an adjacent rectangle below this region.

**Constraints**

- Negative dimensions become zero.

### `left(width: int) -> Region`

Return an adjacent rectangle left of this region.

**Constraints**

- Negative dimensions become zero.

### `right(width: int) -> Region`

Return an adjacent rectangle right of this region.

**Constraints**

- Negative dimensions become zero.

### `to_string() -> string`

Return a diagnostic representation.

### `to_debug() -> string`

Return a diagnostic representation for Rhai debugging.

## Target

An immutable, lazy description of a search and action point.

Builder methods return configured copies. Search modifiers require image, OCR, or UI matching; geometric locators reject pattern-only operations.

### `similar(value: float) -> Target`

Override the primary needle's match threshold.

**Constraints**

- Requires an image, OCR, or UI locator.

### `offset(dx: int, dy: int) -> Target`

Offset the action point from the matched rectangle's center.

**Constraints**

- Requires an image, OCR, or UI locator.

### `timeout(ms: int) -> Target`

Override this target's wait timeout.

**Constraints**

- Negative values become zero.
- Requires an image, OCR, or UI locator.

### `force() -> Target`

Skip stability checking while still waiting for appearance.

**Constraints**

- Requires an image, OCR, or UI locator.

### `recheck(enabled: bool) -> Target`

Override action rechecking for this target.

Takes precedence over `set_recheck`. Use `recheck(true)` to keep the pre-action search for one target while the script runs with `set_recheck(false)` for an application that draws its own cursor (see the guide on software cursors).

### `in_region(region: Region) -> Target`

Replace the target's search scope.

**Constraints**

- Requires an image, OCR, or UI locator.

### `or(locator: string) -> Target`

Try an alternative needle when the primary needle is absent.

The alternative uses the same scope and must be an image, OCR, or UI locator. Alternatives are tried in declaration order.

### `nth(index: int) -> Target`

Select a zero-based match in reading order.

**Constraints**

- Negative indices become zero.
- Requires an image, OCR, or UI locator.

### `first() -> Target`

Select the first match in reading order.

Sugar for `nth(0)`; the behavior is identical.

**Constraints**

- Requires an image, OCR, or UI locator.

### `last() -> Target`

Select the last match in reading order.

**Constraints**

- Requires an image, OCR, or UI locator.

### `best() -> Target`

Select the highest-scoring match.

**Constraints**

- Requires an image, OCR, or UI locator.

### `right_of(anchor: string, distance: int) -> Target`

Restrict matches to the right of an image or OCR anchor.

Candidates must overlap the anchor vertically and remain within the maximum edge-to-edge distance.

### `left_of(anchor: string, distance: int) -> Target`

Restrict matches to the left of an image or OCR anchor.

Candidates must overlap the anchor vertically and remain within the maximum edge-to-edge distance.

### `above(anchor: string, distance: int) -> Target`

Restrict matches to above an image or OCR anchor.

Candidates must overlap the anchor horizontally and remain within the maximum edge-to-edge distance.

### `below(anchor: string, distance: int) -> Target`

Restrict matches to below an image or OCR anchor.

Candidates must overlap the anchor horizontally and remain within the maximum edge-to-edge distance.

### `near(anchor: string, distance: int) -> Target`

Restrict matches to a center-distance from an image or OCR anchor.

**Constraints**

- Negative distances become zero.
- UI locators cannot be anchors.

### `click() -> Match`

Resolve, wait, move, and left-click the action point.

### `right_click() -> Match`

Resolve, wait, move, and right-click the action point.

### `double_click() -> Match`

Resolve, wait, move, and double-click the action point.

### `middle_click() -> Match`

Resolve, wait, move, and middle-click the action point.

### `hover() -> Match`

Resolve, wait, and move to the action point without clicking.

### `type_text(text: string) -> Match`

Resolve, click, and type Unicode text.

### `press(keys: string) -> Match`

Resolve, move to the target, and press a key combination.

### `scroll(horizontal: int, vertical: int) -> Match`

Resolve, move to the target, and scroll.

### `drag_to(destination: Target)`

Resolve both endpoints, then drag this target to the destination.

Both searches complete before the mouse button is pressed.

### `resolve() -> Match`

Search now and return a coordinate snapshot.

This always searches again regardless of the recheck setting.

### `exists() -> bool`

Check once without waiting.

Geometric locators always return true. Absence returns false; other failures remain errors.

### `wait_vanish(ms: int) -> bool`

Wait for the target to disappear, returning false on timeout.

A non-positive duration uses the target or global default.

### `highlight(ms: int) -> Match`

Resolve and display the matched rectangle for a duration.

The call blocks while highlighted. Negative durations become zero.

### `expect() -> Expect`

Create an assertion preserving this target's modifiers.

### `to_string() -> string`

Return a diagnostic representation.

### `to_debug() -> string`

Return a diagnostic representation for Rhai debugging.

## Match

A resolved coordinate snapshot.

Actions on a Match use stored coordinates without rechecking. Prefer Target actions when the screen may have changed.

### `x: int`

Return the stored rectangle's left coordinate.

### `y: int`

Return the stored rectangle's top coordinate.

### `width: int`

Return the stored rectangle width.

### `height: int`

Return the stored rectangle height.

### `center_x: int`

Return the stored rectangle's center x coordinate.

### `center_y: int`

Return the stored rectangle's center y coordinate.

### `score: float`

Return the similarity score.

Direct geometric locators produce 1.0.

### `click()`

Left-click the stored action point without searching again.

### `right_click()`

Right-click the stored action point without searching again.

### `double_click()`

Double-click the stored action point without searching again.

### `middle_click()`

Middle-click the stored action point without searching again.

### `hover()`

Move to the stored action point without searching again.

### `highlight(ms: int)`

Display the stored rectangle for a duration.

### `region() -> Region`

Convert the stored rectangle into a plain search scope.

### `to_string() -> string`

Return a diagnostic representation.

### `to_debug() -> string`

Return a diagnostic representation for Rhai debugging.

## Expect

A retrying assertion for a Target.

A failed condition is a runtime error. Non-positive timeout arguments select the target or global default.

### `to_appear(ms: int) -> Match`

Require at least one match and return it.

**Errors**

- A timeout is a runtime error.

### `to_vanish(ms: int)`

Require all matches to disappear.

**Errors**

- A timeout is a runtime error.

### `to_have_count(count: int, ms: int) -> array<Match>`

Require exactly a count of matches and return them.

**Constraints**

- Negative counts become zero.

**Errors**

- A timeout is a runtime error.

### `to_have_count_at_least(count: int, ms: int) -> array<Match>`

Require at least a count of matches and return all current matches.

**Constraints**

- Negative counts become zero.

**Errors**

- A timeout is a runtime error.

### `to_have_count_at_most(count: int, ms: int) -> array<Match>`

Require at most a count of matches and return all current matches.

**Constraints**

- Negative counts become zero.

**Errors**

- A timeout is a runtime error.

## WindowExpect

A retrying assertion for a window query.

It retries at the global scan interval. Capture errors other than a missing window fail immediately.

### `to_appear(ms: int) -> Region`

Wait for the window and return a window-backed region.

**Errors**

- A timeout is a runtime error.

### `to_vanish(ms: int)`

Wait until no window matches.

**Errors**

- A timeout is a runtime error.

