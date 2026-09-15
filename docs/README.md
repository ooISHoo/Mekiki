# Documentation Map

This directory contains release-facing documentation and stable maintenance
knowledge. It is not a development journal.

## User documentation

- [Script guide](script-guide.md) (translated from the Japanese master [script-guide.ja.md](script-guide.ja.md))
- [IDE user guide](ide-guide.md) (translated from the Japanese master [ide-guide.ja.md](ide-guide.ja.md))
- [Generated Rhai API reference](../api/generated/rhai-api.md)
- [MCP startup authorization](mcp-start.md)
- [MCP server reference](mcp-reference.md)
- [Release checklist](release-checklist.md)
- [Licensing policy](licensing.md)
- [Third-party software inventory](third-party-software.md)

## Architecture

- [Rhai API](architecture/rhai-api.md)
- [Matching](architecture/matching.md)
- [Window locators](architecture/window-locators.md)
- [IDE](architecture/ide.md)
- [MCP server](architecture/mcp.md)

## Maintenance

- [Windows capture and UI Automation](maintenance/windows-capture-and-uia.md)
- [Windows input reliability](maintenance/windows-input.md)
- [MCP operational lessons](maintenance/mcp-lessons.md)
- [Performance decisions](maintenance/performance-decisions.md)
- [Testing strategy](testing.md)

## Editing rules

`api/rhai-api.toml` is the only hand-maintained source for the public Rhai API
catalog. Generated API Markdown and JSON must not be edited directly.

Keep current behavior in the documents above. Do not add dated implementation
plans, agent transcripts, or round-by-round test reports to this directory.
Record durable failure causes, rejected approaches, regression tests, and
revisit criteria in the relevant architecture or maintenance document; Git
history preserves the chronology.

Files marked "Legacy Path" are temporary compatibility pages for links in the
root README and agent briefing. Do not add new content to them. Remove them
after those root links have been migrated.

Documentation in this directory is written in English except for the Japanese
masters `script-guide.ja.md` and `ide-guide.ja.md`. Each is translated to its
English counterpart (`script-guide.md`, `ide-guide.md`) only after a human has
finalized the Japanese master.
