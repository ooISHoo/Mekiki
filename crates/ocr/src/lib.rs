//! Text recognition on screen.
//!
//! Phase 4-1 of the development plan. The implementation is Windows.Media.Ocr
//! (built into the OS).
//!
//! # Why the OS engine is the first choice
//!
//! - no extra model files to ship
//! - many languages, Japanese included, work out of the box following the
//!   user's language settings
//! - no inference runtime (ONNXRuntime and friends) to carry around
//!
//! The cost is that **which languages work depends on the installed language
//! packs**. When the language you want is missing this returns
//! [`OcrError::LanguageUnavailable`], so the caller can fall back to something
//! else.
//!
//! The OS-dependent part is split the same way as in `mekiki-capture` /
//! `mekiki-input`: the [`TextRecognizer`] trait is the boundary and the
//! implementation stays inside `windows_*.rs`.

use std::fmt;

#[cfg(windows)]
mod windows_ocr;

#[cfg(test)]
mod test_fixtures;

/// One recognised word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    /// The rectangle within the recognised image (origin at its top-left).
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// One recognised line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The whole line as text. Word-separating spaces follow the OS's guess.
    pub text: String,
    pub words: Vec<Word>,
}

impl Line {
    /// The rectangle enclosing the line. `None` when it has no words.
    pub fn bounds(&self) -> Option<(i32, i32, u32, u32)> {
        let first = self.words.first()?;
        let mut left = first.x;
        let mut top = first.y;
        let mut right = first.x + first.width as i32;
        let mut bottom = first.y + first.height as i32;

        for w in &self.words[1..] {
            left = left.min(w.x);
            top = top.min(w.y);
            right = right.max(w.x + w.width as i32);
            bottom = bottom.max(w.y + w.height as i32);
        }
        Some((left, top, (right - left) as u32, (bottom - top) as u32))
    }
}

/// A recognition result.
#[derive(Clone, Debug, Default)]
pub struct Recognition {
    pub lines: Vec<Line>,
    /// The text skew the OS estimated, in degrees. Unused, but handy for diagnosis.
    pub text_angle: Option<f64>,
}

impl Recognition {
    /// The full text: the lines joined with newlines.
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn words(&self) -> impl Iterator<Item = &Word> {
        self.lines.iter().flat_map(|l| l.words.iter())
    }
}

#[derive(Debug)]
pub enum OcrError {
    Unsupported(&'static str),
    /// OCR is unavailable for the requested language (no language pack installed).
    LanguageUnavailable(String),
    /// Not a single usable OCR engine exists.
    NoEngine,
    /// The image size is outside what the OCR engine accepts.
    ImageSize {
        width: u32,
        height: u32,
    },
    Os(String),
}

impl fmt::Display for OcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(p) => write!(f, "OCR is not implemented for {p}"),
            Self::LanguageUnavailable(l) => write!(
                f,
                "OCR for '{l}' is unavailable. Adding the language pack in Windows settings enables it"
            ),
            Self::NoEngine => write!(
                f,
                "no usable OCR engine. Check the Windows language settings"
            ),
            Self::ImageSize { width, height } => write!(
                f,
                "image size cannot be passed to OCR: {width}x{height} (below 40x40, or too large)"
            ),
            Self::Os(e) => write!(f, "an OS API call failed: {e}"),
        }
    }
}

impl std::error::Error for OcrError {}

/// A text recognition backend.
///
/// Construction is not free, so reuse one.
pub trait TextRecognizer: Send {
    /// Recognise text in a BGRA8 (tightly packed) image.
    ///
    /// Coordinates use the top-left of the image you passed as the origin.
    /// Converting to screen coordinates is the caller's job.
    fn recognize_bgra(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Recognition, OcrError>;

    /// The language being used for recognition.
    fn language(&self) -> String;

    fn backend_name(&self) -> &'static str;
}

/// Open an engine following the user's language settings.
pub fn open() -> Result<Box<dyn TextRecognizer>, OcrError> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows_ocr::WindowsOcr::from_user_profile()?))
    }
    #[cfg(not(windows))]
    {
        // This would be ONNXRuntime plus something like PaddleOCR. Out of scope for now.
        Err(OcrError::Unsupported("this platform"))
    }
}

/// Open an engine for a specific language. A BCP-47 tag such as `"ja"` or `"en-US"`.
pub fn open_with_language(tag: &str) -> Result<Box<dyn TextRecognizer>, OcrError> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows_ocr::WindowsOcr::from_language(tag)?))
    }
    #[cfg(not(windows))]
    {
        let _ = tag;
        Err(OcrError::Unsupported("this platform"))
    }
}

/// The list of usable OCR languages.
pub fn available_languages() -> Result<Vec<String>, OcrError> {
    #[cfg(windows)]
    {
        windows_ocr::available_languages()
    }
    #[cfg(not(windows))]
    {
        Err(OcrError::Unsupported("this platform"))
    }
}

// ---------------------------------------------------------------------------
// string matching
// ---------------------------------------------------------------------------

