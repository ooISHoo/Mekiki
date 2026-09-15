//! A diagnostic that measures the round-trip accuracy of absolute positioning.
//!
//! `SendInput` takes absolute coordinates normalised to 0..65535, and getting
//! the normalisation denominator wrong by one makes the error grow towards the
//! edges of the screen. This moves the cursor to a given coordinate, reads it
//! back with `GetCursorPos` and tabulates the difference.
//!
//! ```text
//! cargo run --release -p mekiki-input --example coord_check
//! ```
//!
//! The cursor is returned to its original position at the end.

use std::thread::sleep;
use std::time::Duration;

fn main() {
    let mut input = match mekiki_input::open() {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cannot open an input backend: {e}");
            std::process::exit(1);
        }
    };

    let metrics = virtual_screen_metrics();
    println!("virtual desktop (GetSystemMetrics):");
    println!(
        "  origin=({}, {}) size={}x{}",
        metrics.0, metrics.1, metrics.2, metrics.3
    );
    println!();

    let original = input
        .cursor_position()
        .expect("cannot read the cursor position");
    println!("original cursor position: {original:?}");
    println!();

    // Test **inside the primary monitor only**.
    //
    // The virtual desktop rectangle is the bounding box of the union of the
    // monitors, so with an uneven arrangement there are coordinates that lie
    // inside it but on no monitor at all. Sending the cursor there makes
    // Windows pull it back to a valid position, which is indistinguishable from
    // a normalisation error.
    let (pw, ph) = primary_screen_size();
    println!("primary monitor: {pw}x{ph} (only this area is checked)");
    println!();

    let targets = [
        (0, 0),
        (pw - 1, 0),
        (0, ph - 1),
        (pw - 1, ph - 1),
        (pw / 2, ph / 2),
        (100, 100),
        (pw / 3, ph / 4),
        (pw * 7 / 8, ph * 7 / 8),
    ];

    println!("{:>16} {:>16} {:>12}", "requested", "read back", "diff");
    println!("{}", "-".repeat(48));

    let mut worst = 0i32;
    for &(x, y) in &targets {
        // Try three times and take the best. That keeps one attempt where the
        // physical mouse was moved from dragging the result down.
        let mut best: Option<((i32, i32), i32)> = None;
        for _ in 0..3 {
            if input.mouse_move(x, y).is_err() {
                break;
            }
            sleep(Duration::from_millis(40));
            let Ok(got) = input.cursor_position() else {
                break;
            };
            let err = (got.0 - x).abs() + (got.1 - y).abs();
            if best.is_none_or(|(_, b)| err < b) {
                best = Some((got, err));
            }
        }

        let Some((got, _)) = best else {
            println!("{:>16} move failed", format!("({x}, {y})"));
            continue;
        };

        let dx = got.0 - x;
        let dy = got.1 - y;
        worst = worst.max(dx.abs()).max(dy.abs());
        println!(
            "{:>16} {:>16} {:>12}",
            format!("({x}, {y})"),
            format!("({}, {})", got.0, got.1),
            format!("({dx}, {dy})")
        );
    }

    let _ = input.mouse_move(original.0, original.1);
    println!();
    println!("worst error: {worst} px");
    if worst <= 1 {
        println!("normalisation is correct (1px from rounding into 0..65535 is expected)");
    } else {
        println!("normalisation is off.");
        println!("NOTE: moving the physical mouse during the run shows up directly as error.");
        println!("      Check again with your hand off the mouse.");
    }
}

#[cfg(windows)]
fn primary_screen_size() -> (i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

#[cfg(not(windows))]
fn primary_screen_size() -> (i32, i32) {
    (1, 1)
}

#[cfg(windows)]
fn virtual_screen_metrics() -> (i32, i32, i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

#[cfg(not(windows))]
fn virtual_screen_metrics() -> (i32, i32, i32, i32) {
    (0, 0, 0, 0)
}
