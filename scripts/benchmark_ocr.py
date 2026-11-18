#!/usr/bin/env python3
"""Measure Daybook's accurate Vision text extraction using only generated pages."""
import json
from pathlib import Path
import random
import re
import statistics
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
HELPER = ROOT / "native-rust/target/release/daybook-native"
CASES = [(1280, 720, 12), (1920, 1080, 12), (3840, 2160, 12),
         (1920, 1080, 0), (1920, 1080, 3), (1920, 1080, 20)]


def measure(folder):
    results = {case: [] for case in CASES}
    rng = random.Random(37)
    files = {}
    for case in CASES:
        image = folder / ("%dx%d-%d.png" % case)
        subprocess.run([str(HELPER), "--make-ocr-fixture", *map(str, case), str(image)], check=True)
        files[case] = str(image)
    for _ in range(3):
        order = CASES.copy()
        rng.shuffle(order)
        for case in order:
            # Time the very same one-shot process that handles an actual saved PNG.
            begin = time.perf_counter()
            once = subprocess.run(["/usr/bin/time", "-l", str(HELPER), "--ocr-file", files[case]],
                                  capture_output=True, text=True, timeout=120)
            elapsed = (time.perf_counter() - begin) * 1000
            if once.returncode:
                raise SystemExit(once.stdout + once.stderr)
            peak = re.search(r"(?m)^\s*(\d+)\s+maximum resident set size", once.stderr)
            if not peak or (case[2] and len(json.loads(once.stdout)["text"].split()) < case[2]):
                raise SystemExit("Incomplete resource measurement or missing fixture text")
            command = [str(HELPER), "--benchmark-ocr", files[case], "4"]
            run = subprocess.run(command, capture_output=True, text=True, timeout=120)
            if run.returncode:
                raise SystemExit(run.stdout + run.stderr)
            result = json.loads(run.stdout)
            result.update(one_shot_ms=elapsed, one_shot_peak=int(peak[1]))
            results[case].append(result)
    print("Pixels | Lines | Words | One-shot ms | One-shot peak MiB | First Vision ms | Warm Vision ms | Repeated peak rise MiB")
    for (width, height, lines), runs in results.items():
        first = statistics.median(r["ms"][0] for r in runs)
        warm = statistics.median(t for r in runs for t in r["ms"][1:])
        one_shot = statistics.median(r["one_shot_ms"] for r in runs)
        peak = statistics.median(r["one_shot_peak"] for r in runs) / 2**20
        rise = statistics.median(r["peak_rss"] - r["before_peak_rss"] for r in runs) / 2**20
        words = statistics.median(r["words"] for r in runs)
        print(f"{width}×{height} | {lines} | {words} | {one_shot:.1f} | {peak:.1f} | {first:.1f} | {warm:.1f} | {rise:.1f}")


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="daybook-ocr-bench-") as directory:
        measure(Path(directory))
