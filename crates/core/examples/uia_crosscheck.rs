//! Checks that UIA rectangles are in the same coordinate system as the capture.
//!
//! ```text
//! cargo run --release -p mekiki-core --example uia_crosscheck
//! cargo run --release -p mekiki-core --example uia_crosscheck -- Search
//! ```
//!
//! # Why measure this
//!
//! Phase 4-4 exists because DPI scaling breaks image matching. But UIA can also
//! return **logical pixel** coordinates if queried the wrong way, which
//! produces the nastiest failure of all: the rectangles come back fine while
//! every click lands in the wrong place.
//!
//! So each rectangle UIA returns is **cropped out of the screen capture and run
//! through OCR**, and compared against the name UIA claimed. If the coordinate
//! systems disagree, the crop lands on something else and the text will not
//! match.
//!
//! Comparing by image matching would be circular — UIA chose the crop position,
//! so it necessarily matches there. OCR reads the pixel content independently,
//! so it works.

use mekiki_capture::Rect;
use mekiki_core::Mekiki;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    if let Err(e) = run() {
        eprintln!("failed: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut mekiki = Mekiki::new()?;
    let screen = mekiki.screen(0)?;
    let filter = std::env::args().nth(1);

    let mut finder = mekiki_uia::open()?;
    let mut ocr = mekiki_ocr::open()?;

    let elements = finder.find(&mekiki_uia::Query::new().within(mekiki_uia::Bounds::new(
        screen.rect.x,
        screen.rect.y,
        screen.rect.width,
        screen.rect.height,
    )))?;
    println!("elements returned by UIA: {}", elements.len());

    // Capture once and reuse it for every candidate. Capturing per candidate
    // would let the screen move between them.
    let frame = mekiki.capture_region(screen)?;
    println!();

    let mut checked = 0usize;
    let mut agreed = 0usize;

    println!(
        "{:<26} {:<24} {:>8}",
        "UIA name", "OCR of the crop", "score"
    );
    println!("{}", "-".repeat(62));

    for e in &elements {
        if e.name.is_empty() {
            continue;
        }
        if let Some(f) = &filter
            && !e.name.contains(f.as_str())
        {
            continue;
        }
        if !is_comparable(e) {
            continue;
        }

        let rect = Rect::new(e.bounds.x, e.bounds.y, e.bounds.width, e.bounds.height);
        let Some((bgra, w, h)) = crop_bgra(&frame, rect) else {
            continue;
        };
        let Ok(recognition) = ocr.recognize_bgra(&bgra, w, h) else {
            continue;
        };

        let text = recognition.text().replace('\n', " ");
        if text.trim().is_empty() {
            // An icon-only button and the like. Being unreadable is not evidence
            // of a mismatch.
            continue;
        }

        // A UIA name is not necessarily what is displayed, so a partial match
        // is good enough.
        let score = mekiki_ocr::match_score(&text, &e.name);
        checked += 1;
        if score >= 0.5 {
            agreed += 1;
        }

        println!(
            "{:<26} {:<24} {:>8.3}",
            truncate(&e.name, 24),
            truncate(&text, 22),
            score
        );

        if checked >= 12 {
            break;
        }
    }

    println!();
    if checked == 0 {
        println!(
            "no comparable element was found (one at least 40x40 with readable text is required)."
        );
        println!("Try again, passing part of a displayed name as an argument.");
        return Ok(());
    }
    println!("of the {checked} compared, the text matched in {agreed}.");
    if agreed * 2 >= checked {
        println!(
            "=> UIA coordinates share the capture's coordinate system. No DPI virtualisation."
        );
    } else {
        println!("=> **They disagree.** UIA is likely returning logical pixels.");
    }

    Ok(())
}

/// Whether this is an element whose name should match the text it displays.
///
/// **Without narrowing this down, no conclusion emerges.** Selecting by size
/// alone pulled in a flood of containers such as windows and panes. A
/// container's name is a role label like "chat messages", and cropping it reads
/// the text inside. Of course that does not match, but it is not evidence of a
/// coordinate offset either.
///
/// Only **small controls whose displayed label is their name** are usable for
/// verifying the coordinate system.
fn is_comparable(e: &mekiki_uia::Element) -> bool {
    use mekiki_uia::ControlType::*;

    if !matches!(
        e.control_type,
        Button | CheckBox | RadioButton | Hyperlink | TabItem | MenuItem | ListItem | Text
    ) {
        return false;
    }

    // OCR has a lower bound of 40x40. The upper bound is there to reject
    // containers; a control is almost never larger than this.
    (40..=600).contains(&e.bounds.width) && (40..=200).contains(&e.bounds.height)
}

/// Crop a rectangle out of a frame and return it as BGRA.
fn crop_bgra(frame: &mekiki_core::Frame, rect: Rect) -> Option<(Vec<u8>, u32, u32)> {
    let x = rect.x - frame.origin.0;
    let y = rect.y - frame.origin.1;
    if x < 0 || y < 0 {
        return None;
    }
    let (x, y) = (x as u32, y as u32);
    if x + rect.width > frame.width || y + rect.height > frame.height {
        return None;
    }

    let mut out = Vec::with_capacity((rect.width * rect.height * 4) as usize);
    for row in 0..rect.height {
        let start = (((y + row) * frame.width + x) * 4) as usize;
        out.extend_from_slice(&frame.bgra[start..start + (rect.width * 4) as usize]);
    }
    Some((out, rect.width, rect.height))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}
