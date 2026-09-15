// Mekiki template matching compute shaders.
//
// Carries over SAD / SSD from upstream (urholaukkarinen/template-matching, MIT)
// and adds ZMD (zero-mean Dice similarity), which defines the score semantics
// behind similar().
//
// Assumptions shared by every entry point:
//   - both input and template are row-major f32 greyscale
//   - the result buffer is (W - w + 1) x (H - h + 1) (valid positions only)
//   - dispatch is rounded up to the workgroup size, so out-of-range invocations
//     must return early (upstream clamped instead, which can write past the end
//     of the result buffer)

struct Uniforms {
    input_width: u32,
    input_height: u32,
    template_width: u32,
    template_height: u32,
    result_width: u32,
    result_height: u32,
    // ZMD only: sum((T - mean(T))^2), one half of the denominator. The host
    // writes it as norm^2. Uniform templates are rejected host-side, so this is
    // always positive here.
    template_norm2: f32,
    // ZMD only: 1 / (template_width * template_height)
    inv_area: f32,
};

@group(0) @binding(0) var<storage, read> input_buf: array<f32>;
@group(0) @binding(1) var<storage, read> template_buf: array<f32>;
@group(0) @binding(2) var<storage, read_write> result_buf: array<f32>;
@group(0) @binding(3) var<uniform> uniforms: Uniforms;

const WG_X: u32 = 16u;
const WG_Y: u32 = 16u;

/// Upper bound on the input tile held in shared memory (count of f32).
///
/// 48x48 = 2304 (9KB). Combined with the 16x16 output tile, a template of up to
/// 33px fits whole.
///
/// It is not raised to the full 4096 (16KB) because **the allocation directly
/// limits how many workgroups fit per SM**. Templates of 34px and above do not
/// fit regardless (64px would need 79x79 = 6241), so a larger tile barely
/// widens the usable range and only costs occupancy.
///
/// For templates above this, the host switches to the naive variant.
const TILE_CAPACITY: u32 = 2304u;

fn out_of_range(x: u32, y: u32) -> bool {
    return x >= uniforms.result_width || y >= uniforms.result_height;
}

// ---------------------------------------------------------------------------
// SAD: sum(|I - T|). Lower is a better match. Fragile under brightness changes,
// so it is not used where score compatibility matters.
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16, 1)
fn main_sad(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (out_of_range(x, y)) {
        return;
    }

    let iw = uniforms.input_width;
    let tw = uniforms.template_width;
    let th = uniforms.template_height;

    // Accumulate per row, same as ZMD (see the comment on main_zmd for why).
    var total = 0.0;
    for (var j = 0u; j < th; j = j + 1u) {
        let in_row = (y + j) * iw + x;
        let tp_row = j * tw;
        var row = 0.0;
        for (var i = 0u; i < tw; i = i + 1u) {
            row = row + abs(input_buf[in_row + i] - template_buf[tp_row + i]);
        }
        total = total + row;
    }

    result_buf[y * uniforms.result_width + x] = total;
}

// ---------------------------------------------------------------------------
// SSD: sum((I - T)^2). Lower is a better match. Equivalent to OpenCV TM_SQDIFF.
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16, 1)
fn main_ssd(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (out_of_range(x, y)) {
        return;
    }

    let iw = uniforms.input_width;
    let tw = uniforms.template_width;
    let th = uniforms.template_height;

    var total = 0.0;
    for (var j = 0u; j < th; j = j + 1u) {
        let in_row = (y + j) * iw + x;
        let tp_row = j * tw;
        var row = 0.0;
        for (var i = 0u; i < tw; i = i + 1u) {
            let d = input_buf[in_row + i] - template_buf[tp_row + i];
            row = row + d * d;
        }
        total = total + row;
    }

    result_buf[y * uniforms.result_width + x] = total;
}

