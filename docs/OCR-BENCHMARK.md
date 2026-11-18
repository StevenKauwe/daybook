# What does it cost to read a screenshot?

On an Apple M2 Max with 32 GiB of memory running macOS 27.2, Daybook read a 144-word page in 236–304 ms. Going from 720p to 4K raised peak memory from 78 to 143 MiB. More pixels used more memory, while elapsed time varied with the amount of text and between runs.

## Results

Each row shows medians from three runs on generated PNGs. **OCR helper time** includes launching Daybook’s OCR helper, decoding the image, recognizing text and returning it. **Peak memory** is the helper’s maximum resident memory. The helper exits after each save. Screen selection and journal writes are outside the measurement.

| Image | Recognized words | OCR helper time | Peak memory | First OCR on decoded image | Repeat OCR on decoded image |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1280 × 720 | 144 | 235.9 ms | 77.9 MiB | 212.5 ms | 119.4 ms |
| 1920 × 1080 | 144 | 303.9 ms | 86.5 MiB | 241.6 ms | 132.6 ms |
| 3840 × 2160 | 144 | 264.6 ms | 143.0 MiB | 230.5 ms | 131.4 ms |
| 1920 × 1080 | 0 | 111.2 ms | 69.8 MiB | 84.1 ms | 26.2 ms |
| 1920 × 1080 | 36 | 211.7 ms | 84.0 MiB | 190.1 ms | 100.9 ms |
| 1920 × 1080 | 240 | 267.6 ms | 92.5 MiB | 244.7 ms | 144.9 ms |

At 1080p, the blank page took 111 ms and the page with 240 words took 268 ms. The 4K page happened to finish faster than the 1080p page with the same 144 words. A separate check using the helper from the signed app bundle measured 254 ms and 91 MiB for the 144-word 1080p page. Hardware, system load, shared caches, typography, layout and languages can change the result.

## Run it on another Mac

From the Daybook checkout, run `just benchmark-ocr`. The command builds the release helper, generates six temporary PNGs and runs the on-device Apple Vision recognizer Daybook uses for saved images. The benchmark uses generated pages for every input.

The resolution cases draw the same twelve lines at the same proportions on each canvas. The density cases vary the line count at 1080p. Each round shuffles the cases. Python measures elapsed wall time around a fresh helper process and reads its peak resident memory from macOS `/usr/bin/time -l`. This measures the helper process only. The other Daybook processes and system-wide accelerator allocations are outside that figure.

For the last two columns, a separate helper decodes the same PNG and calls Vision four times. The first column of OCR timings uses the first call in each process. The repeat column uses the nine subsequent calls across the three processes. The script also reports the change in that process’s memory high-water mark after loading the image. [Benchmark script](../scripts/benchmark_ocr.py)
