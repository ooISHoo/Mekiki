# Performance Decisions

Performance changes in Mekiki are accepted only with correctness gates and
representative measurements. This document records optimizations that are easy
to reconsider without the original evidence.

## CPU ZMD

The scalar CPU ZMD matcher remains the correctness baseline. Optimized kernels
must preserve finite behavior for degenerate inputs, CPU/GPU parity, exact peak
selection, golden fixtures, and overlap semantics before speed is considered.

Current measurements and gates were added before changing the implementation.
If an optimization regresses a required fixture or introduces architecture-
specific instability, reverting to the scalar path is the preferred rollback.

## Pyramid search

Direct spatial matching scales poorly for large templates. Pyramid search is the
primary mitigation, with a full-resolution fallback to protect against phase
alignment and downsampling misses. Benchmarks must include both 1080p and 4K
screens and several template sizes; one small-template number is not predictive.

## GPU shared-memory tiling

A shared-memory tiled shader was measured and did not improve the tested RTX
4080 workload. Cache behavior and occupancy offset the expected reduction in
global reads. The implementation was not retained merely because the technique
is conventional.

The conclusion may differ on an integrated GPU with weaker caches. Revisit only
with measurements on the target class and the same matching correctness gates.

## Full zero-copy capture

Profiling showed that the initially suspected CPU/GPU copy was not the dominant
cost after parallel search work. A complete zero-copy path would also complicate
OCR, diagnostics, and CPU fallback. It was therefore deferred in favor of the
measured bottlenecks.

## Running the golden test and the benchmark

These are two separate activities. The golden test checks numerical
correctness against committed fixtures; the benchmark measures CPU and GPU
search time and writes a machine-specific JSON report. A passing golden test
is not a performance result, and a benchmark says nothing about correctness.

### Preparation

- Build from a clean checkout on the machine being measured. Never copy or
  reuse another machine's `target` directory: Rust build artifacts are
  machine-, toolchain-, profile-, and feature-specific.
- Use the toolchain selected by `rust-toolchain.toml` (stable). The first
  Cargo invocation includes compilation time, which the benchmark executable
  does not count.
- `testdata/golden` is committed test input and expected output. Do not
  regenerate it merely to run the test; regeneration needs the pinned
  Python/OpenCV environment in `tools/golden` and is a separate maintenance
  operation.
