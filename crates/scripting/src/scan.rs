//! Finds the strings in a script that refer to images.
//!
//! Phase 3-3 of the development plan. The IDE uses this to decide what to
//! replace with an inline thumbnail.
//!
//! # Why not a regular expression
//!
//! Matching `"[^"]*\.png"` would also replace these:
//!
//! ```rhai
//! let asset_path = "save_icon.png";   // an assignment; replacing it makes it uneditable
//! // the same goes for "cancel_button.png" inside a comment
//! ```
//!
//! Instead this actually tokenises the Rhai source and targets **image paths in
//! call-argument position** and **explicit image literals beginning with
//! `image:` or `sha256:`**. Comments are skipped. A bare `"ok.png"` in an
//! assignment is excluded, since replacing it would make it uneditable.
//!
//! # Why Rust rather than the frontend
//!
//! Doing it in the IDE's JavaScript would make the rules hard to test and would
//! need its own implementation matching Rhai syntax (object maps written `#{ }`
//! and so on). Here, `cargo test` protects it.

/// One image literal that was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageLiteral {
    /// The start of the range including the quotes, in **UTF-16 code units**.
    ///
    /// CodeMirror addresses positions in these units, so they can be passed
    /// through without conversion. Byte positions would be wrong for any script
    /// containing non-ASCII text.
    pub from: usize,
    /// The end of that range.
    pub to: usize,
    /// The contents without the quotes. Escapes are not resolved.
    pub raw: String,
    /// The image reference with any `image:` prefix stripped.
    pub reference: String,
}

/// The extensions treated as images.
const IMAGE_EXTENSIONS: [&str; 6] = [".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp"];

/// The kind of bracket, used to decide whether a string sits in argument
/// position.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Bracket {
    /// A function or method call's argument list.
    CallArgs,
    /// Anything else: grouping parentheses, indexing, blocks, object maps.
    Other,
}

/// Return the image literals in the source, in order of appearance.
pub fn image_literals(source: &str) -> Vec<ImageLiteral> {
    let mut found = Vec::new();
    let chars: Vec<char> = source.chars().collect();

    // The bracket nesting. The last entry is the current context.
    let mut stack: Vec<Bracket> = Vec::new();
    // Whether the previous meaningful token could be a callee. A `(` right
    // after an identifier, `)` or `]` opens a call's argument list.
    let mut callee_before = false;

    let mut i = 0usize;
    // The position in UTF-16 code units; this is what CodeMirror receives.
    let mut u16_pos = 0usize;

    while i < chars.len() {
        let c = chars[i];

        // --- comments ---
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            while i < chars.len() && chars[i] != '\n' {
                u16_pos += chars[i].len_utf16();
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            // A block comment. Exits safely even if EOF arrives unterminated.
            u16_pos += chars[i].len_utf16() + chars[i + 1].len_utf16();
            i += 2;
            while i < chars.len() {
                if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    u16_pos += chars[i].len_utf16() + chars[i + 1].len_utf16();
                    i += 2;
                    break;
                }
                u16_pos += chars[i].len_utf16();
                i += 1;
            }
            callee_before = false;
            continue;
        }

        // --- strings ---
        if c == '"' || c == '`' {
            let quote = c;
            let start_u16 = u16_pos;
            let mut content = String::new();

            u16_pos += c.len_utf16();
            i += 1;

            let mut terminated = false;
            while i < chars.len() {
                let ch = chars[i];
                if ch == '\\' && i + 1 < chars.len() {
                    // Skip escapes two characters at a time. Mistaking `\"` for
                    // the terminator would throw off everything after it.
                    content.push(ch);
                    content.push(chars[i + 1]);
                    u16_pos += ch.len_utf16() + chars[i + 1].len_utf16();
                    i += 2;
                    continue;
                }
                if ch == quote {
                    u16_pos += ch.len_utf16();
                    i += 1;
                    terminated = true;
                    break;
                }
                content.push(ch);
                u16_pos += ch.len_utf16();
                i += 1;
            }

            if terminated && let Some(reference) = image_reference(&content) {
                // A bare path such as `ok.png` counts only as a call argument;
                // replacing assignments too would make them uneditable.
                // `image:` and `sha256:` state that it is an image, so they are
                // replaced wherever they appear, including the `"image:..."`
                // produced by tile insertion.
                let in_call = stack.last() == Some(&Bracket::CallArgs);
                if in_call || is_explicit_image_locator(&content) {
                    found.push(ImageLiteral {
                        from: start_u16,
                        to: u16_pos,
                        raw: content,
                        reference,
                    });
                }
            }

            // A string cannot be a callee; Rhai has no calling of strings.
            callee_before = false;
            continue;
        }

        // --- character literals ---
        if c == '\'' {
            u16_pos += c.len_utf16();
            i += 1;
            while i < chars.len() {
                let ch = chars[i];
                if ch == '\\' && i + 1 < chars.len() {
                    u16_pos += ch.len_utf16() + chars[i + 1].len_utf16();
                    i += 2;
                    continue;
                }
                u16_pos += ch.len_utf16();
                i += 1;
                if ch == '\'' {
                    break;
                }
            }
            callee_before = false;
            continue;
        }

        // --- brackets ---
        match c {
            '(' => {
                stack.push(if callee_before {
                    Bracket::CallArgs
                } else {
                    Bracket::Other
                });
                callee_before = false;
            }
            '[' | '{' => {
                stack.push(Bracket::Other);
                callee_before = false;
            }
            ')' | ']' => {
                stack.pop();
                // A closing bracket can also be followed by a call, to cover
                // forms like `f(x)(y)` and `a[0](y)`.
                callee_before = true;
            }
            '}' => {
                stack.pop();
                callee_before = false;
            }
            _ => {
                // After an identifier character, the next `(` opens a call.
                callee_before = c.is_alphanumeric() || c == '_';
            }
        }

        u16_pos += c.len_utf16();
        i += 1;
    }

    found
}

