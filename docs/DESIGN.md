# Reading and recording in Daybook

The live workspace opens on a time-aligned journal. The **Inputs** menu opens the audio monitors. Switch between system audio and passive screen text alongside the microphone. Manual screenshots and clipboard entries are visible with either choice. Recorder state and the Start or Stop control stay visible while switching pages or reading back. Paper uses a warm background, dark ink and a spruce navigation rail. Night uses a deep green reading surface with rose microphone, lilac system and green saved-item accents. Both retain explicit labels for each input and state.

| Surface | Behavior |
| --- | --- |
| Input monitors | The trace contains the most recent 3.2 seconds of measured RMS and peak values. The narrow meter at the right reacts to the latest reading. Missing or expired readings clear the display. |
| Transcript | The reading-size setting scales speech text and offers a system sans alternative to the journal serif. Text also scales with the visible time span and space until the next passage. Fonts come from macOS with egui fallbacks. |
| Passages | Simultaneous passages line up across columns. Hover a saved image icon for its original. In Paragraph view, adjacent chunks from the same source read as paragraphs and Writing marks an in-progress passage. |
| Focus | Hides navigation and signal monitors to expand the transcript. Escape returns to the workspace. The recording control stays visible. |
| Quick note | ⌘N opens the same local draft from any page. Escape keeps the draft. ⌘Return saves a note. |
| Navigation | ⌘1 through ⌘5 switch pages. Narrow windows use an icon rail with names in tooltips and accessibility metadata. |
| Diagnostics | Capture interruptions stay in the header. Detailed engine activity, loss counts, provider budgets and source evidence have dedicated views. |

The parallel reader positions every source by its first sighting on a shared clock. The Activity strip browses a fractional time scale anchored at the cursor, with ticks at clock boundaries. A drag there selects an interval. The Selection band moves or trims that interval. A gesture keeps its original scale and rectangle through release, including when a frame contains the entire drag. Wheel and button navigation reproject cached activity locally and fetch the final overview after input settles. A small nearby cache lets the columns update during a drag while the recorder coalesces exact range requests. Dense windows group by time and source. A verbatim name, number or topic stays at reading size. Opening a group pages through its passages, and copying queries the full range. The optional continuous reader holds an 80-passage page. Revisions invalidate cached paragraph layout. Meter frames reuse that layout and draw visible paragraphs. New times on an already-silent input can be coalesced while real amplitude changes request a repaint. Minimized windows back off polling. The backend continues independently of the controls window.

The design uses explicit status, consistent actions and keyboard access. The compact instrument display draws from game HUDs, and the serif journal gives longer conversations a quiet reading surface. The fine contour drawing on the navigation rail is static artwork. [Nielsen Norman Group usability heuristics](https://www.nngroup.com/articles/ten-usability-heuristics/) · [USWDS design tokens](https://designsystem.digital.gov/design-tokens/) · [Game Accessibility Guidelines](https://gameaccessibilityguidelines.com/provide-high-contrast-between-text-ui-and-background/)

## Archive timeline

The Archive uses the day’s actual activity to draw a 15-minute histogram. A drag selects a time interval. The journal below it groups entries by hour and gives the words more room than the source labels. A reader can open an image alongside its text, enlarge it, then close the entry in the same window. Background refresh waits while someone reads back. A manual refresh preserves the visible entry when newer entries arrive.

**Sessions** groups activity until a 15-minute gap. A group spans ongoing passages, including across hour and pagination boundaries. A note or a saved image can supply its title. A search matches any entry in the group, and opening the result selects the whole session so its conversation, notes and screen entries can be read or copied together. A possible visual index could also show a strip of saved screenshots for someone who remembers what a page looked like.

## Native previews

These are native GPU window renders of a fictional conversation. The developer fixture has no connection to the recorder. Use `ui-rust/target/release/daybook-ui --design-preview /tmp/daybook.png` with optional `--night`, `--compact`, `--focus`, `--empty` or `--motion`.

Use `--archive` for the fictional Archive and add `--selection` to show a selected time range.
