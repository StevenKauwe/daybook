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

To repeat the changed-region experiment below, activate a Python environment with Pillow installed and run `just benchmark-ocr-changes`. It generates all eight frames, checks the recovered lines against their original text, and reports the time spent reading them. [Experiment script](../scripts/benchmark_changed_ocr.py)

The resolution cases draw the same twelve lines at the same proportions on each canvas. The density cases vary the line count at 1080p. Each round shuffles the cases. Python measures elapsed wall time around a fresh helper process and reads its peak resident memory from macOS `/usr/bin/time -l`. This measures the helper process only. The other Daybook processes and system-wide accelerator allocations are outside that figure.

For the last two columns, a separate helper decodes the same PNG and calls Vision four times. The first column of OCR timings uses the first call in each process. The repeat column uses the nine subsequent calls across the three processes. The script also reports the change in that process’s memory high-water mark after loading the image. [Benchmark script](../scripts/benchmark_ocr.py)

## Could Daybook read only what changed?

Yes. Daybook’s periodic capture currently takes a 1280-pixel-wide screenshot and compares a tiny whole-screen thumbnail. A small edit can disappear in that average. An opt-in screen stream could use ScreenCaptureKit’s per-frame changed rectangles and idle status. Apple Vision can recognize text in a specified region. These APIs are available on Daybook’s supported Macs. [ScreenCaptureKit frame changes](https://developer.apple.com/documentation/screencapturekit/scstreamframeinfo/dirtyrects) · [Idle frames](https://developer.apple.com/documentation/screencapturekit/scframestatus/idle) · [Vision regions of interest](https://developer.apple.com/documentation/vision/vnimagebasedrequest/regionofinterest)

Vision also reports where recognized words appear. Periodic OCR already saves each available word box as X, Y, width and height on a 0–1 scale from the image’s lower-left corner, up to 1,000 boxes. Manually saved screenshots and clipboard images currently keep the original image and plain text. For a stream, a word’s position and its neighbors can help identify a line that moved during a scroll. [Apple’s text recognition guide](https://developer.apple.com/documentation/vision/recognizing-text-in-images)

On the M2 Max, an eight-frame, 1920 × 1080 synthetic sequence contained a page, an appended line, a date changed from November 18 to 19, and a scroll that brought another line into view. Each state appeared twice. Both approaches recovered the same 13 distinct lines in order, including both dates.

| Approach | OCR calls | Median time in the OCR helper across eight frames |
| --- | ---: | ---: |
| Read every full frame | 8 full frames | 1.64 s |
| Read changed content | 2 full frames and 2 horizontal strips | 0.70 s |

The experiment compared adjacent PNGs, skipped exact repeats, and widened small pixel changes to a full line with 24 pixels of vertical padding. A change taller than 35% of the screen triggered a full read. The first frame and the scroll took that path. Timings are medians of three passes with alternating run order. They include launching the normal OCR helper, decoding and recognition. Frame acquisition, difference calculation, cropping and battery use were outside the measurement.

A practical first version would keep one OCR job in flight, combine all changed regions while it runs, and read after typing or scrolling settles. A long scroll, lost frames, a new capture target or a scale change would trigger a fresh baseline. ScreenCaptureKit reports changes relative to the preceding frame, so dropping a frame also means keeping its dirty regions or doing a full read. [Apple’s explanation of frame metadata](https://developer.apple.com/videos/play/wwdc2022/10155/?time=814)

For the timeline, match lines within a recent window of the same capture target. Use text, nearby lines and positions to recognize a paragraph that moved during scrolling. Collapse whitespace and keep numbers, punctuation and case so an edit like **$120 → $125** survives. Keep first and last seen times for repeated content. A revisit after switching windows and an explicitly saved screenshot each deserve their own moment. The next feasibility check is a real work session with a terminal, a scrolling document and moving video, measuring missed edits, duplicate entries and energy use. Streaming OCR is a proposal.

The prototype models a screen check every two seconds. A line seen on six consecutive checks appears once, with its first and last observed times. A changed line, a different window or a gap in sampling ends that run at its last confirmed sighting. If the line returns, it starts a new run. A clipboard paste has a single timestamp. The current recorder saves one timestamp per periodic screen observation. It defaults to five-minute checks and accepts intervals down to 30 seconds. [Try the parallel transcript study](transcript-views.html#parallel-transcript)
