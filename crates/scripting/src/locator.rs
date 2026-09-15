//! Parsing of locator strings.
//!
//! The implementation of the [Rhai locator contract](../../../docs/architecture/rhai-api.md#locators-and-scope).
//!
//! A single string expresses several targeting methods. The form is taken from
//! RPA Framework's `RPA.Desktop` and suits a dynamically typed language like
//! Rhai. One `find("image:ok.png")` reads better than a row of overloads —
//! `find(image)`, `find(region)`, `find(point)` — and adding `ocr:` later does
//! not add a function.

use std::fmt;

/// The parse result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Locator {
    /// Template match. The value references an image (a relative path or
    /// `sha256:...`).
    Image(String),
    /// Absolute coordinates, in virtual desktop space.
    Point(i32, i32),
    /// Relative to the current mouse position.
    Offset(i32, i32),
    /// A rectangle.
    Region(i32, i32, u32, u32),
    /// A window, addressable by executable name or class as well as by title.
    Window(mekiki_core::WindowQuery),
    /// Text on screen, via OCR.
    Ocr(String),
    /// A UI element found through the accessibility API.
    Ui(UiQuery),
}

/// The contents of a `ui:` locator.
///
/// There are two forms.
///
/// - `ui:Save` — the short form, giving only the displayed name
/// - `ui:name=Save,type=button` — a list of conditions
///
/// The short form exists because in practice you almost always only want the
/// name. The rule is that with no `=` anywhere, the whole thing is the name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiQuery {
    pub name: Option<String>,
    pub automation_id: Option<String>,
    /// The control type name, already verified to parse via
    /// `mekiki_core::ControlType::parse`.
    pub control_type: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LocatorError {
    Empty,
    /// A known prefix, but the arguments are malformed.
    BadArguments {
        prefix: &'static str,
        expected: &'static str,
        got: String,
    },
    /// Reserved but not implemented.
    NotImplemented(&'static str),
}

impl fmt::Display for LocatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "the locator is empty"),
            Self::BadArguments {
                prefix,
                expected,
                got,
            } => write!(
                f,
                "'{prefix}' is malformed. Expected {expected}, got '{got}'"
            ),
            Self::NotImplemented(what) => write!(
                f,
                "{what} is reserved but not yet implemented (Phase 4-1 of the development plan)"
            ),
        }
    }
}

impl std::error::Error for LocatorError {}

/// Interpret a locator string.
///
/// With no prefix it is taken as `image:`, so that the most common form can be
/// written briefly.
pub fn parse(input: &str) -> Result<Locator, LocatorError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(LocatorError::Empty);
    }

    let Some((prefix, rest)) = split_prefix(s) else {
        return Ok(Locator::Image(s.to_string()));
    };
    let rest = rest.trim();

    match prefix {
        "image" => {
            if rest.is_empty() {
                return Err(bad("image", "image:<path|sha256:...>", rest));
            }
            Ok(Locator::Image(rest.to_string()))
        }
        "point" => {
            let (x, y) = two_ints(rest).ok_or_else(|| bad("point", "point:<x>,<y>", rest))?;
            Ok(Locator::Point(x, y))
        }
        "offset" => {
            let (dx, dy) = two_ints(rest).ok_or_else(|| bad("offset", "offset:<dx>,<dy>", rest))?;
            Ok(Locator::Offset(dx, dy))
        }
        "region" => {
            let parts: Vec<&str> = rest.split(',').map(str::trim).collect();
            if parts.len() != 4 {
                return Err(bad("region", "region:<x>,<y>,<w>,<h>", rest));
            }
            let x = parts[0]
                .parse::<i32>()
                .map_err(|_| bad("region", "region:<x>,<y>,<w>,<h>", rest))?;
            let y = parts[1]
                .parse::<i32>()
                .map_err(|_| bad("region", "region:<x>,<y>,<w>,<h>", rest))?;
            let w = parts[2]
                .parse::<u32>()
                .map_err(|_| bad("region", "region:<x>,<y>,<w>,<h>", rest))?;
            let h = parts[3]
                .parse::<u32>()
                .map_err(|_| bad("region", "region:<x>,<y>,<w>,<h>", rest))?;
            if w == 0 || h == 0 {
                return Err(bad("region", "width and height must be at least 1", rest));
            }
            Ok(Locator::Region(x, y, w, h))
        }
        "window" => parse_window_spec(rest).map(Locator::Window),
        "ocr" => {
            if rest.is_empty() {
                return Err(bad("ocr", "ocr:<text>", rest));
            }
            Ok(Locator::Ocr(rest.to_string()))
        }
        "ui" => parse_ui(rest).map(Locator::Ui),
        _ => {
            // An unknown prefix is not treated as image:. So that a path like
            // "C:\path\to\ok.png" is not split by mistake,
            // split_prefix only treats lowercase ASCII as a prefix.
            Err(LocatorError::BadArguments {
                prefix: "?",
                expected: "one of image: / point: / offset: / region: / window: / ocr: / ui:",
                got: s.to_string(),
            })
        }
    }
}

