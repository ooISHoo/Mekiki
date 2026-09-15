# Third-Party Software Inventory

Status: working inventory for release preparation.
Snapshot date: 2026-08-25.

This document distinguishes code incorporated into the repository, software
linked or bundled into release artifacts, and tools used only for development.
It is not legal advice and is not yet a release-ready third-party notices file.

Mekiki-owned code is distributed under Apache License 2.0 unless a more
specific license is declared. Third-party notices and license conditions remain
in force independently of Mekiki's outbound license.

## Incorporated source

| Project | Version or revision | License | Use |
|---|---|---|---|
| [template-matching](https://github.com/urholaukkarinen/template-matching) | v0.2.0 lineage; upstream `main` as of 2023-07-23 | MIT | `crates/matching` began as a vendored fork and has substantial local changes. |

The upstream MIT text is preserved verbatim in
`crates/matching/LICENSE-THIRD-PARTY`, and the root `NOTICE` records the fork.
The crate manifest declares MIT rather than inheriting the workspace's
Apache-2.0 license. This attribution must remain in source and binary
distributions that contain the matching implementation.

## Direct Rust dependencies

These are the external packages directly resolved from workspace manifests by
the current `Cargo.lock`. Internal `mekiki-*` crates are omitted.

| Package | Locked version | Declared license | Primary use |
|---|---:|---|---|
| arboard | 3.6.1 | MIT OR Apache-2.0 | Clipboard image import |
| base64 | 0.23.1 | MIT OR Apache-2.0 | IDE image data URLs |
| bytemuck | 1.25.2 | Zlib OR Apache-2.0 OR MIT | GPU buffer conversion |
| env_logger | 0.11.11 | MIT OR Apache-2.0 | Logging initialization |
| image | 0.25.9 | MIT OR Apache-2.0 | Image decoding, encoding, and thumbnails |
| log | 0.4.33 | MIT OR Apache-2.0 | Logging facade |
| pollster | 1.0.1 | Apache-2.0 / MIT | Synchronous GPU initialization |
| rayon | 1.12.0 | MIT OR Apache-2.0 | Parallel CPU matching and capture work |
| rhai | 1.25.1 | MIT OR Apache-2.0 | Script runtime |
| rmcp | 3.1.2 | Apache-2.0 | MCP server implementation |
| rustfft | 6.4.1 | MIT OR Apache-2.0 | Fast CPU correlation |
| schemars | 1.2.2 | MIT | MCP JSON schemas |
| serde | 1.0.229 | MIT OR Apache-2.0 | Serialization |
| serde_json | 1.0.151 | MIT OR Apache-2.0 | JSON and JSON-RPC data |
| sha2 | 0.10.9 | MIT OR Apache-2.0 | Content-addressed image assets |
| tauri | 2.11.5 | Apache-2.0 OR MIT | Desktop application runtime |
| tauri-build | 2.6.3 | Apache-2.0 OR MIT | IDE build integration |
| tauri-plugin-dialog | 2.7.2 | Apache-2.0 OR MIT | Native file and message dialogs |
| tauri-plugin-global-shortcut | 2.3.2 | Apache-2.0 OR MIT | Emergency-stop shortcut |
| tokio | 1.53.1 | MIT | MCP async runtime |
| tray-icon | 0.24.2 | MIT OR Apache-2.0 | MCP notification-area icon and its menu (muda) |
| toml | 0.9.12+spec-1.1.0 | MIT OR Apache-2.0 | Canonical API catalog parsing |
| wgpu | 30.0.0 | MIT OR Apache-2.0 | GPU template matching |
| windows | 0.62.2 | MIT OR Apache-2.0 | Win32, DXGI, UIA, OCR, and input APIs |

Some names also occur at other versions as transitive dependencies. The locked
version in this table is the package reached directly from a workspace member.

## IDE production JavaScript

The following 18 packages are the non-development entries in
`ide/package-lock.json`. Vite bundles them into the IDE frontend.

| Package | Locked version | Declared license |
|---|---:|---|
| @codemirror/autocomplete | 6.20.3 | MIT |
| @codemirror/commands | 6.10.4 | MIT |
| @codemirror/lang-javascript | 6.2.5 | MIT |
| @codemirror/language | 6.12.4 | MIT |
| @codemirror/lint | 6.9.7 | MIT |
| @codemirror/state | 6.7.1 | MIT |
| @codemirror/theme-one-dark | 6.1.3 | MIT |
| @codemirror/view | 6.43.8 | MIT |
| @lezer/common | 1.5.2 | MIT |
| @lezer/highlight | 1.2.3 | MIT |
| @lezer/javascript | 1.5.4 | MIT |
| @lezer/lr | 1.4.10 | MIT |
| @marijn/find-cluster-break | 1.0.3 | MIT |
| @tauri-apps/api | 2.11.1 | Apache-2.0 OR MIT |
| @tauri-apps/plugin-dialog | 2.7.2 | MIT OR Apache-2.0 |
| crelt | 1.0.7 | MIT |
| style-mod | 4.1.3 | MIT |
| w3c-keyname | 2.2.8 | MIT |

## Transitive dependency snapshot

`cargo metadata --locked` contains 557 external package-version records across
all workspace members, targets, features, build dependencies, and development
dependencies. This is broader than the Windows release binary. No record has
missing license metadata, and no dependency declares GPL as its only available
license.

The less uniform license families in the locked Rust graph include:

| License family | Packages requiring attention |
|---|---|
| MPL-2.0 / MPL-2.0+ | cssparser, cssparser-macros, dtoa-short, option-ext, selectors, smartstring |
| Unicode-3.0 | ICU4X data and support crates, including icu_normalizer, icu_properties, yoke, zerovec, and related derives |
| BSL-1.0 | clipboard-win, error-code |
| BSD-3-Clause | alloc-no-stdlib, alloc-stdlib |
| ISC | libloading |
| CC0-1.0 | tiny-keccak |
| Zlib | foldhash, slotmap |

Other Rust records use MIT, Apache-2.0, BSD, Zlib, ISC, 0BSD, Unlicense, CC0,
or expressions offering a choice among those licenses. Two `r-efi` records
include LGPL as an alternative, but also offer MIT or Apache-2.0; they are not
LGPL-only dependencies.

`ide/package-lock.json` contains 94 package records: 78 MIT, 13
`Apache-2.0 OR MIT`, one `MIT OR Apache-2.0`, one BSD-3-Clause, and one ISC.
Only 18 are production entries; the remaining 76 are development-only in the
current lockfile.

The lockfiles are the authoritative complete package lists. This summary is for
review, not a substitute for generating notices from the exact release graph.

## Development and packaging tools

These projects are used to build, test, or package Mekiki but are not normal
runtime library dependencies.

| Project | License | Role |
|---|---|---|
| Rust toolchain and Cargo | MIT OR Apache-2.0 | Compile and test Rust code |
| Node.js | MIT | Run frontend tooling and tests |
| npm | Artistic-2.0 | Install locked frontend packages |
| Vite 6.4.3 | MIT | Bundle the IDE frontend |
| Tauri CLI 2.11.4 | Apache-2.0 OR MIT | Build the desktop application and installers |
| OpenCV | Apache-2.0 | Generate and verify matching golden data; not a runtime dependency |
| NumPy | BSD-3-Clause | Support golden-data scripts; not a runtime dependency |
| NSIS | zlib/libpng for the core, with separately licensed compression modules | Build the Windows NSIS installer |
| WiX Toolset 3.14 | Microsoft Reciprocal License | Build MSI packages; the tool itself is not shipped as an application component |

The exact NSIS module selected by the generated installer must be checked before
publishing its final notices. Tauri's cached NSIS `COPYING` also lists bzip2 and
CPL-1.0 terms for optional compression modules.

## Referenced designs, not incorporated code

- SikuliX influenced API vocabulary such as Region, target offsets, and
  similarity selection. Mekiki does not include SikuliX source or claim script
  compatibility.
- OpenCV supplies test-oracle output and benchmark comparisons. Mekiki release
  binaries do not link OpenCV.
- OculiX and rustautogui appear in historical design comparisons only.

These references need accurate prose attribution but do not by themselves make
their software licenses apply to Mekiki source.

## Platform components outside this OSS inventory

Windows, Windows Installer, Windows SDK import libraries, DirectX/DXGI, Windows
UI Automation, Windows OCR, and WebView2 are platform or redistributable
components governed by Microsoft terms. They must be reviewed separately from
the OSS dependency inventory.

## Release work still required

1. Generate a release-specific third-party notices file from the exact Windows
   IDE and MCP dependency graph, selecting one license where an `OR` expression
   permits a choice.
2. Include required copyright notices and complete license texts, especially
   MIT, Apache-2.0, MPL-2.0, Unicode-3.0, and the template-matching notice.
3. Confirm whether the chosen NSIS compression module adds a notice or source
   obligation to the installer distribution.
4. Run a policy tool in CI so new dependencies or license changes fail review
   instead of silently changing the release obligations.
5. Review fonts, icons, screenshots, test images, and other non-code assets
   separately; package managers do not report their licenses.

## Refresh commands

Run these from the repository root after changing dependencies:

```text
cargo metadata --format-version 1 --locked
cargo tree --locked --target x86_64-pc-windows-msvc \
  -e normal,build -p mekiki-ide -p mekiki-mcp
npm --prefix ide ls --all
```

Review `Cargo.lock`, `ide/package-lock.json`, `NOTICE`, and every vendored
license file in the same change.
