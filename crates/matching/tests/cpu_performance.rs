//! Local performance gates for the experimental CPU backend.
//!
//! Timings are ignored in normal CI because shared runners cannot provide a
//! stable ratio. Run with:
//!
//! cargo test -p mekiki-matching --release --test cpu_performance -- --ignored --nocapture

use std::time::{Duration, Instant};

use mekiki_matching::{Image, MatchMethod, cpu, cpu_fast::FastCpuMatcher};

fn scene(width: usize, height: usize) -> Image<'static> {
    let mut data = vec![0.82; width * height];
    let mut state = 0x9e37_79b9_u32;
    for y in 0..height {
        for x in 0..width {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            if (x + y) % 19 < 4 || state % 97 == 0 {
                data[y * width + x] = (state & 0xff) as f32 / 255.0;
            }
        }
    }
    Image::new(data, width as u32, height as u32)
}

fn crop(input: &Image<'_>, x: usize, y: usize, size: usize) -> Image<'static> {
    let mut data = Vec::with_capacity(size * size);
    for row in 0..size {
        let start = (y + row) * input.width as usize + x;
        data.extend_from_slice(&input.data[start..start + size]);
    }
    Image::new(data, size as u32, size as u32)
}

fn elapsed(mut f: impl FnMut(), iterations: usize) -> Duration {
    f();
    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    start.elapsed() / iterations as u32
}

#[test]
#[ignore = "local release-mode performance gate"]
fn fft_cpu_is_clearly_faster_from_32px() {
    let input = scene(960, 540);
    let mut fast = FastCpuMatcher::new();

    for size in [32, 64, 128] {
        let template = crop(&input, 307, 181, size);
        let reference = elapsed(
            || {
                std::hint::black_box(cpu::match_template(
                    &input,
                    &template,
                    MatchMethod::ZeroMeanDice,
                ));
            },
            2,
        );
        let candidate = elapsed(
            || {
                std::hint::black_box(fast.match_template(&input, &template));
            },
            3,
        );

        eprintln!(
            "960x540 tpl={size}: reference={:.2}ms fast={:.2}ms ratio={:.2}x",
            reference.as_secs_f64() * 1000.0,
            candidate.as_secs_f64() * 1000.0,
            reference.as_secs_f64() / candidate.as_secs_f64(),
        );
        assert!(
            candidate.as_secs_f64() * 1.5 < reference.as_secs_f64(),
            "{size}px did not reach the required 1.5x speedup"
        );
    }
}
