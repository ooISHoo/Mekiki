# Testing Strategy

Mekiki separates deterministic correctness tests from desktop-dependent system
tests. CI must remain useful on machines without a Windows desktop or supported
GPU, while release validation must exercise the real interaction boundaries.

## Layers

1. Pure unit tests cover parsing, selection, geometry, scoring, and error
   classification.
2. Golden fixtures compare matching output with the accepted baseline.
3. Catalog tests connect the canonical Rhai contract, runtime registration, IDE
   completion data, MCP resources, and human MCP reference.
4. Protocol tests run `mekiki-mcp` as a child process and exercise stdio,
   interruption, timeout, malformed requests, and recovery.
5. Deterministic UI fixtures exercise focus, modal dialogs, overlapping windows,
   DPI, UIA identity, and changing screen content.
6. Application tests cover representative desktop software in an isolated
   profile and verify persistent output independently of the UI.

## Test environment rules

Only one component may inject input into a desktop session at a time. Desktop
tests must confirm the lease mode and close or isolate other automation hosts.

Fixtures should target recognition variability before business workflows:
duplicate labels, controls moving during a wait, low-variance images, layered
windows, negative monitor coordinates, provider elements without RuntimeId, and
modal dialogs are more diagnostic than a long happy-path script.

Application success is not inferred from tool return values alone. Read the
resulting field, inspect a stable UI state, or parse the saved artifact. Tests
must clean only their dedicated output and profile directories.

## Release evidence

A release candidate should include:

- formatting, Clippy, and workspace release tests;
- CPU/GPU matching parity and golden fixtures;
- MCP protocol and catalog tests;
- generated Rhai API drift check;
- packaged IDE launch and core editor workflow on a clean Windows system;
- capture fallback and UIA-only operation on at least one constrained system;
- installer install, update, and uninstall verification;
- hashes for published artifacts and a documented rollback path.