// ---------------------------------------------------------------------------
// ZMD: zero-mean Dice similarity. Higher is a better match (-1.0 ..= 1.0).
//
//   s(x,y) = 2 * sum(T' * I') / (sum(I'^2) + sum(T'^2))
//     T' = T - mean(T)   ... template_buf already holds this, pre-subtracted on
//                            the CPU
//     I' = I - mean(I over window)
//
// Since sum(T') = 0, sum(T' * I') equals sum(T' * I) and subtracting the window
// mean is not needed numerically either. The window-side energy is built from
// the variance:
//     dev2 = max(sum(I^2) - sum(I)^2 / N, 0)   ( = sum(I'^2) )
//
// Its relation to NCC (OpenCV TM_CCOEFF_NORMED) is s = 2rg/(1+g^2), where r is
// NCC and g is the window/template contrast ratio. At equal contrast, s = r.
//
// # Why not NCC
//
// The NCC denominator sqrt(dev2)*tn goes to 0 on a uniform window, and the
// clamp inherited from OpenCV turned that 0/0 into "exactly ±1.0" — a false
// positive we hit in a real application. Worse, contrast invariance itself is
// harmful for UI search: it returns 1.0 even for a copy of the template
// flattened below the quantisation step (visually blank).
//
// In ZMD the denominator is held up from below by dev2 + tn2 >= tn2 > 0, so:
//   - no 0/0 exists structurally (no guards, no clamping branches)
//   - on a uniform window both num and dev2 are ~0 and s falls continuously to ~0
//   - f32 cancellation noise is merely *added* to a large tn2 and cannot make
//     the ratio blow up
//
// By Cauchy-Schwarz, |num| <= sqrt(dev2)*tn <= (dev2+tn2)/2, so |s| <= 1
// strictly. The clamp only exists to absorb f32 rounding overshoot.
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16, 1)
fn main_zmd(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (out_of_range(x, y)) {
        return;
    }

    let iw = uniforms.input_width;
    let tw = uniforms.template_width;
    let th = uniforms.template_height;

    // Two-stage accumulation: the inner loop adds one row into a row-local
    // variable, and the row total is added to the grand total at the end.
    //
    // Naively adding w*h values into a single f32 loses more and more of each
    // addend as the accumulator grows. Measured on a 48x48 window, the maximum
    // difference from OpenCV widened to 2.2e-3 (the f64 CPU implementation was
    // at 2.2e-4). Splitting per row alone drops the accumulation length from
    // w*h to roughly max(w, h).
    var sum_i = 0.0;
    var sum_i2 = 0.0;
    var sum_it = 0.0;

    for (var j = 0u; j < th; j = j + 1u) {
        let in_row = (y + j) * iw + x;
        let tp_row = j * tw;

        var row_i = 0.0;
        var row_i2 = 0.0;
        var row_it = 0.0;
        for (var i = 0u; i < tw; i = i + 1u) {
            let v = input_buf[in_row + i];
            let t = template_buf[tp_row + i];
            row_i = row_i + v;
            row_i2 = row_i2 + v * v;
            row_it = row_it + v * t;
        }

        sum_i = sum_i + row_i;
        sum_i2 = sum_i2 + row_i2;
        sum_it = sum_it + row_it;
    }

    let num = sum_it;
    let wnd_mean2 = sum_i * sum_i * uniforms.inv_area;
    let dev2 = max(sum_i2 - wnd_mean2, 0.0);

    let s = clamp(2.0 * num / (dev2 + uniforms.template_norm2), -1.0, 1.0);
    result_buf[y * uniforms.result_width + x] = s;
}

// ---------------------------------------------------------------------------
// ZMD (shared-memory tiled variant). Computes exactly the same values as
// main_zmd.
//
// # Why it should be faster
//
// In the naive variant each thread reads the input from global memory tw*th
// times, so 256 * tw * th times per workgroup.
//
// But the windows of neighbouring output positions overlap heavily: a 16x16
// output tile only ever needs (16+tw-1) x (16+th-1) of input. Staging that into
// shared memory once collapses the global reads — for a 48x48 template, from
// 589,824 down to 3,969.
//
// The template stays in global memory, but every thread in the workgroup reads
// the same position at the same time, so it broadcasts and stays in cache.
//
// # Constraint
//
// Unusable when the input tile does not fit in shared memory. The host checks
// and falls back to the naive variant.
// ---------------------------------------------------------------------------
var<workgroup> tile: array<f32, TILE_CAPACITY>;

@compute @workgroup_size(16, 16, 1)
fn main_zmd_tiled(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let iw = uniforms.input_width;
    let ih = uniforms.input_height;
    let tw = uniforms.template_width;
    let th = uniforms.template_height;

    // Top-left of the output tile this workgroup is responsible for.
    let origin_x = wid.x * WG_X;
    let origin_y = wid.y * WG_Y;

    // Dimensions of the input tile staged into shared memory.
    let tile_w = WG_X + tw - 1u;
    let tile_h = WG_Y + th - 1u;
    let tile_len = tile_w * tile_h;

    // --- cooperative load ---
    //
    // **Out-of-range threads take part here too.** Returning early before the
    // barrier would make control flow non-uniform, which WGSL does not allow.
    let flat = lid.y * WG_X + lid.x;
    let threads = WG_X * WG_Y;
    for (var i = flat; i < tile_len; i = i + threads) {
        let ty = i / tile_w;
        let tx = i - ty * tile_w;
        let sx = origin_x + tx;
        let sy = origin_y + ty;

        var v = 0.0;
        if (sx < iw && sy < ih) {
            v = input_buf[sy * iw + sx];
        }
        tile[i] = v;
    }

    workgroupBarrier();

    let x = gid.x;
    let y = gid.y;
    if (out_of_range(x, y)) {
        return;
    }

    // From here on the computation is identical to the naive variant; only the
    // source of the input reads changed to shared memory.
    var sum_i = 0.0;
    var sum_i2 = 0.0;
    var sum_it = 0.0;

    for (var j = 0u; j < th; j = j + 1u) {
        let tile_row = (lid.y + j) * tile_w + lid.x;
        let tp_row = j * tw;

        var row_i = 0.0;
        var row_i2 = 0.0;
        var row_it = 0.0;
        for (var i = 0u; i < tw; i = i + 1u) {
            let v = tile[tile_row + i];
            let t = template_buf[tp_row + i];
            row_i = row_i + v;
            row_i2 = row_i2 + v * v;
            row_it = row_it + v * t;
        }

        sum_i = sum_i + row_i;
        sum_i2 = sum_i2 + row_i2;
        sum_it = sum_it + row_it;
    }

    let num = sum_it;
    let wnd_mean2 = sum_i * sum_i * uniforms.inv_area;
    let dev2 = max(sum_i2 - wnd_mean2, 0.0);

    // Same expression as the naive variant. See main_zmd for the reasoning.
    let s = clamp(2.0 * num / (dev2 + uniforms.template_norm2), -1.0, 1.0);
    result_buf[y * uniforms.result_width + x] = s;
}