/// Interpret the contents of a `window:` locator.
///
/// The form matches `ui:`. With no `=` anywhere, the whole thing is the title.
///
/// ```text
/// window:Notepad                        title substring (the original form)
/// window:exe=notepad.exe                executable
/// window:exe=notepad.exe,title=*Minutes* AND
/// window:class=Notepad,index=1          the second window matching the conditions
/// ```
pub fn parse_window_spec(rest: &str) -> Result<mekiki_core::WindowQuery, LocatorError> {
    const EXPECTED: &str = "window:<title> or window:title=...,exe=...,class=...,pid=...,index=...";

    if rest.is_empty() {
        return Err(bad("window", EXPECTED, rest));
    }

    // With no `=`, the whole thing is the title, so `window:Notepad` keeps working.
    if !rest.contains('=') {
        return Ok(mekiki_core::WindowQuery::title_contains(rest));
    }

    let mut query = mekiki_core::WindowQuery::default();
    for part in split_window_parts(rest) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((key, value)) = part.split_once('=') else {
            return Err(bad("window", EXPECTED, part));
        };
        let value = value.trim().to_string();
        if value.is_empty() {
            return Err(bad("window", "the value is empty", part));
        }
        match key.trim() {
            "title" => query.title = Some(value),
            "title_exact" => query.title_exact = Some(value),
            "exe" | "executable" => query.exe = Some(value),
            "class" | "class_name" => query.class_name = Some(value),
            "pid" => {
                query.pid =
                    Some(value.parse::<u32>().map_err(|_| {
                        bad("window", "pid must be a non-negative integer", &value)
                    })?);
            }
            "index" => {
                query.index = value
                    .parse::<usize>()
                    .map_err(|_| bad("window", "index must be a non-negative integer", &value))?;
            }
            other => {
                return Err(LocatorError::BadArguments {
                    prefix: "window",
                    expected: "title= / title_exact= / exe= / class= / pid= / index=",
                    got: other.to_string(),
                });
            }
        }
    }

    if query.is_empty() {
        // index alone does not narrow anything down; it would take the n-th of
        // every window.
        return Err(bad("window", EXPECTED, rest));
    }
    Ok(query)
}

/// Split a structured window spec without losing delimiters inside values.
///
/// `\,` represents a comma and `\\` represents a backslash. Other backslash
/// sequences stay unchanged so existing titles such as `C:\work` keep working.
fn split_window_parts(spec: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = spec.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek().copied() {
                Some(',' | '\\') => current.push(chars.next().unwrap()),
                _ => current.push(ch),
            }
        } else if ch == ',' {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    parts.push(current);
    parts
}

/// Interpret the contents of a `ui:` locator.
fn parse_ui(rest: &str) -> Result<UiQuery, LocatorError> {
    const EXPECTED: &str = "ui:<displayed name> or ui:name=...,id=...,type=...";

    if rest.is_empty() {
        return Err(bad("ui", EXPECTED, rest));
    }

    // With no `=`, the whole thing is the displayed name, so `ui:Save` works.
    if !rest.contains('=') {
        return Ok(UiQuery {
            name: Some(rest.to_string()),
            ..Default::default()
        });
    }

    let mut query = UiQuery::default();
    for part in rest.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((key, value)) = part.split_once('=') else {
            return Err(bad("ui", EXPECTED, part));
        };
        let value = value.trim().to_string();
        if value.is_empty() {
            return Err(bad("ui", "the value is empty", part));
        }
        match key.trim() {
            "name" => query.name = Some(value),
            "id" | "automationid" | "automation_id" => query.automation_id = Some(value),
            "type" | "control_type" => {
                // Validate the control type here. Deferring it to run time turns
                // a typo into an unrelated "not found" failure.
                if mekiki_core::ControlType::parse(&value).is_none() {
                    return Err(LocatorError::BadArguments {
                        prefix: "ui",
                        expected: mekiki_core::ControlType::NAMES,
                        got: value,
                    });
                }
                query.control_type = Some(value);
            }
            other => {
                return Err(LocatorError::BadArguments {
                    prefix: "ui",
                    expected: "name= / id= / type=",
                    got: other.to_string(),
                });
            }
        }
    }

    if query == UiQuery::default() {
        return Err(bad("ui", EXPECTED, rest));
    }
    Ok(query)
}

