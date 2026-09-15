//! The Phase 0 benchmark.
//!
//! Measures a full ZMD search over inputs the size of a full-HD / 4K
//! screenshot. The JSON keeps the original OpenCV-compatible row fields and
//! adds enough system metadata and raw samples to compare different machines.
//!
//! ```text
//! cargo run --release --example bench -- --json bench-results/gpu.json
//! python tools/golden/bench_opencv.py --json bench-results/opencv.json
//! ```
//!
//! The numbers printed here are exactly the evidence behind the Phase 0
//! abandonment criteria in the development plan.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_os = "windows")]
use std::process::Command;

use mekiki_matching::{
    Image, MatchMethod, TemplateMatcher, Tiling, cpu, cpu_fast::FastCpuMatcher, find_extremes,
};
use serde::Serialize;

const DEFAULT_SIZES: &[(u32, u32)] = &[(1920, 1080), (3840, 2160)];

/// Why the template sizes span such a wide range:
///
/// OpenCV's matchTemplate is DFT based, so its runtime barely depends on the
/// template area (measured: about 120ms at 4K for 32, 64 and 128 alike). Our
/// naive exhaustive search is O(W*H*w*h) and scales with the area. That means
/// there is necessarily a crossover somewhere: the GPU wins for small templates
/// and loses for large ones. Locating that point *is* the Phase 0 abandonment
/// decision, so the sweep covers both sides of it.
const DEFAULT_TEMPLATES: &[u32] = &[16, 32, 64, 128, 256];

struct Config {
    iters: usize,
    warmup: usize,
    run_cpu: bool,
    /// The CPU reference implementation is O(W*H*w*h). Running every
    /// combination would take minutes, so it is cut off by operation count.
    cpu_max_ops: u64,
    json: Option<String>,
    label: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            iters: 5,
            warmup: 2,
            run_cpu: true,
            // About 2.1e9 for 32x32 at 1920x1080. That takes a few seconds with
            // rayon.
            cpu_max_ops: 4_000_000_000,
            json: None,
            label: None,
        }
    }
}

