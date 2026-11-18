# Daybook

Daybook is a local workday journal for macOS. It transcribes microphone and system audio, reads text from periodic screenshots, and saves clipboard notes and screenshots you choose. Search them together in the Archive. A menu-bar indicator shows when the recorder is active.

## Install on a Mac

You need macOS 14 or later, Xcode Command Line Tools (including Git and Python 3), Rust 1.99 or later, and `just`. Apple Silicon is the tested architecture. If you use Homebrew, `brew install rust just` supplies the last two tools. Install Apple's command-line tools with `xcode-select --install` if needed.

```sh
git clone https://github.com/StevenKauwe/daybook.git
cd daybook
just install
just open
```

The first install fetches a pinned Speechworks source snapshot, locked Rust dependencies and the English Moonshine Streaming Medium speech model (296 MB). The installer verifies the model's byte length and SHA-256 before putting it in `data/models/`. It installs the signed app at `~/Applications/Daybook.app` and adds a Dock shortcut. Each Mac keeps its own journal in the clone's ignored `data/` folder. Choose a permanent location for the clone.

Open **Settings → Permissions & app** in Daybook. Click **Allow access…** for **Microphone** and **Screen Recording & system audio**. Approve Daybook in **System Settings → Privacy & Security** on that Mac and reopen it if macOS requests it. Daybook checks both grants at Start, including when screen sampling is off, because system audio uses ScreenCaptureKit. The app opens stopped. Click **Start recording** when ready.

While recording, the default screen mode samples the first display every five minutes and uses Apple Vision to read text when the image changes. It keeps the recognized text. Saving images is optional. The archive contains individual observations and has no continuous video track.

## Save something while you work

Click **Daybook** in the Mac menu bar. Both actions work with recording stopped.

- **Save Clipboard to Daybook** saves copied text or a link as a note. If you copied an image, Daybook saves a full-resolution PNG and reads its text. Daybook reads the clipboard only when you click this action. If macOS asks whether Daybook can paste, choose Allow to save that item. [macOS pasteboard access](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum)
- **Save Screenshot…** opens the macOS screenshot picker. Drag to select an area, or press Space and click a window. Escape cancels. macOS may ask for Screen Recording permission on first use. A checkmark beside Daybook confirms the save.

Open **Daybook controls → Archive** and search for a word from the note or image. Entries are grouped by hour. The **Clipboard** and **Saved screenshots** filters narrow the list. Click an image entry to see the picture and extracted text together in Daybook. Use **Zoom in** to read small details or **Copy text** to reuse the words. **Show saved PNG in Finder** exports a copy to `data/saved-images/`.

To take part of a day with you, drag across the **Day timeline**, then click **Copy selection**. Click a single bar for a 15-minute interval or choose **Set times…** to enter exact times. The clipboard gets matching entries in chronological order with their times and sources. It includes complete passages that overlap the range, even across a page boundary. Images contribute their extracted text, or a timestamped entry when they have no text. A range ends at the selected **Until** time. Selections larger than 2 MiB prompt you to narrow the range.

Choose **Sessions** to browse conversations alongside the notes and screenshots saved around them. A 15-minute break starts a new session. Searching for a word on a screenshot finds its session. Open the session to see all its entries or use **Copy selection** to take them with you.

The journal keeps each manually saved image even when periodic screenshots are off or expire. Daybook stores the image before it reads the text with Apple Vision on your Mac. If recognition fails, open the entry and choose **Try reading text again**. Saved image pixels stay local. If you enable a hosted summary provider, clipboard notes and extracted text can be included in summaries. The storage limit applies to manual saves. Each save accepts up to 512 KiB of text or a 64 MiB image of up to 40 megapixels.

On an M2 Max, reading a generated page of 144 words took 0.24–0.30 seconds across 720p, 1080p and 4K. Peak memory for the short-lived OCR process ranged from 78 to 143 MiB. [Benchmark results and instructions](docs/OCR-BENCHMARK.md)

## Running Daybook

