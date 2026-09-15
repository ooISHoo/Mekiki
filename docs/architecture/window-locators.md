# Window Locator Design

Window titles are not stable identifiers. They change with the open document,
localization, application state, and generated suffixes. Mekiki therefore uses
one locator syntax that can combine multiple Win32 properties.

## Supported properties

The `window:` form accepts a title expression and structured properties such as
the executable name, class name, and process ID. Prefer executable name for
ordinary desktop applications and title for applications hosted by a shared
process, including some packaged Windows applications.

Examples:

```text
window:exe=notepad.exe
window:title=Untitled*,class=Notepad
window:pid=1234
```

Matching is intentionally descriptive rather than based on a stored HWND. A
window handle is transient and can be reused after a process exits.

## Design rules

- All filters are combined, allowing a broad title to be narrowed safely.
- Executable names are compared without requiring an absolute installation
  path.
- Class matching supports generated suffixes used by framework windows.
- A result carries physical desktop coordinates, consistent with capture, UIA,
  and input injection.
- An ambiguous selector must be narrowed rather than silently choosing an
  arbitrary process.

The design follows the common model used by Windows automation systems: combine
caption, class, process, and executable metadata, then scope descendant searches
to the selected top-level window. Mekiki keeps this under `window:` instead of
introducing a separate selector language.

## Selection guidance

Use `exe=` when the executable uniquely identifies the application and the
title varies. Use `title=` for shared host processes or when multiple documents
from one executable must be distinguished. Add `class=` only when it is stable
for the target framework. Use `pid=` for a process that the same workflow has
just launched and can identify directly.

