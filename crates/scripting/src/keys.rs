//! Parsing of key specification strings such as `"ctrl+s"`, `"enter"` and `"f5"`.

use mekiki_core::{InputKey as Key, InputModifiers as Modifiers};

/// Split `"ctrl+shift+s"` into modifiers and the key itself.
///
/// The last element is the key; everything before it is a modifier.
pub fn parse_combo(input: &str) -> Result<(Key, Modifiers), String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("the key specification is empty".to_string());
    }

    let parts: Vec<&str> = s
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return Err(format!("cannot interpret the key specification: '{input}'"));
    }

    let (last, mods) = parts.split_last().expect("checked non-empty above");

    let mut modifiers = Modifiers::NONE;
    for m in mods {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers.ctrl = true,
            "shift" => modifiers.shift = true,
            "alt" => modifiers.alt = true,
            "win" | "meta" | "cmd" => modifiers.meta = true,
            other => return Err(format!("unknown modifier: '{other}'")),
        }
    }

    Ok((parse_key(last)?, modifiers))
}

/// Interpret a lone key name.
pub fn parse_key(name: &str) -> Result<Key, String> {
    let n = name.trim().to_ascii_lowercase();

    if let Some(num) = n.strip_prefix('f')
        && let Ok(v) = num.parse::<u8>()
        && (1..=24).contains(&v)
    {
        return Ok(Key::F(v));
    }

    // The numeric keypad. `0` is the top-row digit, so NUM0 belongs here.
    // VK_NUMPAD0 = 0x60 ... VK_NUMPAD9 = 0x69
    if let Some(rest) = n
        .strip_prefix("numpad")
        .or_else(|| n.strip_prefix("num"))
        .or_else(|| n.strip_prefix("np"))
        && rest.len() == 1
        && let Some(d) = rest.chars().next().and_then(|c| c.to_digit(10))
    {
        return Ok(Key::Raw(0x60 + d as u16));
    }

    Ok(match n.as_str() {
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "esc" | "escape" => Key::Escape,
        "backspace" | "bs" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "insert" | "ins" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "space" => Key::Space,
        "shift" => Key::Shift,
        "ctrl" | "control" => Key::Ctrl,
        "alt" => Key::Alt,
        "win" | "meta" | "cmd" => Key::Meta,
        "capslock" => Key::CapsLock,
        "printscreen" | "prtsc" => Key::PrintScreen,
        "numlock" => Key::Raw(0x90),
        "numadd" | "num+" | "npadd" => Key::Raw(0x6B),
        "numsub" | "num-" | "npsub" => Key::Raw(0x6D),
        "nummul" | "num*" | "npmul" => Key::Raw(0x6A),
        "numdiv" | "num/" | "npdiv" => Key::Raw(0x6F),
        "numdec" | "num." | "npdec" => Key::Raw(0x6E),
        "numenter" | "nument" | "npenter" | "numpadenter" => Key::NumEnter,
        // Symbols are the US-layout OEM virtual keys. Other layouts may place
        // them elsewhere.
        "minus" | "hyphen" | "-" => Key::Raw(0xBD),
        "equals" | "equal" | "=" => Key::Raw(0xBB),
        "lbracket" | "[" => Key::Raw(0xDB),
        "rbracket" | "]" => Key::Raw(0xDD),
        "semicolon" | ";" => Key::Raw(0xBA),
        "quote" | "apostrophe" | "'" => Key::Raw(0xDE),
        "comma" | "," => Key::Raw(0xBC),
        "period" | "dot" | "." => Key::Raw(0xBE),
        "slash" | "/" => Key::Raw(0xBF),
        "backslash" | "\\" => Key::Raw(0xDC),
        "grave" | "backtick" | "`" => Key::Raw(0xC0),
        _ => {
            // A single alphanumeric character maps to a virtual key code.
            //
            // **This is layout dependent.** The mapping assumes the US layout,
            // and symbols move between layouts. Use type() for text input, which
            // sends Unicode and therefore does not depend on the layout.
            let mut chars = n.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphanumeric() => {
                    Key::Raw(c.to_ascii_uppercase() as u16)
                }
                _ => return Err(format!("unknown key name: '{name}'")),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_keys() {
        assert_eq!(parse_key("enter").unwrap(), Key::Enter);
        assert_eq!(parse_key("ESC").unwrap(), Key::Escape);
        assert_eq!(parse_key("pgdn").unwrap(), Key::PageDown);
    }

    #[test]
    fn function_keys() {
        assert_eq!(parse_key("f1").unwrap(), Key::F(1));
        assert_eq!(parse_key("F12").unwrap(), Key::F(12));
        // f25 is not a function key, and it does not match the single-character
        // rule either, so it fails.
        assert!(parse_key("f25").is_err());
    }

    #[test]
    fn single_characters_map_to_virtual_keys() {
        assert_eq!(parse_key("a").unwrap(), Key::Raw(b'A' as u16));
        assert_eq!(parse_key("Z").unwrap(), Key::Raw(b'Z' as u16));
        assert_eq!(parse_key("5").unwrap(), Key::Raw(b'5' as u16));
    }

    #[test]
    fn numpad_keys_are_not_the_top_row() {
        assert_eq!(parse_key("num0").unwrap(), Key::Raw(0x60));
        assert_eq!(parse_key("NUM0").unwrap(), Key::Raw(0x60));
        assert_eq!(parse_key("numpad9").unwrap(), Key::Raw(0x69));
        assert_eq!(parse_key("np5").unwrap(), Key::Raw(0x65));
        assert_ne!(parse_key("0").unwrap(), parse_key("num0").unwrap());
        assert_eq!(parse_key("numlock").unwrap(), Key::Raw(0x90));
        assert_eq!(parse_key("numenter").unwrap(), Key::NumEnter);
        assert_eq!(parse_key("numpadenter").unwrap(), Key::NumEnter);
        assert_ne!(parse_key("enter").unwrap(), parse_key("numenter").unwrap());
    }

    #[test]
    fn oem_symbol_keys() {
        assert_eq!(parse_key("-").unwrap(), Key::Raw(0xBD));
        assert_eq!(parse_key("minus").unwrap(), Key::Raw(0xBD));
        assert_eq!(parse_key("=").unwrap(), Key::Raw(0xBB));
        assert_eq!(parse_key("[").unwrap(), Key::Raw(0xDB));
        assert_eq!(parse_key("]").unwrap(), Key::Raw(0xDD));
        assert_eq!(parse_key(";").unwrap(), Key::Raw(0xBA));
        assert_eq!(parse_key("'").unwrap(), Key::Raw(0xDE));
        assert_eq!(parse_key(",").unwrap(), Key::Raw(0xBC));
        assert_eq!(parse_key(".").unwrap(), Key::Raw(0xBE));
        assert_eq!(parse_key("/").unwrap(), Key::Raw(0xBF));
        assert_eq!(parse_key("\\").unwrap(), Key::Raw(0xDC));
        assert_eq!(parse_key("`").unwrap(), Key::Raw(0xC0));
        let (k, m) = parse_combo("ctrl+-").unwrap();
        assert_eq!(k, Key::Raw(0xBD));
        assert!(m.ctrl);
    }

    #[test]
    fn combos_split_modifiers_from_key() {
        let (k, m) = parse_combo("ctrl+s").unwrap();
        assert_eq!(k, Key::Raw(b'S' as u16));
        assert!(m.ctrl && !m.shift);

        let (k, m) = parse_combo("Ctrl + Shift + Tab").unwrap();
        assert_eq!(k, Key::Tab);
        assert!(m.ctrl && m.shift);

        let (k, m) = parse_combo("win+d").unwrap();
        assert_eq!(k, Key::Raw(b'D' as u16));
        assert!(m.meta);
    }

    #[test]
    fn modifier_alone_is_a_key() {
        let (k, m) = parse_combo("shift").unwrap();
        assert_eq!(k, Key::Shift);
        assert!(m.is_empty());
    }

    #[test]
    fn unknown_names_are_rejected() {
        assert!(parse_key("wat").is_err());
        assert!(parse_combo("hyper+a").is_err());
        assert!(parse_combo("").is_err());
    }
}