fn parse_args() -> Config {
    let mut cfg = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iters" => cfg.iters = args.next().and_then(|v| v.parse().ok()).unwrap_or(5),
            "--warmup" => cfg.warmup = args.next().and_then(|v| v.parse().ok()).unwrap_or(2),
            "--no-cpu" => cfg.run_cpu = false,
            "--cpu-all" => cfg.cpu_max_ops = u64::MAX,
            "--json" => cfg.json = args.next(),
            "--label" => cfg.label = args.next(),
            "--help" | "-h" => {
                eprintln!(
                    "usage: bench [--iters N] [--warmup N] [--no-cpu] [--cpu-all] \
                     [--label NAME] [--json PATH]"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    cfg
}

/// Build a deterministic pseudo-screenshot.
///
/// It mixes flat areas, rectangles and fine detail to stay close to a real
/// screen. Pure random noise everywhere would give an unrealistic cache hit
/// rate, and it would also keep the score denominator large, skewing the branch
/// distribution away from reality.
fn synthetic_screen(width: u32, height: u32, seed: u64) -> Image<'static> {
    let mut state = seed | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let mut data = vec![0.92f32; (width as usize) * (height as usize)];

    // Lay down some larger panels.
    for _ in 0..40 {
        let x0 = (next() % u64::from(width)) as u32;
        let y0 = (next() % u64::from(height)) as u32;
        let w = 40 + (next() % 400) as u32;
        let h = 30 + (next() % 300) as u32;
        let shade = 0.15 + (next() % 1000) as f32 / 1400.0;
        for y in y0..(y0 + h).min(height) {
            for x in x0..(x0 + w).min(width) {
                data[(y as usize) * (width as usize) + x as usize] = shade;
            }
        }
    }

    // Fine detail (standing in for text and icons).
    for _ in 0..(width as u64 * height as u64 / 400) {
        let x = (next() % u64::from(width)) as u32;
        let y = (next() % u64::from(height)) as u32;
        let v = (next() % 1000) as f32 / 1000.0;
        data[(y as usize) * (width as usize) + x as usize] = v;
    }

    Image::new(data, width, height)
}

/// Crop the template from the position with the highest variance.
///
/// A hard-coded position can land inside one of the flat panels of the
/// synthetic scene. A uniform template makes the ZMD denominator (tn2) zero, so
/// the matcher takes the early return that fills the map without dispatching,
/// and **no GPU time is measured at all**. The very first measurements hit
/// exactly this at 1920x1080 / 16px.
fn pick_textured_crop(src: &Image<'_>, size: u32) -> Image<'static> {
    let mut best = (f64::NEG_INFINITY, 0u32, 0u32);
    // Walk a fixed grid. No randomness needed.
    for gy in 1..8u32 {
        for gx in 1..8u32 {
            let x = (src.width - size) * gx / 8;
            let y = (src.height - size) * gy / 8;
            let patch = crop(src, x, y, size, size);
            let n = f64::from(size) * f64::from(size);
            let sum: f64 = patch.data.iter().map(|&v| f64::from(v)).sum();
            let sq: f64 = patch
                .data
                .iter()
                .map(|&v| f64::from(v) * f64::from(v))
                .sum();
            let var = (sq - sum * sum / n) / n;
            if var > best.0 {
                best = (var, x, y);
            }
        }
    }

    assert!(
        best.0.sqrt() > 1e-3,
        "no textured position available for a {size}px template (stddev {:.2e}). \
         Measuring as-is would fall into the uniform-template early return and \
         record no GPU time",
        best.0.sqrt()
    );
    crop(src, best.1, best.2, size, size)
}

fn crop(src: &Image<'_>, x: u32, y: u32, w: u32, h: u32) -> Image<'static> {
    let mut out = Vec::with_capacity((w * h) as usize);
    for j in 0..h {
        let row = ((y + j) as usize) * (src.width as usize) + x as usize;
        out.extend_from_slice(&src.data[row..row + w as usize]);
    }
    Image::new(out, w, h)
}

#[derive(Serialize)]
struct Timing {
    median_ms: f64,
    min_ms: f64,
    max_ms: f64,
    samples_ms: Vec<f64>,
}

fn summarize(samples_ms: Vec<f64>) -> Timing {
    let mut sorted = samples_ms.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    Timing {
        median_ms: sorted[sorted.len() / 2],
        min_ms: sorted[0],
        max_ms: sorted[sorted.len() - 1],
        samples_ms,
    }
}

#[derive(Serialize)]
struct Row {
    backend: &'static str,
    width: u32,
    height: u32,
    template: u32,
    #[serde(flatten)]
    timing: Timing,
    peak_score: f32,
    peak_at: (u32, u32),
}

#[derive(Serialize)]
struct SystemInfo {
    cpu_name: String,
    logical_threads: usize,
    rayon_threads: usize,
    rayon_num_threads_env: Option<String>,
    os: &'static str,
    arch: &'static str,
}

#[derive(Serialize)]
struct GpuInfo {
    name: String,
    backend: String,
    device_type: String,
    vendor_id: u32,
    device_id: u32,
    driver: String,
    driver_info: String,
}

#[derive(Serialize)]
struct RunConfig {
    iterations: usize,
    warmup_iterations: usize,
    cpu_reference_enabled: bool,
    cpu_reference_max_operations: u64,
    scene_sizes: &'static [(u32, u32)],
    template_sizes: &'static [u32],
}

#[derive(Serialize)]
struct BackendComparison {
    width: u32,
    height: u32,
    template: u32,
    fastest_gpu_backend: String,
    gpu_median_ms: f64,
    cpu_fast_median_ms: f64,
    /// Below 1.0 means cpu-fast was faster; above 1.0 means the GPU was faster.
    cpu_fast_to_gpu_ratio: f64,
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u32,
    tool: &'static str,
    device: &'a str,
    mekiki_version: &'static str,
    generated_unix_ms: u128,
    label: &'a Option<String>,
    profile: &'static str,
    method: &'static str,
    system: &'a SystemInfo,
    gpu: &'a Option<GpuInfo>,
    config: RunConfig,
    rows: &'a [Row],
    comparisons: Vec<BackendComparison>,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cfg = parse_args();

    let mut matcher = match TemplateMatcher::new() {
        Ok(m) => Some(m),
        Err(e) => {
            eprintln!("cannot initialise a GPU: {e}");
            eprintln!("measuring the CPU implementation only");
            None
        }
    };
    let mut fast_cpu = FastCpuMatcher::new();
    let system = system_info();
    let gpu = matcher.as_ref().map(gpu_info);

    println!("CPU: {}", system.cpu_name);
    if let Some(info) = &gpu {
        println!(
            "GPU: {} / backend={} / type={} / driver={} / driver_info={}",
            info.name, info.backend, info.device_type, info.driver, info.driver_info
        );
    }
    println!(
        "iters={} warmup={} threads={}",
        cfg.iters,
        cfg.warmup,
        rayon_threads()
    );
    println!();

    let mut rows: Vec<Row> = Vec::new();

    for &(width, height) in DEFAULT_SIZES {
        let scene = synthetic_screen(width, height, u64::from(width) * 7 + 13);

        for &tsize in DEFAULT_TEMPLATES {
            let template = pick_textured_crop(&scene, tsize);

            if let Some(m) = matcher.as_mut() {
                // Measure both the naive and the tiled variant. They compute
                // the same values, so the difference is exactly the effect of
                // the optimisation.
                let fits = mekiki_matching::tile_fits_for_test((tsize, tsize));
                let variants: &[(&'static str, Tiling)] = if fits {
                    &[("gpu", Tiling::Never), ("gpu-tiled", Tiling::WhenItFits)]
                } else {
                    // At sizes that do not fit in shared memory there is no
                    // choice to make.
                    &[("gpu", Tiling::Never)]
                };

                for (label, tiling) in variants {
                    m.set_tiling(*tiling);

                    for _ in 0..cfg.warmup {
                        let _ = m.match_template(&scene, &template, MatchMethod::ZeroMeanDice);
                    }
                    let mut samples = Vec::with_capacity(cfg.iters);
                    let mut last = None;
                    for _ in 0..cfg.iters {
                        let t0 = Instant::now();
                        let r = m
                            .match_template(&scene, &template, MatchMethod::ZeroMeanDice)
                            .expect("GPU matching failed");
                        samples.push(t0.elapsed().as_secs_f64() * 1000.0);
                        last = Some(r);
                    }
                    let e = find_extremes(&last.unwrap());
                    rows.push(Row {
                        backend: label,
                        width,
                        height,
                        template: tsize,
                        timing: summarize(samples),
                        peak_score: e.max_value,
                        peak_at: e.max_value_location,
                    });
                }
            }

            for _ in 0..cfg.warmup {
                let _ = fast_cpu.match_template(&scene, &template);
            }
            let mut samples = Vec::with_capacity(cfg.iters);
            let mut last = None;
            for _ in 0..cfg.iters {
                let t0 = Instant::now();
                let r = fast_cpu
                    .match_template(&scene, &template)
                    .expect("fast CPU matching failed");
                samples.push(t0.elapsed().as_secs_f64() * 1000.0);
                last = Some(r);
            }
            let e = find_extremes(&last.unwrap());
            rows.push(Row {
                backend: "cpu-fast",
                width,
                height,
                template: tsize,
                timing: summarize(samples),
                peak_score: e.max_value,
                peak_at: e.max_value_location,
            });

            let ops = u64::from(width) * u64::from(height) * u64::from(tsize) * u64::from(tsize);
            if cfg.run_cpu && ops <= cfg.cpu_max_ops {
                // One run is enough for the CPU. It takes tens of seconds.
                let t0 = Instant::now();
                let r = cpu::match_template(&scene, &template, MatchMethod::ZeroMeanDice)
                    .expect("CPU matching failed");
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                let e = find_extremes(&r);
                rows.push(Row {
                    backend: "cpu",
                    width,
                    height,
                    template: tsize,
                    timing: Timing {
                        median_ms: ms,
                        min_ms: ms,
                        max_ms: ms,
                        samples_ms: vec![ms],
                    },
                    peak_score: e.max_value,
                    peak_at: e.max_value_location,
                });
            }
        }
    }

    print_table(&rows);

    if let Some(path) = &cfg.json {
        write_json(path, &cfg, &system, &gpu, &rows);
    }
}

fn rayon_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

fn system_info() -> SystemInfo {
    let logical_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    SystemInfo {
        cpu_name: cpu_name(),
        logical_threads,
        rayon_threads: rayon_threads(),
        rayon_num_threads_env: std::env::var("RAYON_NUM_THREADS").ok(),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    }
}

fn cpu_name() -> String {
    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = Command::new("reg")
            .args([
                "query",
                r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0",
                "/v",
                "ProcessorNameString",
            ])
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(name) = stdout
                .lines()
                .find_map(|line| line.split_once("REG_SZ").map(|(_, value)| value.trim()))
                .filter(|name| !name.is_empty())
            {
                return name.to_string();
            }
        }
    }

    std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".to_string())
}

fn gpu_info(matcher: &TemplateMatcher) -> GpuInfo {
    let info = matcher.adapter_info();
    GpuInfo {
        name: info.name.clone(),
        backend: format!("{:?}", info.backend),
        device_type: format!("{:?}", info.device_type),
        vendor_id: info.vendor,
        device_id: info.device,
        driver: info.driver.clone(),
        driver_info: info.driver_info.clone(),
    }
}

fn print_table(rows: &[Row]) {
    println!(
        "{:<8} {:>11} {:>6} {:>12} {:>10} {:>10} {:>12}",
        "backend", "scene", "tpl", "median[ms]", "min[ms]", "max[ms]", "peak"
    );
    println!("{}", "-".repeat(76));
    for r in rows {
        println!(
            "{:<8} {:>5}x{:<5} {:>6} {:>12.3} {:>10.3} {:>10.3} {:>7.4} @{:?}",
            r.backend,
            r.width,
            r.height,
            r.template,
            r.timing.median_ms,
            r.timing.min_ms,
            r.timing.max_ms,
            r.peak_score,
            r.peak_at,
        );
    }
    println!();

    // Compute ratios between pairs measured under the same conditions.
    let find = |backend: &str, r: &Row| {
        rows.iter().find(|c| {
            c.backend == backend
                && c.width == r.width
                && c.height == r.height
                && c.template == r.template
        })
    };

    println!("  --- effect of tiling (naive -> tiled) ---");
    for r in rows.iter().filter(|r| r.backend == "gpu") {
        match find("gpu-tiled", r) {
            Some(t) => println!(
                "  {}x{} tpl={:>3} : {:>8.1} -> {:>8.1} ms  ({:.2}x)",
                r.width,
                r.height,
                r.template,
                r.timing.median_ms,
                t.timing.median_ms,
                r.timing.median_ms / t.timing.median_ms
            ),
            None => println!(
                "  {}x{} tpl={:>3} : {:>8.1} ms  (does not fit in shared memory, no tiling)",
                r.width, r.height, r.template, r.timing.median_ms
            ),
        }
    }

    println!("  --- ratio against our own CPU implementation ---");
    for r in rows.iter().filter(|r| r.backend == "cpu") {
        let best = ["gpu-tiled", "gpu"]
            .iter()
            .filter_map(|b| find(b, r))
            .min_by(|a, b| a.timing.median_ms.total_cmp(&b.timing.median_ms));
        if let Some(g) = best {
            println!(
                "  {}x{} tpl={:>3} : GPU({}) is {:.1}x the CPU",
                r.width,
                r.height,
                r.template,
                g.backend,
                r.timing.median_ms / g.timing.median_ms
            );
        }
    }

    println!("  --- fast CPU against reference CPU ---");
    for r in rows.iter().filter(|r| r.backend == "cpu") {
        if let Some(fast) = find("cpu-fast", r) {
            println!(
                "  {}x{} tpl={:>3} : {:>8.1} -> {:>8.1} ms  ({:.2}x)",
                r.width,
                r.height,
                r.template,
                r.timing.median_ms,
                fast.timing.median_ms,
                r.timing.median_ms / fast.timing.median_ms,
            );
        }
    }

    println!("  --- fast CPU against fastest GPU ---");
    for comparison in backend_comparisons(rows) {
        let ratio = comparison.cpu_fast_to_gpu_ratio;
        let verdict = if ratio < 1.0 {
            format!("CPU-fast is {:.2}x faster", 1.0 / ratio)
        } else {
            format!("GPU is {ratio:.2}x faster")
        };
        println!(
            "  {}x{} tpl={:>3} : GPU({}) {:>8.1} ms / CPU-fast {:>8.1} ms  ({verdict})",
            comparison.width,
            comparison.height,
            comparison.template,
            comparison.fastest_gpu_backend,
            comparison.gpu_median_ms,
            comparison.cpu_fast_median_ms,
        );
    }
}

fn backend_comparisons(rows: &[Row]) -> Vec<BackendComparison> {
    rows.iter()
        .filter(|row| row.backend == "cpu-fast")
        .filter_map(|cpu| {
            let gpu = rows
                .iter()
                .filter(|row| {
                    matches!(row.backend, "gpu" | "gpu-tiled")
                        && row.width == cpu.width
                        && row.height == cpu.height
                        && row.template == cpu.template
                })
                .min_by(|a, b| a.timing.median_ms.total_cmp(&b.timing.median_ms))?;
            Some(BackendComparison {
                width: cpu.width,
                height: cpu.height,
                template: cpu.template,
                fastest_gpu_backend: gpu.backend.to_string(),
                gpu_median_ms: gpu.timing.median_ms,
                cpu_fast_median_ms: cpu.timing.median_ms,
                cpu_fast_to_gpu_ratio: cpu.timing.median_ms / gpu.timing.median_ms,
            })
        })
        .collect()
}

fn write_json(path: &str, cfg: &Config, system: &SystemInfo, gpu: &Option<GpuInfo>, rows: &[Row]) {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let report = Report {
        schema_version: 2,
        tool: "mekiki-matching bench",
        device: gpu.as_ref().map(|info| info.name.as_str()).unwrap_or("n/a"),
        mekiki_version: env!("CARGO_PKG_VERSION"),
        generated_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        label: &cfg.label,
        profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        method: "zmd",
        system,
        gpu,
        config: RunConfig {
            iterations: cfg.iters,
            warmup_iterations: cfg.warmup,
            cpu_reference_enabled: cfg.run_cpu,
            cpu_reference_max_operations: cfg.cpu_max_ops,
            scene_sizes: DEFAULT_SIZES,
            template_sizes: DEFAULT_TEMPLATES,
        },
        rows,
        comparisons: backend_comparisons(rows),
    };
    let s = serde_json::to_string_pretty(&report).expect("benchmark report should serialize");

    match std::fs::write(path, s) {
        Ok(()) => println!("wrote the results to {path}"),
        Err(e) => eprintln!("cannot write to {path}: {e}"),
    }
}