/// Split off a leading `word:`.
///
/// So that a Windows path (`C:\...`) is not mistaken for a prefix, only **two
/// or more lowercase ASCII letters** count. A drive letter is a single
/// character, so this rule excludes it naturally.
fn split_prefix(s: &str) -> Option<(&str, &str)> {
    let colon = s.find(':')?;
    let head = &s[..colon];
    if head.len() < 2 || !head.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    Some((head, &s[colon + 1..]))
}

fn two_ints(s: &str) -> Option<(i32, i32)> {
    let (a, b) = s.split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn bad(prefix: &'static str, expected: &'static str, got: &str) -> LocatorError {
    LocatorError::BadArguments {
        prefix,
        expected,
        got: got.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_string_is_an_image() {
        assert_eq!(parse("ok.png"), Ok(Locator::Image("ok.png".into())));
        assert_eq!(
            parse("images/ok_button.png"),
            Ok(Locator::Image("images/ok_button.png".into()))
        );
    }

    #[test]
    fn explicit_image_prefix() {
        assert_eq!(parse("image:ok.png"), Ok(Locator::Image("ok.png".into())));
        assert_eq!(parse("image: ok.png "), Ok(Locator::Image("ok.png".into())));
    }

    /// A Windows absolute path must not be mistaken for a prefix. Rejecting
    /// `C:` as an unknown prefix would break the natural way of writing it.
    #[test]
    fn windows_path_is_not_mistaken_for_a_prefix() {
        assert_eq!(
            parse(r"C:\images\ok.png"),
            Ok(Locator::Image(r"C:\images\ok.png".into()))
        );
        assert_eq!(
            parse(r"D:/project/ok.png"),
            Ok(Locator::Image(r"D:/project/ok.png".into()))
        );
    }

    #[test]
    fn content_addressed_image() {
        let hash = "sha256:0123456789abcdef";
        assert_eq!(
            parse(&format!("image:{hash}")),
            Ok(Locator::Image(hash.into()))
        );
    }

    #[test]
    fn point_and_offset() {
        assert_eq!(parse("point:100,200"), Ok(Locator::Point(100, 200)));
        assert_eq!(parse("point: -10 , 20 "), Ok(Locator::Point(-10, 20)));
        assert_eq!(parse("offset:5,-5"), Ok(Locator::Offset(5, -5)));
        assert!(parse("point:100").is_err());
        assert!(parse("point:a,b").is_err());
    }

    #[test]
    fn region_requires_four_numbers() {
        assert_eq!(
            parse("region:10,20,300,400"),
            Ok(Locator::Region(10, 20, 300, 400))
        );
        assert!(parse("region:10,20,300").is_err());
        assert!(
            parse("region:10,20,0,400").is_err(),
            "a width of 0 must not be accepted"
        );
    }

    #[test]
    fn window_takes_a_title() {
        assert_eq!(
            parse("window:メモ帳"),
            Ok(Locator::Window(mekiki_core::WindowQuery::title_contains(
                "メモ帳"
            )))
        );
        assert!(parse("window:").is_err());
    }

    /// A title changes with the file being edited, so a window must also be
    /// addressable by executable name and the like.
    #[test]
    fn window_key_value_form() {
        let q = match parse("window:exe=notepad.exe,title=*議事録*").unwrap() {
            Locator::Window(q) => q,
            other => panic!("{other:?}"),
        };
        assert_eq!(q.exe.as_deref(), Some("notepad.exe"));
        assert_eq!(q.title.as_deref(), Some("*議事録*"));
        assert!(q.class_name.is_none());
    }

    #[test]
    fn window_accepts_class_pid_and_index() {
        let q = match parse("window: class = Notepad , pid=1234 , index=2 ").unwrap() {
            Locator::Window(q) => q,
            other => panic!("{other:?}"),
        };
        assert_eq!(q.class_name.as_deref(), Some("Notepad"));
        assert_eq!(q.pid, Some(1234));
        assert_eq!(q.index, 2);
    }

    #[test]
    fn window_structured_values_escape_commas_and_backslashes() {
        let q = match parse(r"window:exe=editor.exe,title_exact=Report\, Q1 \\ Draft").unwrap() {
            Locator::Window(q) => q,
            other => panic!("{other:?}"),
        };
        assert_eq!(q.exe.as_deref(), Some("editor.exe"));
        assert_eq!(q.title_exact.as_deref(), Some(r"Report, Q1 \ Draft"));
    }

    #[test]
    fn window_unknown_backslash_sequences_are_preserved() {
        let q = parse_window_spec(r"title_exact=C:\work\file.txt").unwrap();
        assert_eq!(q.title_exact.as_deref(), Some(r"C:\work\file.txt"));
    }

    #[test]
    fn window_rejects_unknown_keys_and_bad_numbers() {
        assert!(parse("window:app=notepad.exe").is_err());
        assert!(parse("window:pid=abc").is_err());
        assert!(parse("window:index=-1").is_err());
        assert!(parse("window:exe=").is_err());
        // index alone does not narrow anything down.
        assert!(parse("window:index=1").is_err());
    }

    /// As long as the title contains no `=`, the original form applies.
    #[test]
    fn window_title_without_equals_stays_whole() {
        let q = match parse("window:合計, 内訳").unwrap() {
            Locator::Window(q) => q,
            other => panic!("{other:?}"),
        };
        assert_eq!(q.title.as_deref(), Some("合計, 内訳"));
    }

    #[test]
    fn ocr_takes_a_query_string() {
        assert_eq!(parse("ocr:ログイン"), Ok(Locator::Ocr("ログイン".into())));
        assert_eq!(
            parse("ocr: 名前を付けて保存 "),
            Ok(Locator::Ocr("名前を付けて保存".into()))
        );
        assert!(parse("ocr:").is_err());
    }

    /// In practice you almost always only want the name, so the short form is
    /// accepted.
    #[test]
    fn ui_bare_string_is_a_name() {
        assert_eq!(
            parse("ui:保存"),
            Ok(Locator::Ui(UiQuery {
                name: Some("保存".into()),
                ..Default::default()
            }))
        );
    }

    #[test]
    fn ui_key_value_form() {
        assert_eq!(
            parse("ui:name=保存,type=button"),
            Ok(Locator::Ui(UiQuery {
                name: Some("保存".into()),
                control_type: Some("button".into()),
                ..Default::default()
            }))
        );
        assert_eq!(
            parse("ui: id = submitButton "),
            Ok(Locator::Ui(UiQuery {
                automation_id: Some("submitButton".into()),
                ..Default::default()
            }))
        );
    }

    /// A misspelled control type is rejected before running turns it into "not
    /// found".
    #[test]
    fn ui_rejects_unknown_control_type() {
        let err = parse("ui:type=buton").unwrap_err();
        assert!(err.to_string().contains("button"), "{err}");
    }

    #[test]
    fn ui_rejects_unknown_key_and_empty() {
        assert!(parse("ui:").is_err());
        assert!(parse("ui:xpath=//div").is_err());
        assert!(parse("ui:name=").is_err());
    }

    /// As long as the name contains no `=`, the short form applies. Getting
    /// this wrong breaks names like `ui:Total = 100`.
    #[test]
    fn ui_name_with_comma_stays_whole_when_there_is_no_equals() {
        assert_eq!(
            parse("ui:はい, すべて"),
            Ok(Locator::Ui(UiQuery {
                name: Some("はい, すべて".into()),
                ..Default::default()
            }))
        );
    }

    #[test]
    fn unknown_prefix_is_rejected() {
        let err = parse("xpath://div").unwrap_err();
        assert!(err.to_string().contains("image:"), "{err}");
    }

    #[test]
    fn empty_is_rejected() {
        assert_eq!(parse(""), Err(LocatorError::Empty));
        assert_eq!(parse("   "), Err(LocatorError::Empty));
    }
}