/// Normalise a string for matching.
///
/// OCR output wobbles. Three systematic wobbles observed in practice are
/// absorbed here.
///
/// 1. **Spacing is not stable.** In Japanese a space can appear between every
///    character (measured: 「アーティファクト」 came back as
///    `ア - テ ィ フ ァ ク ト`).
/// 2. **Full-width vs half-width.** 「ＯＫ」 and "OK" must not be different things.
/// 3. **The prolonged sound mark confused with a hyphen.** The katakana 「ー」
///    (U+30FC) is often read as a hyphen. That happened in the example above,
///    and without fixing it an exact match drops to 0.875.
pub fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .map(fold_char)
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Fold a single character into its matching form.
fn fold_char(c: char) -> char {
    let cp = c as u32;

    // Unify the prolonged sound mark and the dashes into a hyphen, because OCR
    // cannot tell them apart reliably.
    if matches!(
        c,
        '\u{30FC}'   // KATAKANA-HIRAGANA PROLONGED SOUND MARK
            | '\u{2010}'
            ..='\u{2015}'   // hyphens and dashes
            | '\u{2212}'   // MINUS SIGN
            | '\u{FF0D}' // FULLWIDTH HYPHEN-MINUS
    ) {
        return '-';
    }

    // Fold full-width alphanumerics and symbols to half-width.
    if (0xFF01..=0xFF5E).contains(&cp) {
        return char::from_u32(cp - 0xFEE0).unwrap_or(c);
    }

    c
}

