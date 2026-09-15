#!/usr/bin/env python3
"""OpenCV CPU 版の探索時間を測る。Rust 側ベンチの比較対象。

`cargo run --release --example bench` と同じシーン生成・同じサイズ構成で測り、
同じ形式の JSON を出す。Phase 0 の撤退基準（GPU が CPU より遅ければ転換）を
判断するための数字はここと Rust 側の 2 本立てで揃う。

    python tools/golden/bench_opencv.py --json bench-results/opencv.json
"""

from __future__ import annotations

import argparse
import json
import platform
import time
from pathlib import Path

import cv2
import numpy as np

SIZES = [(1920, 1080), (3840, 2160)]
# Rust 側 bench.rs の DEFAULT_TEMPLATES と揃える。
# OpenCV は DFT ベースで面積にほぼ依存しないが、自前実装は比例するため
# 交差点を挟む範囲を振る必要がある。
TEMPLATES = [16, 32, 64, 128, 256]


def synthetic_screen(width: int, height: int, seed: int) -> np.ndarray:
    """Rust 側 `synthetic_screen` と同じ性格の画像を作る。

    画素単位で一致させる必要はない（測るのは時間であって値ではない）が、
    「平坦な面が多く、細かい模様が散る」という統計は揃えておく。
    分岐や早期打ち切りの当たり方が変わると比較にならないため。
    """
    rng = np.random.default_rng(seed)
    img = np.full((height, width), 0.92, dtype=np.float32)

    for _ in range(40):
        x0 = int(rng.integers(0, width))
        y0 = int(rng.integers(0, height))
        w = int(rng.integers(40, 440))
        h = int(rng.integers(30, 330))
        img[y0:y0 + h, x0:x0 + w] = float(rng.uniform(0.15, 0.86))

    n = width * height // 400
    xs = rng.integers(0, width, size=n)
    ys = rng.integers(0, height, size=n)
    img[ys, xs] = rng.random(n).astype(np.float32)
    return img


def bench(iters: int, warmup: int) -> list[dict]:
    rows: list[dict] = []
    for width, height in SIZES:
        scene = synthetic_screen(width, height, seed=width * 7 + 13)
        for tsize in TEMPLATES:
            x, y = width // 3, height // 3
            template = scene[y:y + tsize, x:x + tsize].copy()

            for _ in range(warmup):
                cv2.matchTemplate(scene, template, cv2.TM_CCOEFF_NORMED)

            samples = []
            for _ in range(iters):
                t0 = time.perf_counter()
                cv2.matchTemplate(scene, template, cv2.TM_CCOEFF_NORMED)
                samples.append((time.perf_counter() - t0) * 1000.0)

            samples.sort()
            rows.append({
                "backend": "opencv",
                "width": width,
                "height": height,
                "template": tsize,
                "median_ms": samples[len(samples) // 2],
                "min_ms": samples[0],
                "max_ms": samples[-1],
            })
            print(
                f"opencv  {width:>5}x{height:<5} {tsize:>6} "
                f"{samples[len(samples) // 2]:>12.3f} {samples[0]:>10.3f} {samples[-1]:>10.3f}"
            )
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iters", type=int, default=5)
    parser.add_argument("--warmup", type=int, default=2)
    parser.add_argument("--json", type=str, default=None)
    parser.add_argument("--threads", type=int, default=None,
                        help="OpenCV のスレッド数を固定する（既定は OpenCV 任せ）")
    args = parser.parse_args()

    if args.threads is not None:
        cv2.setNumThreads(args.threads)

    print(f"OpenCV {cv2.__version__} / threads={cv2.getNumThreads()} / {platform.processor()}")
    print(f"{'backend':<8}{'scene':>13}{'tpl':>7}{'median[ms]':>13}{'min[ms]':>11}{'max[ms]':>11}")
    print("-" * 66)

    rows = bench(args.iters, args.warmup)

    if args.json:
        out = Path(args.json)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps({
            "tool": "opencv matchTemplate",
            "device": f"CPU ({platform.processor()})",
            "opencv_version": cv2.__version__,
            "threads": cv2.getNumThreads(),
            "method": "ncc",
            "rows": rows,
        }, indent=2) + "\n", encoding="utf-8")
        print(f"\n結果を {out} に書き出した")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
