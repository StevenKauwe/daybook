#!/usr/bin/env python3
"""Compare full-frame and changed-line Vision OCR on a generated desktop sequence."""
import json
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

try:
    from PIL import Image, ImageChops, ImageDraw, ImageFont
except ModuleNotFoundError:
    raise SystemExit("Install Pillow in your Python environment to run the changed-line benchmark.")

from benchmark_ocr import HELPER


def make_sequence(folder):
    first = [
        "Morning planning session", "Review the budget proposal", "Email the design team",
        "Prepare the weekly update", "Confirm the meeting room", "Delivery date November 18",
        "Read the field notes", "Check the revised schedule", "Approve the travel request",
        "Publish the final draft",
    ]
    appended = first + ["Add the shipping label"]
    edited = appended.copy()
    edited[5] = "Delivery date November 19"
    scrolled = edited[4:] + ["Meet the new supplier"]
    states = [first, first, appended, appended, edited, edited, scrolled, scrolled]
    expected_lines = list(dict.fromkeys(line for state in states for line in state))
    font = ImageFont.truetype("/System/Library/Fonts/Supplemental/Arial.ttf", 31)
    full_frames, changed_frames = [], []
    previous = None
    for index, lines in enumerate(states):
        frame = Image.new("RGB", (1920, 1080), "white")
        draw = ImageDraw.Draw(frame)
        for row, line in enumerate(lines):
            draw.text((84, 90 + 63 * row), line, font=font, fill="black")
        original = folder / f"frame-{index}.png"
        frame.save(original)
        full_frames.append(original)
        if previous is None:
            changed_frames.append(original)
        else:
            box = ImageChops.difference(frame, previous).getbbox()
            if box:
                top, bottom = max(0, box[1] - 24), min(frame.height, box[3] + 24)
                if bottom - top > frame.height * 0.35:
                    changed_frames.append(original)
                else:
                    strip = folder / f"strip-{index}.png"
                    frame.crop((0, top, frame.width, bottom)).save(strip)
                    changed_frames.append(strip)
        previous = frame
    return full_frames, changed_frames, expected_lines


def read_lines(paths):
    distinct = {}
    elapsed = 0.0
    for path in paths:
        start = time.perf_counter()
        run = subprocess.run([str(HELPER), "--ocr-file", str(path)], capture_output=True,
                             text=True, timeout=120)
        elapsed += time.perf_counter() - start
        if run.returncode:
            raise SystemExit(f"Couldn’t read {path.name}. {run.stderr.strip() or run.stdout.strip()}")
        for line in json.loads(run.stdout)["text"].splitlines():
            words = " ".join(line.split())
            if words:
                distinct[words] = None
    return list(distinct), elapsed


def measure(folder):
    full, changed, expected = make_sequence(folder)
    paths = {"full": full, "changed": changed}
    seconds = {mode: [] for mode in paths}
    for round_number in range(3):
        for mode in (("full", "changed") if round_number % 2 == 0 else ("changed", "full")):
            observed, elapsed = read_lines(paths[mode])
            if observed != expected:
                raise SystemExit(f"{mode} OCR differed from the generated text.\n"
                                 f"Expected {expected}\nRecognized {observed}")
            seconds[mode].append(elapsed)
    print(f"Both methods recovered all {len(expected)} distinct lines in order.")
    for mode, inputs in paths.items():
        median = statistics.median(seconds[mode])
        print(f"{mode}: {len(inputs)} OCR calls across {len(full)} frames, median {median:.3f} s")


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="daybook-changed-ocr-") as directory:
        measure(Path(directory))