/// How well the query matches a candidate (`0.0..=1.0`).
///
/// - exact match — 1.0
/// - substring — query length / candidate length (longer candidates score lower)
/// - otherwise — a similarity derived from the edit distance
///
/// Scaled so it can be treated on the same footing as `similar()` for image
/// matching.
pub fn match_score(query: &str, candidate: &str) -> f32 {
    let q = normalize(query);
    let c = normalize(candidate);

    if q.is_empty() || c.is_empty() {
        return 0.0;
    }
    if q == c {
        return 1.0;
    }

    let q_chars: Vec<char> = q.chars().collect();
    let c_chars: Vec<char> = c.chars().collect();

    if c.contains(&q) {
        // A substring. The more extra characters, the lower the score.
        // Never reaches 1.0, so it stays distinguishable from an exact match.
        return (q_chars.len() as f32 / c_chars.len() as f32).min(0.999);
    }

    let distance = levenshtein(&q_chars, &c_chars);
    let longest = q_chars.len().max(c_chars.len());
    1.0 - (distance as f32 / longest as f32)
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }

    // One row is all that needs to be kept.
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (prev[j + 1] + 1).min(current[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut current);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::ALL;

    fn word(text: &str, x: i32, y: i32, w: u32, h: u32) -> Word {
        Word {
            text: text.into(),
            x,
            y,
            width: w,
            height: h,
        }
    }

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// The number of characters `match_score` actually compares.
    fn len(s: &str) -> usize {
        normalize(s).chars().count()
    }

    // -----------------------------------------------------------------------
    // Structure: language independent
    // -----------------------------------------------------------------------

    #[test]
    fn line_bounds_cover_all_words() {
        for f in ALL {
            let line = Line {
                text: format!("{} {}", f.short, f.label),
                words: vec![word(f.short, 10, 20, 30, 12), word(f.label, 50, 18, 40, 16)],
            };
            assert_eq!(line.bounds(), Some((10, 18, 80, 16)), "{}", f.tag);
        }
    }

    #[test]
    fn empty_line_has_no_bounds() {
        let line = Line {
            text: String::new(),
            words: vec![],
        };
        assert_eq!(line.bounds(), None);
    }

    #[test]
    fn recognition_text_joins_lines() {
        for f in ALL {
            let line = |text: &str| Line {
                text: text.into(),
                words: vec![],
            };
            let r = Recognition {
                lines: vec![line(f.label), line(f.unrelated)],
                text_angle: None,
            };
            assert_eq!(
                r.text(),
                format!("{}\n{}", f.label, f.unrelated),
                "{}",
                f.tag
            );
        }
    }

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein(&chars("kitten"), &chars("sitting")), 3);
        assert_eq!(levenshtein(&chars(""), &chars("abc")), 3);
        assert_eq!(levenshtein(&chars("abc"), &chars("abc")), 0);
    }

    #[test]
    fn empty_inputs_score_zero() {
        assert_eq!(match_score("", "abc"), 0.0);
        assert_eq!(match_score("abc", ""), 0.0);
        assert_eq!(match_score("", ""), 0.0);
    }

    // -----------------------------------------------------------------------
    // Matching: run against every language in `test_fixtures::ALL`
    // -----------------------------------------------------------------------

    /// OCR spacing is not stable. How it wobbles differs by script — a space
    /// between every character in Japanese, a word split in English — but the
    /// normalisation has to absorb all of it.
    #[test]
    fn spacing_wobble_is_absorbed() {
        for f in ALL {
            assert_eq!(
                normalize(f.label_spaced),
                normalize(f.label),
                "{}: spacing was not absorbed",
                f.tag
            );
            assert_eq!(
                match_score(f.label, f.label_spaced),
                1.0,
                "{}: spacing differences must still score as an exact match",
                f.tag
            );
        }
    }

    #[test]
    fn exact_match_scores_one() {
        for f in ALL {
            assert_eq!(match_score(f.label, f.label), 1.0, "{}", f.tag);
            assert_eq!(match_score(f.long, f.long), 1.0, "{}", f.tag);
        }
    }

    /// A short label inside a longer one scores by length ratio, so the longer
    /// the surrounding text the lower the score.
    #[test]
    fn substring_match_scores_by_length_ratio() {
        for f in ALL {
            let expected = (len(f.short) as f32 / len(f.long) as f32).min(0.999);
            let s = match_score(f.short, f.long);
            assert!(
                (s - expected).abs() < 1e-5,
                "{}: expected {expected}, got {s}",
                f.tag
            );
            // It must stay distinguishable from an exact match.
            assert!(s < 1.0, "{}", f.tag);
        }
    }

    /// The single-character mix-up OCR commonly makes costs exactly one edit,
    /// which is a high score but never 1.0.
    #[test]
    fn near_miss_uses_edit_distance() {
        for f in ALL {
            let expected = 1.0 - 1.0 / len(f.label) as f32;
            let s = match_score(f.label, f.label_typo);
            assert!(
                (s - expected).abs() < 1e-5,
                "{}: expected {expected}, got {s}",
                f.tag
            );
            assert!(s < 1.0, "{}", f.tag);
        }
    }

    #[test]
    fn unrelated_text_scores_low() {
        for f in ALL {
            assert!(
                match_score(f.label, f.unrelated) < 0.4,
                "{}: {} and {} are being confused",
                f.tag,
                f.label,
                f.unrelated
            );
            assert!(
                match_score(f.short, f.unrelated) < 0.4,
                "{}: {} and {} are being confused",
                f.tag,
                f.short,
                f.unrelated
            );
        }
    }

    /// The fixtures themselves have to hold, or the tests above pass while
    /// checking nothing. Adding a language fails here first.
    #[test]
    fn fixtures_are_well_formed() {
        assert!(!ALL.is_empty());

        for (i, f) in ALL.iter().enumerate() {
            let tag = f.tag;
            assert!(
                ALL.iter().skip(i + 1).all(|o| o.tag != tag),
                "{tag}: duplicate tag"
            );

            let fields: [(&str, &str); 6] = [
                ("label", f.label),
                ("label_spaced", f.label_spaced),
                ("label_typo", f.label_typo),
                ("short", f.short),
                ("long", f.long),
                ("unrelated", f.unrelated),
            ];
            for (name, value) in fields {
                assert!(!normalize(value).is_empty(), "{tag}: {name} is empty");
            }

            assert_ne!(
                f.label_spaced, f.label,
                "{tag}: label_spaced must differ from label, or the test is vacuous"
            );
            assert_eq!(
                normalize(f.label_spaced),
                normalize(f.label),
                "{tag}: label_spaced must normalise to label"
            );

            assert_eq!(
                len(f.label_typo),
                len(f.label),
                "{tag}: label_typo must be a substitution, not an insertion or deletion"
            );
            assert_eq!(
                levenshtein(
                    &chars(&normalize(f.label)),
                    &chars(&normalize(f.label_typo))
                ),
                1,
                "{tag}: label_typo must differ from label by exactly one character"
            );

            assert!(
                normalize(f.long).contains(&normalize(f.short)),
                "{tag}: long must contain short"
            );
            assert!(
                len(f.long) > len(f.short),
                "{tag}: long must be longer than short"
            );

            assert_ne!(
                normalize(f.unrelated),
                normalize(f.label),
                "{tag}: unrelated must not be label"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Script-specific normalisation
    //
    // These rules exist only for particular writing systems, so they are not
    // parametrised. Putting them in the fixture table would force every new
    // language to carry fields that mean nothing for it.
    // -----------------------------------------------------------------------

    /// Case folding, and folding the full-width forms used in CJK text.
    #[test]
    fn normalize_folds_fullwidth_and_case() {
        assert_eq!(normalize("ABC"), "abc");
        assert_eq!(normalize("ＯＫ"), "ok");
        assert_eq!(normalize("Ｃａｎｃｅｌ"), "cancel");
        assert_eq!(
            match_score("OK", "ＯＫ"),
            1.0,
            "full-width and half-width must be treated as equal"
        );
    }

    /// Japanese: a case hit in practice. Windows OCR reads the prolonged sound
    /// mark as a hyphen. Without absorbing that, what should be an exact match
    /// drops to 0.875.
    #[test]
    fn normalize_unifies_prolonged_sound_mark_with_hyphen() {
        assert_eq!(
            normalize("アーティファクト"),
            normalize("ア - テ ィ フ ァ ク ト")
        );
        assert_eq!(
            match_score("アーティファクト", "ア - テ ィ フ ァ ク ト"),
            1.0
        );
        // Dashes are treated the same way.
        assert_eq!(normalize("メニュー"), normalize("メニュ—"));
        assert_eq!(normalize("A−B"), normalize("A-B"));
    }
}