/// Whether this is an explicit image locator beginning with `image:` or
/// `sha256:`.
fn is_explicit_image_locator(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.starts_with("image:") || trimmed.starts_with("sha256:")
}

/// If the string's contents refer to an image, return the reference with any
/// `image:` prefix stripped.
fn image_reference(content: &str) -> Option<String> {
    let trimmed = content.trim();
    // Strip the locator prefix. Anything other than `image:` (`point:` and so
    // on) is not an image.
    let reference = match trimmed.split_once(':') {
        Some(("image", rest)) => rest.trim(),
        // `sha256:...` is an image reference with the prefix omitted.
        Some(("sha256", _)) => trimmed,
        Some((prefix, _)) if is_locator_prefix(prefix) => return None,
        _ => trimmed,
    };

    if reference.is_empty() {
        return None;
    }
    if reference.starts_with("sha256:") {
        return Some(reference.to_string());
    }

    let lower = reference.to_ascii_lowercase();
    if IMAGE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
        return Some(reference.to_string());
    }
    None
}

/// Whether the word is reserved as a locator prefix.
///
/// Words that are not listed here (a Windows drive letter, say) are not
/// rejected.
fn is_locator_prefix(prefix: &str) -> bool {
    matches!(
        prefix,
        "point" | "offset" | "region" | "window" | "ocr" | "ui"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(source: &str) -> Vec<String> {
        image_literals(source)
            .into_iter()
            .map(|l| l.reference)
            .collect()
    }

    #[test]
    fn finds_image_in_call_arguments() {
        assert_eq!(refs(r#"click("ok.png");"#), vec!["ok.png"]);
        assert_eq!(refs(r#"target("image:ok.png").click();"#), vec!["ok.png"]);
        assert_eq!(
            refs(r#"win.target("images/save.png").click();"#),
            vec!["images/save.png"]
        );
    }

    /// Assignments are not replaced; replacing them would make them uneditable.
    #[test]
    fn ignores_assignments() {
        assert!(refs(r#"let path = "ok.png";"#).is_empty());
        assert!(refs(r#"let m = #{ icon: "ok.png" };"#).is_empty());
    }

    /// Tile insertion writes a standalone `"image:..."`, which is replaced even
    /// outside argument position.
    #[test]
    fn explicit_image_prefix_is_found_outside_calls() {
        assert_eq!(refs(r#""image:ok.png""#), vec!["ok.png"]);
        assert_eq!(
            refs(r#"let p = "image:parts/save.png";"#),
            vec!["parts/save.png"]
        );
        let h = "sha256:0123456789abcdef";
        assert_eq!(refs(&format!(r#""{h}""#)), vec![h]);
        assert_eq!(refs(&format!(r#""image:{h}""#)), vec![h]);
    }

    #[test]
    fn ignores_comments() {
        assert!(refs(r#"// click("ok.png");"#).is_empty());
        assert!(refs("/* click(\"ok.png\"); */").is_empty());
        assert_eq!(refs("// note: \"a.png\"\nclick(\"b.png\");"), vec!["b.png"]);
    }

    /// An unterminated block comment must not panic; it happens routinely while
    /// editing.
    #[test]
    fn unterminated_block_comment_is_safe() {
        assert!(refs("/* click(\"ok.png\");").is_empty());
    }

    #[test]
    fn ignores_non_image_strings() {
        assert!(refs(r#"type_text("こんにちは");"#).is_empty());
        assert!(refs(r#"press("ctrl+s");"#).is_empty());
        assert!(refs(r#"window("メモ帳");"#).is_empty());
    }

    /// Non-image locators are not replaced.
    #[test]
    fn ignores_other_locator_prefixes() {
        assert!(refs(r#"target("point:10,20").click();"#).is_empty());
        assert!(refs(r#"target("region:0,0,10,10").click();"#).is_empty());
        assert!(refs(r#"target("ocr:ログイン").click();"#).is_empty());
        assert!(refs(r#"target("ui:保存").click();"#).is_empty());
        // Ending in an image extension does not make a `ui:` locator an image.
        assert!(refs(r#"target("ui:name=logo.png").click();"#).is_empty());
    }

    #[test]
    fn handles_content_addressed_references() {
        let h = "sha256:0123456789abcdef";
        assert_eq!(refs(&format!(r#"click("{h}");"#)), vec![h]);
        assert_eq!(refs(&format!(r#"click("image:{h}");"#)), vec![h]);
    }

    #[test]
    fn finds_multiple_and_nested() {
        assert_eq!(
            refs(r#"target("a.png").right_of("b.png", 200).click();"#),
            vec!["a.png", "b.png"]
        );
        assert_eq!(
            refs(r#"click(pick("a.png", "b.png"));"#),
            vec!["a.png", "b.png"]
        );
    }

    /// Grouping parentheses are not a call.
    #[test]
    fn grouping_parens_are_not_calls() {
        assert!(refs(r#"let x = ("ok.png");"#).is_empty());
        assert!(refs(r#"if (1 + 2) { let y = "ok.png"; }"#).is_empty());
    }

    /// Positions are UTF-16 code units, which diverge from byte positions once
    /// non-ASCII text is involved.
    #[test]
    fn positions_are_utf16_code_units() {
        let src = r#"type_text("日本語"); click("ok.png");"#;
        let found = image_literals(src);
        assert_eq!(found.len(), 1);

        // Cross-check using the same counting as a JavaScript String.
        let utf16: Vec<u16> = src.encode_utf16().collect();
        let slice = String::from_utf16(&utf16[found[0].from..found[0].to]).unwrap();
        assert_eq!(slice, r#""ok.png""#);
    }

    #[test]
    fn positions_cover_the_quotes() {
        let src = r#"click("ok.png");"#;
        let found = image_literals(src);
        // `click(` is 6 units and `"ok.png"` is 8 including the quotes.
        assert_eq!((found[0].from, found[0].to), (6, 14));
        assert_eq!(&src[found[0].from..found[0].to], r#""ok.png""#);
    }

    /// An escaped quote must not be mistaken for the terminator; mistaking it
    /// throws off everything after it.
    #[test]
    fn escaped_quotes_do_not_end_the_string() {
        let src = r#"type_text("a\"b"); click("ok.png");"#;
        assert_eq!(refs(src), vec!["ok.png"]);
    }

    #[test]
    fn backtick_strings_are_handled() {
        assert_eq!(refs("click(`ok.png`);"), vec!["ok.png"]);
        // A double quote inside backticks must not break it.
        assert_eq!(refs("log(`a\"b`); click(\"ok.png\");"), vec!["ok.png"]);
    }

    #[test]
    fn method_chains_keep_argument_context() {
        let src = r#"
            let win = window("メモ帳");
            win.target("a.png")
               .below("b.png", 100)
               .type_text("文字");
        "#;
        assert_eq!(refs(src), vec!["a.png", "b.png"]);
    }

    #[test]
    fn array_indexing_is_not_a_call() {
        assert!(refs(r#"let x = images["ok.png"];"#).is_empty());
        assert_eq!(refs(r#"find_all("row.png")[0].click();"#), vec!["row.png"]);
    }

    #[test]
    fn case_insensitive_extensions() {
        assert_eq!(refs(r#"click("OK.PNG");"#), vec!["OK.PNG"]);
        assert_eq!(refs(r#"click("a.JPEG");"#), vec!["a.JPEG"]);
    }
}
