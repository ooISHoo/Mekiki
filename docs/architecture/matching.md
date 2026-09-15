# Matching Architecture

Mekiki uses zero-mean Dice similarity (ZMD) for image-template matching. This
document records why that metric replaced normalized cross-correlation (NCC)
and which behavior must remain covered by tests.

## The failure that prompted the change

The original NCC implementation could report a perfect match in a visually
uniform search window. This was not a threshold-tuning problem. When local
variance approached zero, the normalized expression became numerically
degenerate and a guard could turn an undefined comparison into a convincing
score.

UI automation makes this case common: disabled areas, dimmed dialogs, blank
panels, and flat application chrome contain large low-variance regions. A score
that jumps from no evidence to `1.0` is therefore unsafe.

## Selected metric

For zero-mean image and template samples, Mekiki evaluates:

```text
ZMD = 2 * sum(I' * T') / (sum(I'^2) + sum(T'^2))
```

The denominator is a sum rather than a product of norms. If the search window
has no variance but the template does, the score naturally tends to zero. No
runtime epsilon or special perfect-match branch is required.

Flat templates are rejected when loaded because they contain no discriminating
information. Dimmed or contrast-shifted controls receive a lower score; this is
intentional because they often represent a different UI state.

Regularized NCC was rejected because it merely moves the instability boundary.
Gradient and census approaches were unnecessary for the observed failure and
would add a second representation with its own degenerate cases.

## Compatibility contract

- The exact peak location for normal fixtures must remain stable.
- CPU and GPU implementations must agree within the tested tolerance.
- Scores must be finite and bounded for flat and near-flat windows.
- Brightness inversion and contrast changes must not become perfect matches.
- The default threshold is an operating policy, not part of the metric.

Golden fixtures and degenerate-input tests are the primary regression guard.
The history is preserved in Git; current behavior belongs in tests and this
decision record.

## Search strategy

Large templates can make direct CPU spatial matching slower than OpenCV's
algorithm selection. Mekiki uses pyramid search to reduce the candidate set and
falls back to full-resolution search when phase alignment or downsampling could
hide a valid peak. Downsample phase must remain consistent between the screen
and template.

Optimization work must preserve the metric and candidate ordering. Performance
changes are accepted only after CPU/GPU parity, degenerate inputs, golden
fixtures, and representative 1080p/4K measurements pass.

