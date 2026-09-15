# Licensing

Mekiki is distributed under the Apache License, Version 2.0. The complete
license text is in the repository root [`LICENSE`](../LICENSE), and the
attribution notices that must accompany redistributions are in
[`NOTICE`](../NOTICE).

Commercial use, modification, and redistribution are permitted subject to the
terms of the Apache License 2.0. In particular, redistributors must provide a
copy of the license, preserve applicable notices, carry forward the contents of
`NOTICE` in a readable form, and mark modified files as required by Section 4
of the license. `NOTICE` is informational and does not add terms to the
license.

## MIT-licensed matching crate

`crates/matching` began as a vendored fork of
[`template-matching`](https://github.com/urholaukkarinen/template-matching).
That crate remains available under the MIT License rather than the repository's
Apache-2.0 default. Its package manifest declares `MIT`, and the upstream
license and copyright notice are preserved verbatim in
`crates/matching/LICENSE-THIRD-PARTY`.

The fork has since been updated for current wgpu releases and extended with
zero-mean Dice similarity, CPU reference matching, non-maximum suppression,
and correctness fixes. These changes do not remove the upstream attribution or
license obligations.

## Dependencies and distributions

Dependencies and incorporated third-party material retain their own licenses.
The maintained inventory is in
[`docs/third-party-software.md`](third-party-software.md). A release must also
include a release-specific third-party notices bundle generated from the exact
dependency graph and assets shipped in that release; the inventory is not a
substitute for that bundle.

Official Mekiki source and binary release packages include the Apache-2.0
license and a readable copy of `NOTICE`. Redistributors may carry the `NOTICE`
attributions forward using any of the locations permitted by Apache-2.0
Section 4(d). A distribution that contains `crates/matching`, whether as source
or object code, must also preserve its MIT copyright and license notice.

## Contributions

Unless explicitly stated otherwise, contributions intentionally submitted for
inclusion in Mekiki are accepted under Apache License 2.0 Section 5 without
additional terms. Contributors must have the right to submit their work and
must not remove third-party notices from material they modify.

This document summarizes the repository's licensing layout for maintainers. It
does not replace the license texts and is not legal advice.
