# Windows Capture and UI Automation

This document collects Windows-specific failures that are easy to reintroduce
when changing capture, window scoping, or UI Automation.

## Desktop Duplication fallback

DXGI devices must be created on the adapter that owns each output. On hybrid-GPU
systems, creating one device on the default adapter can return
`DXGI_ERROR_UNSUPPORTED` even though another adapter drives the display.

An output that cannot establish duplication falls back to GDI. Repeated
`DXGI_ERROR_ACCESS_LOST` recovery may also switch that output to GDI. The
fallback is slower and does not include the hardware cursor, but it is preferable
to making observation unavailable. `MEKIKI_CAPTURE=gdi` selects GDI from the
start for known-incompatible environments.

`LastPresentTime == 0` represents a pointer-only update and must not replace a
valid desktop frame with black or empty content. A completely static desktop may
produce no new duplication frame; capture then uses the documented static-screen
GDI path rather than waiting forever.

Diagnostics must identify the backend per output. A mixed DXGI/GDI desktop is
otherwise indistinguishable from a fully accelerated capture by its final image.

## Coordinates and DPI

Capture, Win32 windows, UIA bounds, locators, and injected input use physical
coordinates in the Windows virtual desktop. Process DPI awareness must be set
before creating UI or capture resources. Multi-monitor layouts may include
negative coordinates.

Do not infer coordinate compatibility from equal window sizes. Keep explicit
tests that compare UIA rectangles with captured pixels on scaled displays.

## UIA scope and identity

A window-scoped UIA query starts from the selected HWND through
`ElementFromHandle`; filtering only by a rectangular overlap can include controls
from another application placed above the window.

Provider RuntimeId is the primary element identity for deduplication. Some
legacy providers omit it, so the implementation retains a documented composite
fallback key. Removing either path can recreate duplicate controls.

UIA providers vary substantially. Some expose only a partial tree, reveal
descendants intermittently, or do not support ValuePattern/TextPattern. These
are absence or capability results, not proof that the control is empty.

UI-only locators do not require capture. A fallback chain containing an image
locator does require the pixel path because silently skipping the fallback would
change script behavior.

## Wait behavior

Event-driven waiting reduces repeated capture and matching, but an event can
arrive between the initial check and subscription. The implementation must
recheck after subscription or otherwise close that race. Change detection is an
optimization only; it cannot skip a required first search or suppress timeout
and interruption checks.

## Regression checklist

- Hybrid-GPU adapter ownership and forced-GDI paths.
- Static screens, pointer-only frames, and duplication access loss.
- Negative-origin and mixed-DPI monitor layouts.
- UIA HWND subtree isolation with an overlapping foreign window.
- RuntimeId and no-RuntimeId deduplication.
- UI-only operation when capture initialization fails.
- Event arrival at the wait subscription boundary.