- Connect AC power and select a stable, performance-oriented Windows power
  mode. Record any non-default CPU or GPU power limits. Close unrelated CPU-
  or GPU-intensive applications. (Measured impact: see "Windows power mode
  barely matters" below; laptops are bound by the vendor GPU power cap.)
- Keep the same benchmark arguments when comparing machines.

### Correctness test

```powershell
cargo test -p mekiki-matching --release --test golden
```

This validates the CPU implementations and, when a suitable GPU adapter is
available, the GPU implementation against `testdata/golden`. If it fails,
keep the complete output and record the GPU name, driver, backend, OS version,
and failing case. Do not loosen tolerances or regenerate fixtures as a first
response.

### Benchmark

Choose a short, filesystem-safe label that identifies the machine, for example
`ryzen-7840u-radeon780m`, and use it in both places:

```powershell
cargo run --release -p mekiki-matching --example bench -- `
  --iters 5 `
  --warmup 2 `
  --label ryzen-7840u-radeon780m `
  --json bench-results/ryzen-7840u-radeon780m-run1.json
```

The benchmark uses deterministic synthetic scenes at full-HD and 4K sizes and
several template sizes. Warmup is controlled by `--warmup`; do not add ad hoc
sleeps. `--no-cpu` skips the CPU backends and `--cpu-all` lifts the operation
cap that otherwise skips the slowest CPU configurations; neither is used for
cross-machine comparison. The JSON report (`schema_version` 2) records raw
samples and the available system and GPU metadata so results can be reviewed
independently.

For a stable final number, repeat the run after the machine has reached a
steady temperature. Write each run to a distinct file (`-run1.json`,
`-run2.json`, ...) and never overwrite evidence until the comparison is done.

### Optional local CPU performance gate

```powershell
cargo test -p mekiki-matching --release --test cpu_performance -- --ignored --nocapture
```

This ignored test checks the fast CPU backend at a smaller input size. It is a
regression signal, not a substitute for the machine benchmark.

### Baselines and reports

`bench-results/` is ignored by Git, so a fresh clone contains no baseline.
The reference report is `bench-results/amd-7950x3d-rtx4080.json` from the
original machine (Ryzen 9 7950X3D + RTX 4080, schema version 2); maintainers
keep it outside the repository and it must be supplied explicitly when a
direct comparison is needed. Smoke-test and prototype reports are not
baselines. `tools/golden/bench_opencv.py` produces the OpenCV reference
numbers on the same machine when a CPU comparison is wanted.

When reporting results, include the exact commit, the command line, the power
mode, CPU, GPU, driver and Windows version, and a short note on background
load, thermal throttling, or anomalies.

## Multi-machine benchmark findings (2026-08)

The benchmark procedure above was executed on three
machines: the reference desktop (Ryzen 9 7950X3D + RTX 4080), a second desktop
(i7-13700KF + RTX 4080), and a laptop (i7-11800H + RTX 3070 Laptop, Razer Blade
15). Raw JSON and per-machine reports live in `bench-results/`, which is not
tracked by Git; the durable conclusions are recorded here.

### Correctness is portable

All machines passed the golden suite 9/9 including the GPU tests, and peak
coordinates and scores matched the reference machine exactly on every run
(0 mismatches across all backends). No compatibility fixes are needed for
these CPU/GPU/driver combinations. No code changes were made in response to
these measurements.

### Windows power mode barely matters for this workload

Switching the Windows 11 power-mode overlay from Balanced to Best Performance
changed the 27-configuration median by 0.7% on the 13700KF desktop. Only
sub-20 ms workloads (1080p, small templates) improved, by up to 1.3x,
consistent with clock ramp-up latency dominating short runs. Benchmark
conclusions do not hinge on the overlay, but keep following the power
instructions in the procedure above so this stays true.

On laptops the binding constraint is the vendor GPU power cap, not the Windows
plan: the RTX 3070 Laptop sat at its enforced 85 W limit with software power
capping active, and closing resident apps moved CPU results (+11% effective
clock) while GPU results did not move at all (median 1.00x). Interpret laptop
GPU numbers as power-limited.

### Known hardware-specific anomaly: naive GPU shader on 13700KF + RTX 4080

The naive (non-tiled) GPU path at 4K/tpl=32 is reproducibly 1.5–1.7x slower on
the 13700KF + RTX 4080 machine than on the reference machine with the same GPU
model (driver 610.62 vs 610.47; power mode and thermals excluded as causes;
30/30 samples outside the reference range). The tiled shader on the same
machine and the naive shader on other machines are normal, and production
selects the tiled shader for templates that fit shared memory (tpl<=32), so
there is no effective impact. Treat this as a driver/host-specific curiosity;
revisit only if the naive path becomes the selected path for small templates
or the gap appears on a second machine.

### Large templates at 4K favor the fast CPU path

On every machine tested, `cpu-fast` beats the GPU for full-resolution 4K
searches with tpl>=128 (1.05–1.5x on the RTX 4080 desktops, up to ~4.6x on the
power-capped laptop at tpl=256). In production this only affects the
full-resolution fallback, since pyramid search shrinks both scene and template
before the expensive full search; the common path is unaffected. A per-size
backend heuristic was considered and deferred: it would add a selection
surface for a rare path, and the crossover point is machine-dependent. Revisit
with measurements if profiling shows the fallback firing often on large
patterns, per the pyramid-search note above.

## Measurement rules

- Record hardware, resolution, template dimensions, backend, and build profile.
- Warm caches and report repeated samples rather than one elapsed time.
- Keep correctness checks in the same change as a new optimized path.
- Prefer a simple rollback switch until the new path has broad hardware data.

