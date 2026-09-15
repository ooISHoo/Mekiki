//! Per-language fixtures for the string-matching tests.
//!
//! [`match_score`](crate::match_score) has to behave the same way whatever
//! language is on screen, so the tests that exercise it are written once and run
//! against every entry in [`ALL`].
//!
//! # Adding a language
//!
//! Append one [`LangFixture`] to [`ALL`]. **Nothing else needs to change.**
//! Every parametrised test iterates the table, and `fixtures_are_well_formed`
//! checks the new entry satisfies the invariants those tests depend on — so a
//! sloppy fixture fails loudly instead of quietly weakening the suite.
//!
//! Pick the strings the way the tests read them, not by literal translation:
//!
//! - `label` / `label_spaced` — the same label, once as written and once with
//!   the spacing OCR tends to invent. In Japanese that is a space between every
//!   character; in English it is more often a word split (`Login` / `Log in`).
//! - `label_typo` — one character substituted, the mistake OCR makes most.
//!   Choose a pair that actually looks alike in that script (`g` / `q`,
//!   「ン」/「ソ」).
//! - `short` / `long` — a menu item and the longer item containing it. This is
//!   what makes the substring branch fire.
//! - `unrelated` — a label from the same UI that must never be confused with
//!   `label`.
//!
//! # What does not belong here
//!
//! Normalisation rules that only exist for one script (full-width folding, the
//! katakana prolonged sound mark) stay as their own tests in `lib.rs`. Forcing
//! them into this table would mean every new language carrying fields that mean
//! nothing for it.

/// One language's worth of test strings.
///
/// See the module documentation for how to choose them.
pub(crate) struct LangFixture {
    /// BCP-47 tag. Only used to name the language in assertion messages.
    pub tag: &'static str,
    /// A short UI label, as it is written.
    pub label: &'static str,
    /// `label` with OCR-style spacing. Must normalise to the same string as
    /// `label`, and must not already equal it.
    pub label_spaced: &'static str,
    /// `label` with exactly one character substituted.
    pub label_typo: &'static str,
    /// A short label that appears inside `long`.
    pub short: &'static str,
    /// A longer label containing `short`.
    pub long: &'static str,
    /// A label that must not be confused with `label`.
    pub unrelated: &'static str,
}

/// Every language the string-matching tests run against.
pub(crate) const ALL: &[LangFixture] = &[
    LangFixture {
        tag: "ja",
        label: "ログイン",
        label_spaced: "ロ グ イ ン",
        label_typo: "ログイソ",
        short: "保存",
        long: "名前を付けて保存",
        unrelated: "キャンセル",
    },
    LangFixture {
        tag: "en",
        label: "Login",
        label_spaced: "Log in",
        label_typo: "Loqin",
        short: "Save",
        long: "Save As",
        unrelated: "Cancel",
    },
];
