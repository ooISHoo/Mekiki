#!/usr/bin/env python3
"""マッチングのゴールデンフィクスチャを生成する。

スコアは ZMD（ゼロ平均ダイス類似度）:

    s = 2 * sum(I'·T') / (sum(I'^2) + sum(T'^2))

期待値は分子と分母を**別実装**から組み立てる: 分子は OpenCV `TM_CCOEFF`
（DFT 相互相関）、分母は numpy の積分画像（f64）。Rust 側と
「同じ式を同じ順で書いたから一致する」だけの照合にならないようにするため。

歴史的経緯: Phase 0 では OpenCV `TM_CCOEFF_NORMED` とのビット単位に近い一致が
完了条件だった。2026-08-15 に互換要件を撤廃して ZMD へ置き換えた
（docs/architecture/matching.md）。NCC 時代に必要だった 0/0 位置の除外
（unstable_indices）と分母依存の許容誤差は、ZMD では分母が構造的に正のため不要。

出力先: <repo>/testdata/golden/
  scenes/*.png       … 8bit グレースケールのシーン画像
  templates/*.png    … 8bit グレースケールのテンプレート
  expected/*.f32     … ZMD スコアマップ全体（リトルエンディアン f32、row-major）
  manifest.json      … ケース定義と期待値の要約

画像を「グレースケール PNG として」保存しているのが重要で、こうすれば
Python 側と Rust 側で BGR→Gray 変換の丸め差が入り込まない。両者はまったく同じ
画素値から出発する。

使い方:
    python tools/golden/gen_golden.py
    python tools/golden/gen_golden.py --check   # 再生成せず既存ゴールデンを検証
"""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
import sys
from pathlib import Path

import cv2
import numpy as np

REPO_ROOT = Path(__file__).resolve().parents[2]
OUT_ROOT = REPO_ROOT / "testdata" / "golden"

# 完全一致マップを書き出す上限。これを超えるケースは要約統計とサンプル点のみ。
MAX_FULL_MAP_ELEMS = 120_000
# サンプル点の数（決定的に選ぶ）。
NUM_SAMPLES = 2000

METHODS = ("zmd", "ssd")


# --------------------------------------------------------------------------
# シーン生成（すべて決定的）
# --------------------------------------------------------------------------

def _rng(seed: int) -> np.random.Generator:
    return np.random.default_rng(seed)


def make_ui_scene(width: int, height: int, seed: int) -> np.ndarray:
    """GUI っぽいシーンを作る。

    実際の用途がデスクトップ自動化なので、自然画像より
    「平坦な面 + 矩形 + 細い線 + 文字」に寄せたほうが分母が 0 に近づく状況を踏める。
    """
    rng = _rng(seed)
    img = np.full((height, width), 235, dtype=np.uint8)

    # 背景の緩いグラデーション
    gx = np.linspace(-8, 8, width, dtype=np.float32)
    gy = np.linspace(-8, 8, height, dtype=np.float32)
    img = np.clip(img.astype(np.float32) + gy[:, None] + gx[None, :], 0, 255).astype(np.uint8)

    # パネル・ボタン・線
    for _ in range(14):
        x = int(rng.integers(0, max(1, width - 40)))
        y = int(rng.integers(0, max(1, height - 30)))
        w = int(rng.integers(20, 70))
        h = int(rng.integers(12, 40))
        shade = int(rng.integers(60, 210))
        cv2.rectangle(img, (x, y), (x + w, y + h), shade, -1)
        cv2.rectangle(img, (x, y), (x + w, y + h), max(0, shade - 50), 1)

    for _ in range(10):
        p1 = (int(rng.integers(0, width)), int(rng.integers(0, height)))
        p2 = (int(rng.integers(0, width)), int(rng.integers(0, height)))
        cv2.line(img, p1, p2, int(rng.integers(0, 120)), 1)

    # 文字（高周波成分。UI アイコンやラベルの代理）
    for _ in range(12):
        org = (int(rng.integers(0, max(1, width - 60))), int(rng.integers(12, height)))
        text = "".join(chr(int(c)) for c in rng.integers(65, 91, size=int(rng.integers(2, 6))))
        cv2.putText(img, text, org, cv2.FONT_HERSHEY_SIMPLEX, 0.4, int(rng.integers(0, 90)), 1, cv2.LINE_AA)

    return img


def plant(scene: np.ndarray, patch: np.ndarray, positions: list[tuple[int, int]]) -> np.ndarray:
    out = scene.copy()
    ph, pw = patch.shape
    for x, y in positions:
        out[y:y + ph, x:x + pw] = patch
    return out


