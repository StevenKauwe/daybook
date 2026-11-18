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

## Archive timeline

The Archive uses the day’s actual activity to draw a 15-minute histogram. A drag selects a time interval. The journal below it groups entries by hour and gives the words more room than the source labels. A reader can open an image alongside its text, enlarge it, then close the entry in the same window. Background refresh waits while someone reads back. A manual refresh preserves the visible entry when newer entries arrive.

Two possible follow-ups serve different ways of remembering a day. A **session view** could gather a nearby conversation, screenshot and note around a gap in activity, with the actual times visible so a coincidental overlap is clear. A **visual index** could show a strip of saved screenshots above their time range for someone who remembers what a page looked like. Both can use the current timestamped entries without rewriting the journal.

## Native previews

These are native GPU window renders of a fictional conversation. The developer fixture has no connection to the recorder. Use `ui-rust/target/release/daybook-ui --design-preview /tmp/daybook.png` with optional `--night`, `--compact`, `--focus`, `--empty` or `--motion`.

Use `--archive` for the fictional Archive and add `--selection` to show a selected time range.
