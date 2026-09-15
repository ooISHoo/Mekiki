//! Inspect what UI Automation actually returns on a real machine.
//!
//! ```text
//! cargo run --release -p mekiki-uia --example uia_check
//! cargo run --release -p mekiki-uia --example uia_check -- button
//! ```
//!
//! An argument narrows to one control type (`button`, `edit`, ...).
//! A second argument narrows further by a substring of the name.
//! A third argument of `x,y,width,height` limits the search area (to see the
//! speed difference).
//!
//! ```text
//! cargo run --release -p mekiki-uia --example uia_check -- button "" 0,0,1920,1080
//! ```

use std::time::Instant;

use mekiki_uia::{Bounds, ControlType, Query};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    if let Err(e) = run() {
        eprintln!("failed: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut query = Query::new();
    if let Some(kind) = args.first() {
        let parsed = ControlType::parse(kind).ok_or_else(|| {
            format!(
                "unknown type '{kind}'. Usable ones are {}",
                ControlType::NAMES
            )
        })?;
        query = query.with_control_type(parsed);
    }
    let name_filter = args.get(1).filter(|s| !s.is_empty()).cloned();

    if let Some(spec) = args.get(2) {
        let n: Vec<i64> = spec
            .split(',')
            .map(|p| p.trim().parse::<i64>())
            .collect::<Result<_, _>>()
            .map_err(|_| format!("the area must be given as x,y,width,height: '{spec}'"))?;
        if n.len() != 4 {
            return Err(format!("the area needs four numbers: '{spec}'").into());
        }
        query = query.within(Bounds::new(
            n[0] as i32,
            n[1] as i32,
            n[2] as u32,
            n[3] as u32,
        ));
    }

    let t0 = Instant::now();
    let mut finder = mekiki_uia::open()?;
    println!(
        "backend {} (constructed in {:?})",
        finder.backend_name(),
        t0.elapsed()
    );

    let t1 = Instant::now();
    let mut elements = finder.find(&query)?;
    let elapsed = t1.elapsed();

    if let Some(needle) = &name_filter {
        elements.retain(|e| e.name.contains(needle.as_str()));
    }

    println!(
        "conditions {} -> {} hits / {elapsed:?}",
        query.describe(),
        elements.len()
    );
    println!();
    println!(
        "{:<28} {:<12} {:<18} {:>20}",
        "name", "type", "AutomationId", "rect"
    );
    println!("{}", "-".repeat(84));

    for e in elements.iter().take(60) {
        println!(
            "{:<28} {:<12} {:<18} {:>20}",
            truncate(&e.name, 26),
            e.control_type.to_string(),
            truncate(&e.automation_id, 16),
            format!(
                "{},{} {}x{}",
                e.bounds.x, e.bounds.y, e.bounds.width, e.bounds.height
            )
        );
    }
    if elements.len() > 60 {
        println!("... and {} more", elements.len() - 60);
    }

    Ok(())
}

/// Truncate by character count rather than display width. Slightly ragged
/// columns are fine as long as it reads.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}