**Closing or minimizing the window leaves recording and the menu bar running.** Click **○ Daybook** in the menu bar, then **Open Daybook controls** to return. The header has one Start/Stop recording control. The ••• menu includes Update & Restart and Quit. To run the build from a terminal, use the executable in the bundle.

```sh
./dist/Daybook.app/Contents/MacOS/daybook
# Optional separate data directory:
./dist/Daybook.app/Contents/MacOS/daybook --data /absolute/private/directory
```

The build records the checkout path. If you move the checkout, rebuild and check the saved model path in Settings. Fresh configurations prefer a cached Streaming Medium model, then Streaming Small, then Tiny. Existing installations keep their saved model selection. You can choose a local model path and Auto or CPU inference in Settings while stopped. Daybook writes its private per-launch loopback control URL to `data/control-url.txt`. Its backend enforces loopback binding, Host/Origin checks and JSON POSTs. Only one instance may own a data directory.

**Start recording now** explicitly records outside work hours too. **Enable workday auto** follows the configured work hours (default weekdays, 08:00–18:00 local time). Both suspend on lock/sleep and can resume on unlock/wake while still enabled. Wake does not clear the separate lock flag. **Stop disarms recording until another explicit Start/Enable action.** The redundant Pause control has been removed; the legacy API command remains a compatible stop action. Process restart always opens stopped, even after a crash; no launch item is installed. Stop flushes the current chunk but existing saved work can finish processing. Quit exits the capture helper and backend.

Daybook uses native macOS permission prompts and the system's existing input and output devices. macOS may request access again after rebuilding with an ad-hoc signature.

## Settings and models

Open **Settings** in the native window. Controls cover model path, meeting-app bundle ID, work hours, sampling, compression, media retention, resources, independent providers and budgets. **Validate & save** checks the configuration atomically. Stop first; saving reloads the stopped recorder without closing the window. No credentials belong in that JSON.

