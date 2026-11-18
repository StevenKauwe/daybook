# Reading and recording in Daybook

The live workspace pairs a dark audio monitor with a journal. Recorder state and the Start or Stop control stay visible while switching pages or reading back. Paper uses a warm background, dark ink and a spruce navigation rail. Night uses a deep green reading surface with mint microphone and lilac system accents. Both retain explicit labels for each input and state.

| Surface | Behavior |
| --- | --- |
| Input monitors | The trace contains the most recent 3.2 seconds of measured RMS and peak values. The narrow meter at the right reacts to the latest reading. Missing or expired readings clear the display. |
| Transcript | Georgia at 23 pt by default, adjustable from 16 to 33 pt, with a system sans alternative. New York is used for the page title. Fonts come from macOS with egui fallbacks. |
| Passages | Adjacent chunks from the same source read as paragraphs. Writing marks an in-progress passage. Timestamps and retained sources open from each paragraph. |
| Focus | Hides navigation and signal monitors to expand the transcript. Escape returns to the workspace. The recording control stays visible. |
| Quick note | ⌘N opens the same local draft from any page. Escape keeps the draft. ⌘Return saves a note. |
| Navigation | ⌘1 through ⌘5 switch pages. Narrow windows use an icon rail with names in tooltips and accessibility metadata. |
| Diagnostics | Capture interruptions stay in the header. Detailed engine activity, loss counts, provider budgets and source evidence have dedicated views. |

An 80-passage page bounds the text retained by the reader. Revisions invalidate cached paragraph layout. Meter frames reuse that layout and draw visible paragraphs. New times on an already-silent input can be coalesced while real amplitude changes request a repaint. Minimized windows back off polling. The backend continues independently of the controls window.

The design uses explicit status, consistent actions and keyboard access. The compact instrument display draws from game HUDs, and the serif journal gives longer conversations a quiet reading surface. The fine contour drawing on the navigation rail is static artwork. [Nielsen Norman Group usability heuristics](https://www.nngroup.com/articles/ten-usability-heuristics/) · [USWDS design tokens](https://designsystem.digital.gov/design-tokens/) · [Game Accessibility Guidelines](https://gameaccessibilityguidelines.com/provide-high-contrast-between-text-ui-and-background/)

## Native previews

These are native GPU window renders of a fictional conversation. The developer fixture has no connection to the recorder. Use `ui-rust/target/release/daybook-ui --design-preview /tmp/daybook.png` with optional `--night`, `--compact`, `--focus`, `--empty` or `--motion`.