def pick_textured_patch(scene: np.ndarray, w: int, h: int, seed: int) -> tuple[int, int]:
    """分散の大きい位置を選んでテンプレートの切り出し元にする。

    平坦な領域を切り出すと、背景のどこにでも完全一致してしまい
    「ピークが植え込み位置に来る」という前提が壊れる。実際 UI 上のテンプレートも
    模様のある部分を指定するのが普通なので、フィクスチャもそれに合わせる。
    """
    rng = _rng(seed)
    height, width = scene.shape
    best = (-1.0, 0, 0)
    for _ in range(3000):
        x = int(rng.integers(0, width - w + 1))
        y = int(rng.integers(0, height - h + 1))
        std = float(scene[y:y + h, x:x + w].std())
        if std > best[0]:
            best = (std, x, y)
    return best[1], best[2]


def make_icon(size: int, seed: int) -> np.ndarray:
    """植え込み用の、はっきり見分けのつく合成アイコン。

    シーンから切り出すと偶然の一致を招くので、`repeated` ケースだけは
    背景と明確に異なる図形を合成して使う。
    """
    rng = _rng(seed)
    img = np.full((size, size), 245, dtype=np.uint8)
    cv2.rectangle(img, (1, 1), (size - 2, size - 2), 25, 2)
    cv2.circle(img, (size // 2, size // 2), max(2, size // 4), 110, -1)
    cv2.line(img, (3, size - 4), (size - 4, 3), 10, 2)
    cv2.putText(img, "K", (size // 5, size - size // 5), cv2.FONT_HERSHEY_SIMPLEX,
                size / 44.0, 255, 2, cv2.LINE_AA)
    # わずかな粒状ノイズで、平坦部が偶然一致するのを避ける
    noise = rng.integers(-6, 7, img.shape)
    return np.clip(img.astype(np.int32) + noise, 0, 255).astype(np.uint8)


# --------------------------------------------------------------------------
# ケース定義
# --------------------------------------------------------------------------

def build_cases() -> list[dict]:
    cases: list[dict] = []

    # 1. 完全一致。ピークはちょうど 1.0 になるはず。
    scene = make_ui_scene(320, 240, seed=1)
    tx, ty = pick_textured_patch(scene, 48, 48, seed=101)
    tpl = scene[ty:ty + 48, tx:tx + 48].copy()
    cases.append(dict(
        name="exact",
        note="シーンから切り出したテンプレート。NCC のピークは 1.0。",
        scene=scene, template=tpl, planted=[(tx, ty)],
    ))

    # 2. ノイズ付き。スコアは 1.0 未満だが位置は変わらないはず。
    base = make_ui_scene(320, 240, seed=2)
    tx, ty = pick_textured_patch(base, 48, 48, seed=102)
    tpl = base[ty:ty + 48, tx:tx + 48].copy()
    noisy = np.clip(base.astype(np.float32) + _rng(20).normal(0, 6.0, base.shape), 0, 255).astype(np.uint8)
    cases.append(dict(
        name="noise",
        note="ガウスノイズ σ=6 を載せたシーン。位置は保たれ、スコアだけ下がる。",
        scene=noisy, template=tpl, planted=[(tx, ty)],
    ))

    # 3. 輝度・コントラスト変化。ZMD のコントラスト感度を固定するケース。
    #    NCC 時代は「線形変換に不変でピーク 1.0」だったが、ZMD は意図的に
    #    p(g) = 2g/(1+g²) の分だけ減点する（g=0.65 で 0.913）。位置は保たれる。
    base = make_ui_scene(320, 240, seed=3)
    tx, ty = pick_textured_patch(base, 48, 48, seed=103)
    tpl = base[ty:ty + 48, tx:tx + 48].copy()
    shifted = np.clip(base.astype(np.float32) * 0.65 + 55.0, 0, 255).astype(np.uint8)
    cases.append(dict(
        name="brightness",
        note="シーン全体を 0.65 倍 + 55 したもの。ZMD は位置を保ったまま p(0.65)=0.913 へ減点する。",
        scene=shifted, template=tpl, planted=[(tx, ty)],
        min_peak=0.88,
    ))

    # 4. 同じテンプレートが 5 箇所。findAll / NMS の検証用。
    base = make_ui_scene(320, 240, seed=4)
    patch = make_icon(32, seed=104)
    positions = [(20, 20), (140, 30), (250, 90), (60, 160), (200, 180)]
    repeated = plant(base, patch, positions)
    cases.append(dict(
        name="repeated",
        note="合成アイコンを 5 箇所に埋め込み。NMS が 5 件ちょうど返すことを確認する。",
        scene=repeated, template=patch, planted=positions,
    ))

    # 5. 一様なテンプレート。ZMD では「何とも一致しない」= 全面 0。
    #    実運用では Pattern のロード検査が先に弾くので、これはマッチャ防御の検証。
    base = make_ui_scene(320, 240, seed=5)
    flat_tpl = np.full((24, 24), 128, dtype=np.uint8)
    cases.append(dict(
        name="flat_template",
        note="定数テンプレート。tn2=0 の唯一のケースで、マッチャは全面 0 を返す。",
        scene=base, template=flat_tpl, planted=[],
    ))

    # 6. シーン側に広い一様領域。NCC 時代は 0/0 のクランプ挙動を踏むケースだったが、
    #    ZMD では一様な窓が連続に ~0 へ落ちることの検証になる。
    base = make_ui_scene(320, 240, seed=6)
    base[120:240, 0:160] = 200  # 大きな平坦ブロック
    tx, ty = pick_textured_patch(base, 36, 36, seed=106)
    tpl = base[ty:ty + 36, tx:tx + 36].copy()
    cases.append(dict(
        name="flat_region",
        note="シーンに定数領域あり。一様な窓では分子・分母窓側とも ~0 で、スコアは ~0 に落ちる。",
        scene=base, template=tpl, planted=[(tx, ty)],
    ))

    # 7. 小さく高周波なテンプレート。UI アイコン相当。
    base = make_ui_scene(320, 240, seed=7)
    icon = make_icon(16, seed=107)
    base = plant(base, icon, [(200, 70)])
    cases.append(dict(
        name="small_icon",
        note="16x16 の小テンプレート。誤検出しやすい条件。",
        scene=base, template=icon, planted=[(200, 70)],
    ))

    # 8. 大きめ。フルマップは保存せず要約のみ。
    base = make_ui_scene(1280, 720, seed=8)
    tx, ty = pick_textured_patch(base, 96, 96, seed=108)
    tpl = base[ty:ty + 96, tx:tx + 96].copy()
    cases.append(dict(
        name="hd_large",
        note="1280x720 / 96x96。フルマップは保存せず極値とサンプル点のみ突き合わせる。",
        scene=base, template=tpl, planted=[(tx, ty)],
    ))

    return cases


# --------------------------------------------------------------------------
# 期待値の算出と書き出し
# --------------------------------------------------------------------------

def sample_indices(n: int, seed: int) -> np.ndarray:
    """マップ全体から決定的に n 点選ぶ。両端は必ず含める。"""
    count = min(NUM_SAMPLES, n)
    idx = _rng(seed).choice(n, size=count, replace=False)
    idx = np.unique(np.concatenate([idx, np.array([0, n - 1])]))
    return np.sort(idx)


def zmd_expected(s: np.ndarray, t: np.ndarray) -> np.ndarray:
    """ZMD スコアマップを計算する。

    分子 sum(I'·T') は OpenCV `TM_CCOEFF`（DFT 相互相関、窓・テンプレート両方の
    平均を引いたもの）から取り、分母の窓側エネルギー dev2 とテンプレート側
    エネルギー tn2 は numpy の積分画像（f64）で独立に計算する。

    分母は dev2 + tn2 >= tn2 > 0（一様テンプレートを除く）なので 0/0 は無い。
    一様テンプレート（tn2 = 0）では Rust 側が全面 0 を返す仕様に合わせ、
    分母 0 の位置は 0 とする。
    """
    num = cv2.matchTemplate(s, t, cv2.TM_CCOEFF).astype(np.float64)

    th, tw = t.shape
    n = float(tw * th)
    tc = t.astype(np.float64) - t.astype(np.float64).mean()
    tn2 = float((tc * tc).sum())

    # 一様テンプレートは Rust 側（FLAT_TEMPLATE_EPS）が全面 0 を返す。合わせる。
    # ここで弾かないと、tn2=0 かつ dev2~0 の位置で TM_CCOEFF の丸め雑音を
    # 極小の分母で割ることになり、期待値が ±1 に化ける。
    if tn2 < 1e-12:
        return np.zeros(num.shape, dtype=np.float32)

    integral, integral_sq = cv2.integral2(s.astype(np.float64))

    def window_sum(acc: np.ndarray) -> np.ndarray:
        return acc[th:, tw:] - acc[:-th, tw:] - acc[th:, :-tw] + acc[:-th, :-tw]

    dev2 = np.maximum(window_sum(integral_sq) - window_sum(integral) ** 2 / n, 0.0)

    den = dev2 + tn2
    out = np.divide(2.0 * num, den, out=np.zeros_like(num), where=den > 0.0)
    return np.clip(out, -1.0, 1.0).astype(np.float32)


def compute_expected(scene: np.ndarray, template: np.ndarray, method_key: str) -> dict:
    # OpenCV も Rust も 0.0..1.0 の f32 で計算する。ZMD は輝度オフセットには
    # 不変だがスケールには依存する（SSD も同様）ため、/255 を両者で揃えておく。
    s = scene.astype(np.float32) / 255.0
    t = template.astype(np.float32) / 255.0
    result = zmd_expected(s, t) if method_key == "zmd" else cv2.matchTemplate(s, t, cv2.TM_SQDIFF)

    flat = result.reshape(-1)
    min_v, max_v, min_loc, max_loc = cv2.minMaxLoc(result)
    idx = sample_indices(flat.size, seed=hash_seed(method_key))

    return {
        "result_size": [int(result.shape[1]), int(result.shape[0])],
        "min": float(min_v),
        "max": float(max_v),
        "min_loc": [int(min_loc[0]), int(min_loc[1])],
        "max_loc": [int(max_loc[0]), int(max_loc[1])],
        "samples": {
            "indices": [int(i) for i in idx],
            "values": [float(flat[i]) for i in idx],
        },
        "_full": result,
    }


class FixtureError(RuntimeError):
    """フィクスチャ自体が想定を満たしていない（テストの前提が壊れている）。"""


def validate_case(entry: dict, full: np.ndarray, exp: dict, min_peak: float) -> None:
    """生成したフィクスチャが「テストとして意味を持つ」かをその場で確かめる。

    平坦なパッチをテンプレートにすると背景のどこにでも高スコアが出てしまい、
    ピークが植え込み位置に来ない。それを黙って通すと Rust 側のテストが
    「何も検証していないのに緑」になるので、生成時点で落とす。

    `min_peak` はケースごとの下限。等コントラストの一致は 0.98 以上だが、
    brightness ケースは意図的にコントラストを変えており、ZMD は p(g) の分だけ
    下がる（0.65 倍で 0.913）。
    """
    planted = entry["planted"]
    if not planted:
        return

    max_loc = tuple(exp["max_loc"])
    if max_loc not in {tuple(p) for p in planted}:
        raise FixtureError(
            f"{entry['name']}: ZMD のピーク {max_loc} が植え込み位置 {planted} に無い。"
            " テンプレートが平坦すぎて偶然の一致が起きている可能性が高い"
        )

    if exp["max"] < min_peak:
        raise FixtureError(f"{entry['name']}: ピークスコア {exp['max']:.4f} が低すぎる")

    # 複数植え込みのケースは、全箇所が高スコアで出ていないと NMS の検証にならない。
    for x, y in planted:
        score = float(full[y, x])
        if score < min_peak:
            raise FixtureError(
                f"{entry['name']}: 植え込み位置 ({x}, {y}) のスコアが {score:.4f} しかない"
            )


def hash_seed(text: str) -> int:
    return int(hashlib.sha256(text.encode()).hexdigest()[:8], 16)


def write_f32(path: Path, arr: np.ndarray) -> str:
    data = arr.astype("<f4").tobytes()
    path.write_bytes(data)
    return hashlib.sha256(data).hexdigest()


def generate() -> dict:
    for sub in ("scenes", "templates", "expected"):
        (OUT_ROOT / sub).mkdir(parents=True, exist_ok=True)

    manifest = {
        "generator": "tools/golden/gen_golden.py",
        "opencv_version": cv2.__version__,
        "numpy_version": np.__version__,
        "pixel_scale": "uint8 / 255.0 -> f32",
        "tolerance": {
            "comment": (
                "GPU は f32、期待値は TM_CCOEFF(f32 DFT) + 積分画像(f64)。"
                "ZMD は分母が dev2 + tn2 >= tn2 > 0 で下から支えられるため、"
                "NCC 時代の分母依存の許容誤差や 0/0 位置の除外は不要。一律の絶対誤差で比較する。"
            ),
            # 一律の絶対誤差。実測最大の約 10 倍を上限にしている。
            # NCC 時代（f64 vs oracle 2.2e-4 / GPU f32 4.1e-4）から 2 桁締まったのは、
            # 誤差が分母（NCC では 0 に近づきうる）で増幅されなくなったため。
            #   zmd_abs            実測最大 5.4e-6 (flat_region)
            #   zmd_gpu_abs        実測最大 5.4e-6 (flat_region)
            #   zmd_gpu_vs_cpu_abs 実測最大 1.5e-6 (hd_large)
            # 再生成時に Rust テストの出力（最大差）と突き合わせて維持すること。
            "zmd_abs": 5.0e-5,
            "zmd_gpu_abs": 5.0e-5,
            "zmd_gpu_vs_cpu_abs": 2.0e-5,
            "ssd_rel": 1.0e-4,
            "ssd_abs": 1.0e-4,
        },
        "cases": [],
    }

    for case in build_cases():
        name = case["name"]
        scene: np.ndarray = case["scene"]
        template: np.ndarray = case["template"]

        scene_path = OUT_ROOT / "scenes" / f"{name}.png"
        tpl_path = OUT_ROOT / "templates" / f"{name}.png"
        cv2.imwrite(str(scene_path), scene)
        cv2.imwrite(str(tpl_path), template)

        # PNG に落としてから読み直す。Rust が読むのと同じ画素値で期待値を作るため。
        scene = cv2.imread(str(scene_path), cv2.IMREAD_GRAYSCALE)
        template = cv2.imread(str(tpl_path), cv2.IMREAD_GRAYSCALE)

        entry = {
            "name": name,
            "note": case["note"],
            "scene": f"scenes/{name}.png",
            "template": f"templates/{name}.png",
            "scene_size": [int(scene.shape[1]), int(scene.shape[0])],
            "template_size": [int(template.shape[1]), int(template.shape[0])],
            "planted": [[int(x), int(y)] for x, y in case["planted"]],
            "methods": {},
        }

        for method_key in METHODS:
            exp = compute_expected(scene, template, method_key)
            full = exp.pop("_full")

            if method_key == "zmd":
                validate_case(entry, full, exp, case.get("min_peak", 0.98))

            # ZMD はスコア意味論の要なのでフルマップを保存する。SSD は要約とサンプルのみ。
            if method_key == "zmd" and full.size <= MAX_FULL_MAP_ELEMS:
                rel = f"expected/{name}_{method_key}.f32"
                digest = write_f32(OUT_ROOT / rel, full.reshape(-1))
                exp["full_map"] = rel
                exp["full_map_sha256"] = digest
            else:
                exp["full_map"] = None

            entry["methods"][method_key] = exp

        manifest["cases"].append(entry)

    (OUT_ROOT / "manifest.json").write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    return manifest


def check() -> int:
    """既存のゴールデンを OpenCV で再計算して照合する（再生成はしない）。"""
    manifest_path = OUT_ROOT / "manifest.json"
    if not manifest_path.exists():
        print(f"manifest が無い: {manifest_path}", file=sys.stderr)
        return 1

    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    failures = 0

    for case in manifest["cases"]:
        scene = cv2.imread(str(OUT_ROOT / case["scene"]), cv2.IMREAD_GRAYSCALE)
        template = cv2.imread(str(OUT_ROOT / case["template"]), cv2.IMREAD_GRAYSCALE)
        for method_key, exp in case["methods"].items():
            got = compute_expected(scene, template, method_key)
            full = got.pop("_full")
            if abs(got["max"] - exp["max"]) > 1e-6 or got["max_loc"] != exp["max_loc"]:
                print(f"NG {case['name']}/{method_key}: 極値が不一致", file=sys.stderr)
                failures += 1
            if exp.get("full_map"):
                stored = np.frombuffer((OUT_ROOT / exp["full_map"]).read_bytes(), dtype="<f4")
                if not np.allclose(stored, full.reshape(-1), atol=1e-7):
                    print(f"NG {case['name']}/{method_key}: フルマップが不一致", file=sys.stderr)
                    failures += 1

    if failures:
        print(f"{failures} 件の不一致", file=sys.stderr)
        return 1
    print("ゴールデンは OpenCV と一致している")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="再生成せず既存ゴールデンを検証する")
    args = parser.parse_args()

    if args.check:
        return check()

    manifest = generate()
    total_bytes = sum(p.stat().st_size for p in OUT_ROOT.rglob("*") if p.is_file())
    print(f"OpenCV {cv2.__version__} / numpy {np.__version__}")
    print(f"{len(manifest['cases'])} ケースを {OUT_ROOT} に生成 ({total_bytes / 1024:.0f} KiB)")
    for case in manifest["cases"]:
        zmd = case["methods"]["zmd"]
        print(
            f"  {case['name']:<14} scene={case['scene_size'][0]}x{case['scene_size'][1]}"
            f" tpl={case['template_size'][0]}x{case['template_size'][1]}"
            f" zmd_max={zmd['max']:+.6f} @ {tuple(zmd['max_loc'])}"
            f" zmd_min={zmd['min']:+.6f}"
            f" full_map={'yes' if zmd['full_map'] else 'no'}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
