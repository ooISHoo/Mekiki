//! Check that OCR works on a real machine.
//!
//! ```text
//! cargo run --release -p mekiki-ocr --example ocr_check
//! cargo run --release -p mekiki-ocr --example ocr_check -- <query>
//! ```
//!
//! Captures the whole screen, pulls out the text and prints the lines found.
//! Passing a query prints the candidates in descending order of match score.

use mekiki_ocr::match_score;

/// Score, text and rectangle. A type that exists only to sort and print the top hits.
type Candidate = (f32, String, (i32, i32, u32, u32));

fn main() {
    if let Err(e) = run() {
        eprintln!("failed: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== usable OCR languages ===");
    match mekiki_ocr::available_languages() {
        Ok(langs) => println!("  {}", langs.join(", ")),
        Err(e) => println!("  cannot read them: {e}"),
    }

    let mut engine = mekiki_ocr::open()?;
    println!(
        "backend: {} / language: {}",
        engine.backend_name(),
        engine.language()
    );
    println!();

    let mut capture = mekiki_capture::open()?;
    let display = capture
        .displays()
        .iter()
        .find(|d| d.is_primary)
        .or_else(|| capture.displays().first())
        .ok_or("no display")?
        .index;
    let frame = capture.capture(display)?;
    println!("captured: {}x{}", frame.width, frame.height);

    let started = std::time::Instant::now();
    let result = engine.recognize_bgra(&frame.bgra, frame.width, frame.height)?;
    let elapsed = started.elapsed();

    let word_count = result.words().count();
    println!(
        "recognised: {} lines / {} words / {:.0} ms",
        result.lines.len(),
        word_count,
        elapsed.as_secs_f64() * 1000.0
    );
    println!();

    let query = std::env::args().nth(1);

    match query {
        None => {
            println!("=== first 15 lines ===");
            for line in result.lines.iter().take(15) {
                let b = line.bounds();
                println!(
                    "  {:>5},{:<5} {}",
                    b.map(|b| b.0).unwrap_or(0),
                    b.map(|b| b.1).unwrap_or(0),
                    line.text
                );
            }
        }
        Some(q) => {
            println!("=== closest to '{q}' ===");
            let mut scored: Vec<Candidate> = result
                .lines
                .iter()
                .filter_map(|l| {
                    l.bounds()
                        .map(|b| (match_score(&q, &l.text), l.text.clone(), b))
                })
                .collect();
            // Also look word by word: targets shorter than a line show up here.
            for w in result.words() {
                scored.push((
                    match_score(&q, &w.text),
                    w.text.clone(),
                    (w.x, w.y, w.width, w.height),
                ));
            }

            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            for (score, text, b) in scored.iter().take(8) {
                println!(
                    "  {score:.4}  {:>5},{:<5} {:>4}x{:<4}  {text}",
                    b.0, b.1, b.2, b.3
                );
            }
        }
    }

    Ok(())
}
