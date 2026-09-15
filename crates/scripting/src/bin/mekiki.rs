//! The Mekiki CLI runner.
//!
//! ```text
//! mekiki run script.rhai       run a script
//! mekiki eval "expr"           evaluate a single expression and print the result
//! mekiki import image.png ...  import images into the content-addressed store
//! mekiki windows               list windows
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use mekiki_scripting::{ScriptHost, assets::AssetStore};

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("mekiki {}", mekiki_core::VERSION);
        return ExitCode::SUCCESS;
    }

    let Some(command) = args.first().map(String::as_str) else {
        usage();
        return ExitCode::from(2);
    };
    if matches!(command, "-h" | "--help" | "help") {
        usage();
        return ExitCode::SUCCESS;
    }

    // The startup banner goes to stderr; stdout carries the machine-readable
    // output of windows / eval.
    eprintln!("Mekiki {}", mekiki_core::VERSION);

    let result = match command {
        "run" => run_script(args.get(1)),
        "eval" => eval_expr(args.get(1)),
        "import" => import_assets(&args[1..]),
        "capture" => capture_region(&args[1..]),
        "windows" => list_windows(),
        other => Err(format!("unknown command: '{other}'")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!(
        "Mekiki {version} - RPA by image matching

Usage:
  mekiki run <script.rhai>     run a script
  mekiki eval <expr>           evaluate a single expression
  mekiki import <image>...     import images into the content-addressed store
  mekiki capture <x> <y> <w> <h> <out.png>
                               crop a screen rectangle and save it
  mekiki windows               list windows
  mekiki --version             print the version

A script's base directory is where the script file lives. Images are referenced
relative to it, or as sha256:... .",
        version = mekiki_core::VERSION
    );
}

fn run_script(path: Option<&String>) -> Result<(), String> {
    let path = PathBuf::from(path.ok_or("specify the script path")?);
    if !path.is_file() {
        return Err(format!("{} does not exist", path.display()));
    }

    // The base is the script's directory. Depending on the working directory
    // instead would make images resolve or not depending on where it was
    // launched from.
    let base = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let host = ScriptHost::new(&base).map_err(|e| e.to_string())?;
    host.run_file(&path).map_err(|e| format!("{e}"))
}

fn eval_expr(expr: Option<&String>) -> Result<(), String> {
    let expr = expr.ok_or("specify the expression to evaluate")?;
    let host = ScriptHost::new(".").map_err(|e| e.to_string())?;
    let value = host
        .eval::<rhai::Dynamic>(expr)
        .map_err(|e| format!("{e}"))?;
    println!("{value}");
    Ok(())
}

fn import_assets(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("specify the images to import".to_string());
    }
    let store = AssetStore::new(".");
    for p in paths {
        let reference = store.import(p).map_err(|e| e.to_string())?;
        println!("{reference}  {p}");
    }
    println!();
    println!("store: {}", store.store_dir().display());
    println!("Reference them from a script as image:sha256:... .");
    Ok(())
}

/// Crop a screen rectangle and save it as a PNG.
///
/// A way to prepare pattern images until the IDE's capture UI (Phase 3-4 of the
/// plan) lands.
fn capture_region(args: &[String]) -> Result<(), String> {
    if args.len() != 5 {
        return Err("capture <x> <y> <w> <h> <out.png>".to_string());
    }
    let num = |s: &String, what: &str| -> Result<i64, String> {
        s.parse::<i64>()
            .map_err(|_| format!("{what} is not a number: '{s}'"))
    };
    let x = num(&args[0], "x")? as i32;
    let y = num(&args[1], "y")? as i32;
    let w = num(&args[2], "width")?;
    let h = num(&args[3], "height")?;
    if w <= 0 || h <= 0 {
        return Err("width and height must be at least 1".to_string());
    }
    let out = PathBuf::from(&args[4]);

    let mut mekiki = mekiki_core::Mekiki::new().map_err(|e| e.to_string())?;
    let rect = mekiki_core::Rect::new(x, y, w as u32, h as u32);
    let region = mekiki.region(rect);
    let frame = mekiki.capture_region(region).map_err(|e| e.to_string())?;

    // BGRA -> RGBA
    let rgba: Vec<u8> = frame
        .bgra
        .chunks_exact(4)
        .flat_map(|p| [p[2], p[1], p[0], 255])
        .collect();
    let img = image::RgbaImage::from_raw(frame.width, frame.height, rgba)
        .ok_or("the image buffer size does not match")?;
    img.save(&out).map_err(|e| e.to_string())?;

    println!(
        "saved {}x{} to {}",
        frame.width,
        frame.height,
        out.display()
    );
    Ok(())
}

fn list_windows() -> Result<(), String> {
    let windows = mekiki_core::window_list().map_err(|e| e.to_string())?;

    // Also print the executable name and class. **A title changes with the file
    // being edited**, so these are what you would write in a script (see
    // docs/architecture/window-locators.md).
    println!(
        "{:<3} {:<22} {:<22} {:<26} title",
        "Z", "exe", "class", "bounds"
    );
    for w in &windows {
        println!(
            "{:<3} {:<22} {:<22} {:<26} {}",
            w.z_order,
            truncate(&w.exe, 20),
            truncate(&w.class_name, 20),
            w.bounds.to_string(),
            w.title
        );
    }
    println!("\n{} in total", windows.len());
    println!("From a script, address them as window(\"exe=...\").");
    Ok(())
}

/// Truncate by character count rather than display width; slight column drift
/// is fine as long as it reads.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}