- **Audio:** separate microphone `me` and ScreenCaptureKit `remote` tracks. Empty meeting bundle ID records system audio; an explicit ID filters to that running application. This is track attribution, not diarization. The default `audio_storage: "memory"` sends mono 16 kHz audio in half-second blocks directly through a bounded local pipe/RAM queue to Speechworks. No audio or audio sidecar files are saved in this mode. Text, source timing and provenance are durable in SQLite; raw PCM/base64 is never stored there. One inference worker interleaves independent bounded streaming sessions for both tracks. Streaming Small/Medium reuse encoder state and publish revisable previews; Tiny/Base finalize buffered phrases. Speechworks Silero VAD runs independently on both tracks, retaining up to one second of preroll and finalizing phrases after about 0.75 seconds of detected quiet, Stop/inactivity, session/date changes or discontinuities. A twenty-second safety cap bounds each inference phrase; it is no longer cut every eight seconds. Silence without detected speech is explicitly classified and released without calling the recognizer. Finalized audio is discarded immediately. Source wall-clock times are approximate; word alignment and diarization are not implemented. VAD is probabilistic: it can miss very quiet speech or admit noise/music, and uninterrupted speech can still cross the twenty-second safety boundary. A crash loses unfinished RAM audio; admitted unfinished blocks are marked lost on restart, while finalized text survives. The unadmitted tail in native/IPC queues cannot be counted exactly after a crash. Select **Disk recovery** to save temporary half-second PCM/metadata files and replay unfinished sources after restart. Legacy files up to ten seconds remain supported. Abandoned previews are labeled and excluded from summaries.
- **Speechworks** handles inference and audio resampling. Auto selects hardware acceleration with CPU fallback. Health reports the loaded model, device, API and any fallback reason. The build pins the source and applies a two-thread CPU cap and a recovery-loader patch. The upstream notices cover Speechworks, its dependencies and the Moonshine model. [Third-party notices](https://github.com/StevenKauwe/speechworks/blob/8d2690a99ab849d30b9d0fa70f6fda84883b2cef/THIRD_PARTY_NOTICES.md)
- **Periodic screenshots** sample by default every 300 seconds on the first display at 1280 pixels wide. A 64×36 pixel comparison skips OCR/vision when mean absolute channel difference is under 3. This is a simple heuristic, not guaranteed semantic change detection. Native Vision OCR returns text and normalized word boxes (origin bottom-left). No continuous video or frame archive. Modes are `ocr`, `vision`, `both`, and `off`. Screens sample the whole first display even when audio is filtered to a meeting app.
- **Summaries:** checkpointed incremental jobs every 900 seconds when new transcript/screen/note observations exist; bounded batches, original source IDs and text retained. Large inputs are split at UTF-8 boundaries into durable parts of at most 64,000 bytes, retaining source IDs and byte offsets; completed parts are reused after retry/crash. Each provider attempt still consumes budget. Stopped normal sessions also drain final material when resources allow. Empty summary model uses a bounded, explicitly labeled **heuristic extractive summary**, prioritizing lines with action/decision terms, up to 12 excerpts. It cannot provide a model's semantic synthesis. Manual meta-compression creates a separate summary layer from new incremental summaries in bounded batches; it does not replace originals. The UI reports accumulated summary characters and warns above the configured threshold. Daily text exports live at `data/YYYY-MM-DD.txt` and include summary ranges. Daily grouping follows source timestamps, not job creation/completion times. New incremental jobs stop at source-date boundaries. Late vision preserves the original screenshot time. Existing mixed-day summaries and meta summaries appear on every referenced date with explicit cross-day labels; their text cannot be truthfully split after the fact. Durable dirty-date checkpoints and a one-time migration repair older exports. Previews block the summary cursor until finalized or abandoned so a later note cannot cause an earlier transcript to be skipped. Without an LLM, excerpts may omit important context: use the searchable timeline.
- **Providers:** independently configure `summary` and `vision` using OpenAI-compatible `/chat/completions`. A local llama.cpp server can use `http://127.0.0.1:8080/v1`. Set its actual model name. Only literal loopback endpoints are local; hosted endpoints require HTTPS and explicit `remote_opt_in: true` for that tool. Summary transmits selected text; vision transmits selected JPEGs. Image support must be explicitly declared. API redirects are disabled. Request timeout is 45 seconds, input ≤64,000 bytes, image ≤2 MiB, response ≤256 KiB, output text ≤64,000 bytes.
- **Keys:** use Keychain Access to create a generic password with service `local.daybook.provider`, then put its account name in `keychain_account`. Credentials are read at request time using macOS `security`, never stored in the repo/config or logged. Keychain prompting and actual hosted-provider compatibility were not tested.
- **Budgets:** persisted daily request/image limits and conservative token reservations across both tools, including failed requests. Default 100 requests, 24 images, 100,000 reserved units/day. Reservations use UTF-8 input bytes + output allowance + overhead; images add 8192. These are measurable guardrails, **not accurate billing/tokenizer estimates**. Request/image caps provide independent protection. A crash after sending but before committing a result may resend a checkpointed request; every attempt consumes another reservation. No unlimited retries; summaries back off five minutes; failed vision attempts are reported and not auto-retried.

## Retention and resource behavior

Default audio storage is RAM-only, not an audio archive. The rolling window (600 seconds by default) is an expiry ceiling for unfinished audio; the separate byte/block capacity may run out earlier. A finalized phrase releases its PCM immediately. Expired pending audio is marked `expired` with a visible **BACKLOG EXPIRED** event. A full RAM queue preserves admitted audio, marks rejected blocks lost with timing/reason, and continues capture. Processing resumes automatically when space returns; the affected recognition track gets a new session so words are not joined across the gap. The warning stays visible and loss evidence is durable. In **Disk recovery** mode, completed PCM remains for the rolling window; optional `retain_audio_hours > 0` converts it to 32 kbps AAC with built-in `afconvert`. Conversion failures retain PCM until expiry and report a warning. Longer retention requires recovery mode; existing explicit longer-retention settings are preserved on migration. Legacy configurations with zero retained hours adopt the new RAM default. Source IDs, timing and statuses remain after audio is discarded. Retained disk audio is available through authenticated local `/source/ID`; RAM audio is not downloadable. PCM is little-endian float32 mono 16 kHz; AAC is playable M4A.

Periodic OCR samples discard their image unless `retain_images` is enabled. Vision needs a temporary JPEG, deleted after its attempt unless explicitly retained. Retained periodic images expire by `image_hours`. Manually saved screenshots and clipboard images stay in the journal. Summaries, raw observations, and media have distinct retention; text is not silently deleted. The 1 GiB default data cap pauses recording rather than deleting text. RAM queue overload recovers automatically; the disk cap and optional disk-recovery backlog limit still require intervention. Indefinite operation therefore requires adequate configured storage: text/provenance accumulate, and the app never silently deletes them to make space. It includes models under the data directory. Media deletion is normal filesystem deletion, not secure erasure; use FileVault/OS access controls for storage encryption.

The native queue admits up to five seconds of PCM per track, at most 8 MiB allocated PCM and 2,048 packets combined, including the block being processed. Each callback remains capped at 32,768 mono floats; callback-local buffers and resampler state are additional. Short consumer/pipe stalls are absorbed. If a limit is reached, newly arriving audio is counted as lost without disarming capture; admitted audio is preserved, the next successful block starts a new recognition session, and a durable gap warning reports the loss. Invalid formats or a disconnected consumer still stop with an explicit error. Resampled blocks are at most 8,000 frames / 32,000 bytes. The backend RAM inbox admits at most `max_pending` blocks (default 120), at most 3,840,000 PCM bytes combined for both tracks, equivalent to 60 seconds of mono audio or 30 seconds of simultaneous two-track audio. In-progress phrase blocks count toward this cap. Partial blocks can fill the count cap sooner. Each of two inference sessions holds at most twenty seconds of PCM/context and 64 source IDs, normally finalizing at a detected speech pause. Each VAD also retains fewer than 512 residual samples and up to two preroll blocks / one second of PCM outside the inbox. The two Silero VAD models add fixed weights and recurrent state; they are CPU inference from the same pinned Speechworks snapshot. The native event channel has 64 entries; half-second base64 payloads add at most about 2.7 MiB plus metadata, pipe buffers and in-flight copies. The live UI reports the RAM inbox payload, not total app/model memory.

One dedicated worker handles inference and reuses its database connection; disk usage is scanned every five seconds. Active idle polling is 20 ms. A separate background worker handles retention, vision and compression, deferring new work above four unfinished audio blocks. HTTP model requests do not occupy the audio worker. OCR uses one utility task and defers for audio backlog or macOS memory pressure. Inference defaults to Auto (GPU with CPU fallback), a 256 MiB activation admission budget, ≤512 MiB model-file admission and a configured cooldown per ten seconds of audio scaled to block frames. Memory pressure can defer background jobs while transcription continues. These are soft resource targets, not whole-process RSS/CPU limits: model state, GPU/framework caches and allocations add overhead. Disk checks can briefly overshoot the cap by in-flight writes. Queue/expiry limits remain visible; neither full-day PCM nor full-day transcript is held in memory.

## OpenCode compatibility

The UI accepts pasted JSON/JSONC metadata in either current `providers.<id>.settings.baseURL` or V1 `provider.<id>.options.baseURL` format with a root `model: "provider/model"`. It copies endpoint/model and explicit image modality metadata only. It does not import `apiKey`, headers, auth files, OAuth tokens, `{env:...}`, or `{file:...}`. It does not implement automatic multi-file precedence/merging, live config references, or authentication reuse. Imported hosted settings stay disabled pending explicit opt-in. OpenCode's V1 `/provider` and `/config/providers` are configuration/catalog APIs, not proof of Chat Completions compatibility; Daybook does not route inference through them.

Verified documentation: [current config](https://opencode.ai/docs/config/), [current providers](https://opencode.ai/docs/providers), [V1 server](https://opencode.ai/v1/docs/server/). Framework reference: [Apple ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos).

## Build and test

Run the build once to prepare the pinned sources before invoking Cargo directly.

```sh
just build                         # single-job release build and local signing
RUSTC_WRAPPER= CARGO_BUILD_JOBS=1 cargo test --locked
RUSTC_WRAPPER= CARGO_BUILD_JOBS=1 cargo test --locked --manifest-path native-rust/Cargo.toml
RUSTC_WRAPPER= CARGO_BUILD_JOBS=1 cargo test --locked --manifest-path ui-rust/Cargo.toml
python3 -m unittest discover -s tests -p '*_test.py'
python3 tests/integration.py       # synthetic localhost server/mock provider; no native capture
python3 tests/saved_images.py      # private test clipboard, saved PNG recovery and Archive search
python3 tests/disk_bounds.py       # synthetic logical disk cap and restart
python3 tests/native_lifecycle.py  # synthetic native window-process lifecycle
python3 tests/native_smoke.py      # native startup/permission status only; never sends Start
./dist/Daybook.app/Contents/MacOS/daybook-native --self-test-state
./dist/Daybook.app/Contents/MacOS/daybook-native --self-test-audio /tmp/daybook-native-fixture
./dist/Daybook.app/Contents/MacOS/daybook-native --self-test-ocr
./dist/Daybook.app/Contents/MacOS/daybook-native --self-test-denied
```

For a speech check with your own fixture, run `python3 tests/model_smoke.py /path/to/model.gguf /path/to/fixture.wav` with a 16 kHz mono PCM16 WAV. Synthetic mode uses temporary data and bypasses memory-pressure deferral. The OCR self-test reads an image drawn in memory. A live desktop capture on a second Mac still needs hardware verification.

## App management pattern

A second launch focuses the existing controls. Closing the controls leaves the menu-bar recorder available. Quit closes the app. Run `git pull --ff-only` in the checkout to get newer source from GitHub. With a stable local signing identity, choose **••• → Update & Restart** from any page to build that source. Progress appears over the current page. Daybook finishes queued audio, verifies the replacement and reopens the same page with recording stopped. On an ad-hoc installation, quit Daybook and run `just install` again. The in-app updater attempts rollback if the successor fails to start.

### Streaming validation and limitations

The committed `StreamState` / `Model::transcribe_chunk` interface is integrated and verified with local public JFK audio. Independent sessions match one-shot transcription for the same bounded phrases; preview exclusion, source coverage, explicit flush, epoch changes and crash replay are tested. This is not a general accuracy benchmark, full-day soak, or live microphone/meeting validation. Reviewed streaming token `start_ms` values are still zero, so token fields are not usable word timestamps. There is no diarization.

Reproduce with `tests/streaming_smoke.py` after generating `/tmp/daybook-stream-fixture.f32` from the public WAV, or run the ignored real-model Rust test with `DAYBOOK_STREAM_MODEL` and `DAYBOOK_STREAM_PCM` pointing to existing local fixtures. No runtime model download or external telemetry is implemented.

## Native controls and local inspection

The native window has Timeline, Daily brief, Health and Settings views. Timeline supports date navigation, search, kind/source filters and chronological paging. Select an item to inspect its full text, exact summary checkpoint input/results, source IDs, capture metadata and retention state; retained media can be revealed in Finder. Notes keep a local draft across normal window closure and restart. Failed saves preserve edits. Settings remain a draft until explicitly saved while recording is stopped.

Health shows measured saved-audio frames/bytes/duration, queue age, callback queue snapshots, known rejected frames, local model activity, latest source-end-to-preview/final latency, scoped process CPU/peak RSS, provider requests and errors. Latency is not word-onset latency; VAD activity and explicitly filtered no-speech sources are reported locally; word alignment remains unavailable. OS/device losses, current whole-app memory/CPU and actual billed costs are unavailable rather than shown as zero. Provider token usage is provider-reported; optional user-supplied prices produce estimates, not invoices or a dollar spending cap. Request/image/reserved-unit limits remain enforced. Telemetry stays in local SQLite, with the latest 1,000 provider attempts and daily aggregates; the activity ledger excludes prompts, images, response text and credentials.

Each new audio file has a durable metadata sidecar containing capture timestamps, frame count, track and session. Crash recovery uses this metadata, including across midnight, and repairs missing database metadata after partial admission. Legacy files without metadata use measured PCM duration and filesystem modification time, explicitly marked approximate with unknown capture session. Malformed sidecars are reported and preserved until expiry. While capture is inactive, cleanup removes untracked `.f32`, `.tmp`, `.meta.tmp` and sidecar groups after the longer rolling/retained audio window. Every member must be old; database-linked groups, live writer PIDs, fresh sidecars and recent capture-end metadata protect the group. Cleanup emits an expiry event and removes at most 256 groups per pass; disk capacity can pause recording until safe cleanup is possible. Coarse timeline ranges round outward; exact timestamps remain available in source inspection.

Job inspection has independent First / Previous / Next controls for checkpoints and evidence references, with 100 rows per page. Stable part-number/source-ID cursors navigate the complete stored lists without growing an in-memory full-history list. Opening a source or checkpoint and using Back preserves the parent page. The API exposes each list’s current, next and previous cursor; additional rows remain explicit rather than silently omitted. Meta-summary references use the same navigation.

## Updating Speechworks from GitHub

`speechworks-source.json` records the Git repository, exact revision, tree, destination, CPU cap and patch digest. On first build, `scripts/vendor_speechworks.py --ensure` fetches and verifies that revision, applies the CPU cap and recovery-loader patch, and records a per-file manifest. Later builds check the manifest locally. Use `--verify` for an offline integrity check.

For a future update, review the upstream diff, change the revision/tree and choose a new destination in the pin file, run the vendor script, then point the root and native Cargo dependencies at the new snapshot. Update the reported revision, run unit and prerecorded streaming tests, and rebuild with `./build.sh`. If the upstream thread implementation changes, the patch fails closed for review. Keep prior snapshots until the update is verified. The active Speechworks working checkout is never used or modified by this workflow.

## Installation details

From this repository, run `just install`. It checks that Daybook is quit, verifies the pinned Speechworks source, builds and signs the app, installs it to `~/Applications/Daybook.app`, and adds or updates its Dock launcher. Repeating the command does not create duplicate launchers. It preserves other Dock entries and saves a Dock-preference backup under `data/install-backups/` before changing the app tiles. It refuses to overwrite an unrelated app or symlink at the destination. No sudo is required.

`just verify-install` verifies the installed signature and one Dock launcher. `just install-built` installs an already-built bundle and uses your existing model cache. Recording and permission requests are explicit actions in Daybook.

The Live transcription card shows both tracks’ latest provisional/final words and queued completion after Stop; it refreshes through a lightweight endpoint every 250 ms while recording or draining, and once per second while idle. Full health/overview refresh remains two seconds. Empty new previews retain the last available words until fresh text arrives. Transcription continues under macOS memory pressure. Existing disk/queue/retention bounds still apply, and OCR/provider work can defer. The card makes empty, blocked and expired states explicit. The installed icon is bundled in `assets/Daybook.icns`; `scripts/make_icon.swift` is only an optional developer asset generator, not a runtime app component.

On a new Mac the build uses ad-hoc signing. It requires no Apple developer account. For stable signing across rebuilds, put the name or hash of a usable code-signing identity from that Mac's Keychain in the ignored file `data/signing-identity.txt`. The private key stays in Keychain. `DAYBOOK_SIGN_IDENTITY` can override the selection for a build. An unusable configured identity stops the build.

The in-app updater builds offline using one Cargo job and the saved stable signing identity. Keep the checkout and Rust/build tools available. It never updates Speechworks automatically. Save or discard settings drafts before updating; the current note draft is saved with the request. Progress and the latest 8 KiB of build output appear in App updates; the full log is capped at 2 MiB in `data/update/build.log`. Concurrent updates and recording starts during an update are blocked. A build ID/time identifies the installed bundle. No automatic update or login task is installed.

## Using the transcript

Daybook opens on **Transcript**. The microphone and system audio each have a measured level history and a fast current-level meter. Hover over a trace for the measurement method, time window and dBFS. Silent, waiting, stale and stopped inputs have distinct states. The **Stop recording** button disarms the recorder and leaves your words available.

Passages from the same source join into readable paragraphs. **Writing…** marks text that Speechworks can still revise. Choose **Sources** on a paragraph to see its original passages, times and evidence. Scroll back to read while new text continues arriving. **Jump to live** returns to the latest words. Earlier and Later traverse bounded 80-passage pages, and **Browse archive** opens previous days.

Choose **Reading** for Paper or Night, journal serif or system sans, text size and Copy this page. Daybook remembers the reading preferences locally across launches. **Focus** (⌘⇧F) expands the conversation and keeps Stop within reach. **Quick note** (⌘N) opens a local draft. ⌘Return saves it to the archive. ⌘1 through ⌘5 switch between Transcript, Archive, Daily brief, Diagnostics and Settings. [Design notes](docs/DESIGN.md)

The configured Speechworks model prepares in the background when Daybook opens, without starting capture, and stays loaded across Stop/Start. After loading, Daybook releases the GGUF source buffer and keeps an open file descriptor and SHA-256 digest. If Metal fails, Auto reads and validates those same bytes to replay the current phrase on CPU. Loading failures are visible and retried at most once per minute (or when the model configuration changes). Actual backend and model-load duration are reported locally; macOS audio opening has a separate Starting state. Neither model readiness nor granted permissions replaces the explicit Start control.

Previously generated transcripts whose every source block was measured at the -120 dBFS signal floor are excluded from the reader and future summaries. Original output and provenance are preserved under **Archive → Suppressed audio**; missing legacy signal measurements are never treated as proof of silence.

## Responsive controls and meter validation

Microphone levels are measured in a meter-only native audio I/O callback. It scans borrowed float PCM without allocating, retaining audio, making Objective-C calls, or waiting for a lock; publication uses try_lock, and skipped meter updates are counted. The existing tap still supplies transcription audio. Unsupported meter formats fall back to the consumer meter. System levels run on the native audio consumer. A coalesced publisher retains two latest readings and one wake token; short peaks are preserved until delivery. The bridge stores this scalar telemetry in RAM and exposes authenticated localhost `/meters`, without database writes or transcript queries. The native audio blocks, source metadata, VAD and speech queues retain their existing sizes and behavior. Stop and new session epochs discard stale meter data. A dedicated UI observer uses a reusable HTTP connection and a single latest-result slot: 16 ms while active, 500 ms while stopped, 1 second while minimized. Closing the controls ends this observer. These are scheduling targets; callback timing and system load can slow delivery. Transcript/health refresh and meter polling are independent. User requests precede scheduled refreshes, and Quit saves the draft on the background worker instead of blocking the window.

`python3 tests/realtime_meters.py --ui` exercises generated native signals through the controls' HTTP feed and event-driven redraws. The microphone measurement method, window duration, callback size/rate and skipped meter callbacks are available via `/meters`. The level tooltip identifies the measurement method. `daybook-ui --design-preview /tmp/daybook-motion.png --motion` renders a synthetic animated window and writes a neighboring timing JSON file.


### Repeatable overload checks

Run `just test-resilience` for synthetic native-pipe stalls, accelerated queue lifecycle, RAM transcription overload/recovery, manual Stop, source-gap integrity and disk-cap protection. It never opens microphone/screen capture or calls hosted providers. Tests use one build job; logs are in `tests/evidence/capture-resilience/`. This suite is not a full-day live recording or battery benchmark.
