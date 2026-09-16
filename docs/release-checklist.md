# Mekiki Release Checklist

This is the canonical checklist for release work that is not proven merely by
the existing CI configuration.

## Automated gates

- Windows formatting, Clippy, and workspace release tests.
- Linux portability build and software-GPU shader tests.
- OpenCV golden-fixture comparison.
- Canonical Rhai API generation drift check.
- MCP protocol and catalog conformance tests.
- IDE locale key and placeholder parity tests.

The workflow definitions under `.github/workflows/` are authoritative for what
CI actually runs.

## Documentation

- The root README reaches the script guide, generated API reference, MCP
  reference, startup authorization, and security boundary.
- Public commands and examples match `--help` and current defaults.
- No hand-maintained document duplicates the generated Rhai API catalog.
- Legacy compatibility pages are removed after root links are migrated.
- The Japanese IDE guide is finalized by a human before its English translation
  is generated or updated.
- All other release-facing documentation except the Japanese script and IDE
  guide masters is English.

## IDE and installer

- Build the packaged IDE and verify it does not request the development server.
- Verify open, save, run, stop, pause, API info, completion, and language
  switching on a clean Windows system.
- Confirm the toolbar remains usable at the minimum supported window width.
- Validate installer install, update, repair, and uninstall behavior.
- Verify signing identity and expected SmartScreen behavior.

## CLI and MCP distribution

- Verify each distributed binary is self-contained as documented.
- Check `--help`, version output, stderr logging, and clean JSON-RPC stdout.
- Test the published MCP configuration example with an absolute executable path.
- Confirm startup authorization and launch allowlist documentation are bundled.
- Produce and verify SHA-256 hashes for every artifact.

## Package publication

Pushing a `v<version>` tag runs `.github/workflows/release.yml`, which builds
both installers with `scripts/build-installer.bat all` on a GitHub Windows
runner and attaches them, with `SHA256SUMS`, to a draft release. The tag must
equal the version in `ide/src-tauri/tauri.conf.json`. Publish the draft only
after the items below are checked.

- Select which crates are public.
- Verify package metadata, README, license files, repository links, and excluded
  development assets.
- Include `LICENSE` and `NOTICE` in every source and binary distribution.
- Include the MIT notice for `crates/matching` whenever that implementation is
  distributed in source or object form.
- Generate and review third-party notices from the exact release dependency
  graph and shipped assets; do not publish the working inventory as a
  substitute.
- Confirm that modified files and retained notices satisfy Apache-2.0 Section 4.
- Run `cargo publish --dry-run` for every public crate in dependency order.
- Do not treat a dry run as authorization to publish externally.

## Final release decision

- Set versions and update the changelog.
- Complete all automated and manual gates for the release candidate.
- Tag the exact tested commit.
- Publish artifacts and their hashes from the approved build.
- Record rollback steps and retain the previous supported installer.
