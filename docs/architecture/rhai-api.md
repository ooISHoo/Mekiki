# Rhai API Architecture

This document records the stable design decisions behind Mekiki's scripting
API. It is not an API catalog.

The canonical public contract is [`api/rhai-api.toml`](../../api/rhai-api.toml).
Edit that file when a public name, signature, description, constraint, error,
or example changes, then regenerate its consumers:

```text
cargo run -p mekiki-api-gen
cargo run -p mekiki-api-gen -- --check
```

Generated Markdown and JSON must never be edited by hand. Runtime registration
and catalog conformance tests connect the contract to the implementation.

## Execution model

`Target` is a lazy description of how to find something. Constructing one does
not capture the screen. Every action resolves the target again, which allows a
script to describe a control once and use it after the interface has moved.

`Match` is different: it is a snapshot of one resolved rectangle and score.
Keeping a `Match` across an interface transition is unsafe because its
coordinates do not track the control.

Actions normally perform three operations as one contract:

1. wait for the target to appear;
2. wait for the matched pixels to become stable;
3. perform the input operation.

`force()` deliberately disables the stability wait for controls that never
settle. It should be local to the affected target rather than a global default.

## Locators and scope

All locator forms produce a `Target`, including direct points and regions. This
keeps filtering, timeout, fallback, assertions, and diagnostics consistent.

- `image:` matches pixels and supports similarity scoring.
- `ocr:` matches visible text and returns its screen rectangle.
- `ui:` uses Windows UI Automation and can work without screen capture.
- `window:` identifies a top-level window and establishes a scope.
- `point:` and `region:` describe explicit physical screen coordinates.

Window scope is preferred because it limits ambiguity and protects operations
from similarly named controls in other applications. Raw keyboard and scroll
input requires an explicit window scope, or the literal `focused` when the
script established focus immediately beforehand.

Fallback order is part of the script's behavior. Put the cheapest and most
reliable locator first. A UI-only chain avoids capture; adding an image fallback
requires the pixel path to remain available.

## Waiting and assertions

Fixed sleeps are not the normal synchronization mechanism. Actions wait for
their targets, while `expect(...)` and `expect_window(...)` express observable
postconditions. An assertion retries until its timeout and produces the same
diagnostic context as a failed action.

Window construction remains an immediate lookup. Code that waits for startup
must use `expect_window(...).to_appear(...)` so absence is represented as a
normal retry condition rather than an exception-driven polling loop.

## Queries and assertions are separate vocabularies

A function that returns `bool` or a value never fails on absence: `exists`,
`window_exists`, `wait_vanish`, `find_all` (empty array). An `expect` method
fails loudly with diagnostics. Every waiting or checking API must land in
exactly one of these two families; a third hybrid (such as a sleep-flavored
`wait_for`) was considered and rejected as duplication.

## Naming decisions (pre-publication cleanup)

These were resolved before the first public release because published names
cannot be removed:

- **One typing verb: `type_text`** — globally, on `Region`, and on `Target`.
  The `Target.type` name and its `type_text` alias, and `Region.type_into`,
  were removed. The semantic differences (global: current focus; region:
  activate then type; target: click then type) live in the owner, not the verb.
- **`window_exact` was removed.** It duplicated
  `window("title_exact=...")` after structured window specs landed.
- **`first()` is documented sugar for `nth(0)`.**
- **`Match` actions return `unit`, deliberately.** A `Match` is a coordinate
  snapshot; encouraging action chains on one would encourage acting on stale
  coordinates. `Target` actions return the `Match` for the read-then-act
  pattern instead.
- **Settings are write-only.** There are no `get_*` functions; the `get_`
  namespace stays free until a real need appears.

## Reserved names and extension points

- The locator prefix namespace is a documented contract: two or more lowercase
  ASCII letters before `:` always parse as a prefix, unknown prefixes are
  errors, and `sha256:` is reserved. New locator kinds are therefore always
  backward compatible.
- `mouse_down`, `mouse_up`, `key_down`, and `key_up` are **reserved but not
  implemented**. Publishing them requires a design for cleanup on stop
  (`release_input`) and for modifier-held drags; do not reuse these names for
  anything else.
- Rhai overloads on arity and type are additive, so parameters (timeouts,
  scan intervals) can be added to existing functions after release without
  reservation. Only names and return types are locked by publication.

## Reading the screen

Reading is first-class, mirroring writing: `read_text` returns OCR lines (a
line is the unit a reader thinks in; to click a result, hand the text back as
an `ocr:` target), `list_ui` returns ready-made `ui:` locator strings so
discovery output can be pasted into `target(...)`, and `read_value` reads
exactly one UIA element. Ambiguity is an error, and password fields are always
redacted — silently choosing an element or disclosing a secret is worse than
failing.

## Diagnostics and assets

Failures report the required score, best observed score, scope, and elapsed
time. Pixel failures may also produce a captured screen, heatmap, annotated
image, and candidate list.

Captured templates can be addressed by content hash. A `sha256:` reference is
stable when files are moved or renamed and allows the asset store to deduplicate
identical captures. Plain relative image paths remain relative to the configured
base directory; the runtime does not search unrelated implicit directories.

## Rust boundary

The public Rhai surface is dynamic and does not map one-to-one to ordinary Rust
functions. Rustdoc explains implementation types and invariants. The TOML
contract explains the callable script API. Human guides should link to the
generated reference instead of copying signatures into another hand-maintained
table.

