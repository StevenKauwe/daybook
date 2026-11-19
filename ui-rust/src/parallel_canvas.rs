//! The same vertical coordinate represents the same instant in every source column.
use super::*;
use crate::{parallel::clock, time_ruler, transcript_range::*};
use egui::{Align2, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use std::sync::Arc;

pub(super) struct Hit {
    id: i64,
    lane: Lane,
    at: i64,
    end: i64,
    rect: Rect,
    pos: Pos2,
    text: Arc<egui::Galley>,
    len: usize,
}
struct Bundle {
    first: i64,
    last: i64,
    count: u64,
    index: Option<usize>,
    example: String,
}
fn fraction(t: i64, range: Span) -> f32 {
    ((t - range.from) as f64 / range.len() as f64).clamp(0., 1.) as f32
}
fn time_at(f: f32, r: Span) -> i64 {
    r.from + (f.clamp(0., 1.) as f64 * r.len() as f64).round() as i64
}
pub(super) fn source_color(l: Lane, night: bool) -> Color32 {
    match (l, night) {
        (Lane::Mic, false) => Color32::from_rgb(145, 63, 87),
        (Lane::Mic, true) => Color32::from_rgb(239, 173, 192),
        (Lane::System, false) => Color32::from_rgb(74, 82, 137),
        (Lane::System, true) => Color32::from_rgb(188, 196, 249),
        (Lane::Passive, false) => Color32::from_rgb(48, 106, 103),
        (Lane::Passive, true) => Color32::from_rgb(148, 204, 195),
        (Lane::Saves, false) => Color32::from_rgb(53, 104, 64),
        (Lane::Saves, true) => Color32::from_rgb(167, 213, 176),
    }
}
fn bundles(w: &Window, lane: Lane, height: f32) -> Vec<Bundle> {
    let mut result: Vec<Bundle> = vec![];
    let singles = w
        .passages
        .iter()
        .enumerate()
        .filter(|(_, p)| p.lane() == lane)
        .map(|(i, p)| {
            let at = p.at.max(w.range.from);
            Bundle {
                first: at,
                last: at,
                count: 1,
                index: Some(i),
                example: String::new(),
            }
        });
    let folded = w
        .clusters
        .iter()
        .filter(|b| b.lane == lane)
        .map(|b| Bundle {
            first: b.range.from,
            last: b.range.until - 1,
            count: b.count,
            index: None,
            example: b.excerpt.clone(),
        });
    for b in singles.chain(folded) {
        if let Some(prev) = result.last_mut() {
            if (fraction(b.first, w.range) - fraction(prev.first, w.range)) * height < 20. {
                if let Some(i) = prev.index {
                    prev.example = w.passages[i].body().chars().take(130).collect();
                }
                prev.last = prev.last.max(b.last);
                prev.count += b.count;
                prev.index = None;
                continue;
            }
        }
        result.push(b);
    }
    result
}
// Preserve reading order around the full-size keyword. In a narrow lane, discard the
// beginning of a long prefix so that the title still fits on the first line.
fn compact_layout(
    ui: &egui::Ui,
    width: f32,
    parts: (&str, &str, &str),
    tiny: f32,
    ink: Color32,
    muted: Color32,
) -> Arc<egui::Galley> {
    let (before, word, after) = parts;
    let before = before.rsplit(['\r', '\n']).next().unwrap_or("");
    let anchor = design::passage_font(14.5);
    let small = design::passage_font(tiny);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    let context = egui::TextFormat {
        font_id: small.clone(),
        color: muted,
        ..Default::default()
    };
    let mut prefix = before;
    if !before.is_empty() {
        let anchor_width = ui
            .painter()
            .layout_no_wrap(word.to_owned(), anchor.clone(), ink)
            .size()
            .x;
        let budget = (width - anchor_width - 12.).max(0.);
        let lead = ui.painter().layout_no_wrap(before.to_owned(), small, muted);
        if lead.size().x > budget {
            let index = lead
                .cursor_from_pos(vec2(lead.size().x - budget, 0.))
                .index
                .0;
            let remaining = before
                .char_indices()
                .nth(index)
                .map_or("", |(i, _)| &before[i..]);
            prefix = remaining
                .find(char::is_whitespace)
                .map_or("", |i| &remaining[i..]);
        }
    }
    job.append(prefix, 0., context.clone());
    job.append(
        word,
        0.,
        egui::TextFormat {
            font_id: anchor,
            color: ink,
            ..Default::default()
        },
    );
    job.append(after, 0., context);
    ui.fonts_mut(|f| f.layout_job(job))
}
impl Controls {
    pub(super) fn has_visible_picks(&self, w: &Window) -> bool {
        w.passages
            .iter()
            .any(|p| self.parallel.picks.get(p).is_some())
            || w.clusters.iter().any(|b| {
                self.parallel.picks.lanes.contains(&b.lane) || self.parallel.marked(w.range, b.lane)
            })
    }
    pub(super) fn passage_layout(
        &self,
        ui: &egui::Ui,
        p: &Passage,
        size: f32,
        width: f32,
        color: Color32,
    ) -> Arc<egui::Galley> {
        let font = if self.reading.journal && p.lane() != Lane::Saves {
            design::passage_font(size)
        } else {
            FontId::proportional(size)
        };
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = width.max(1.);
        let format = egui::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        };
        let s = p.body();
        if let Some(Pick::Chars([a, b])) = self.parallel.picks.get(p) {
            let byte = |n| s.char_indices().nth(n).map_or(s.len(), |(i, _)| i);
            let (a, b) = (byte(a.min(b)), byte(b.max(a)));
            job.append(&s[..a], 0., format.clone());
            job.append(
                &s[a..b],
                0.,
                egui::TextFormat {
                    background: source_color(p.lane(), self.reading.night).gamma_multiply(0.26),
                    ..format.clone()
                },
            );
            job.append(&s[b..], 0., format);
        } else {
            job.append(s, 0., format);
        }
        ui.fonts_mut(|f| f.layout_job(job))
    }
    pub(super) fn paint_passage(
        &mut self,
        ui: &mut egui::Ui,
        r: Rect,
        galley: Arc<egui::Galley>,
        p: &Passage,
        range: Span,
        hits: &mut Vec<Hit>,
    ) {
        let colors = design::paper(self.reading.night);
        let pos = r.min + vec2(3., 1.);
        if self.parallel.picks.get(p) == Some(Pick::Full) {
            ui.painter().rect_filled(
                r,
                3.,
                source_color(p.lane(), self.reading.night).gamma_multiply(0.16),
            );
        }
        ui.painter()
            .with_clip_rect(r.intersect(ui.clip_rect()))
            .galley(pos, galley.clone(), colors.ink);
        let body_right = if matches!(p.lane(), Lane::Saves | Lane::Passive) {
            r.right() - 22.
        } else {
            r.right()
        };
        let text_rect = Rect::from_min_max(r.min, pos2(body_right.max(r.left() + 1.), r.bottom()));
        let response = ui
            .interact(
                text_rect,
                ui.id().with(("parallel-passage", p.id)),
                Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Text);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, p.body()));
        if response.clicked() {
            self.parallel.pin(range);
            let pick = if self.parallel.picks.get(p).is_some() {
                Pick::Skip
            } else {
                Pick::Full
            };
            self.parallel.mark(p.id, p.lane(), p.at, p.end, pick);
        }
        if response.drag_started()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            self.parallel.pin(range);
            self.parallel.drag_start =
                Some((p.id, galley.cursor_from_pos(origin - pos).index.0, ui.id()));
        }
        response
            .clone()
            .on_hover_text(if p.lane() == Lane::Passive && p.end > p.at {
                format!(
                    "First seen {} · last seen {}",
                    clock(p.at, "%H:%M:%S"),
                    clock(p.end, "%H:%M:%S")
                )
            } else {
                format!("{} · {}", p.label(), clock(p.at, "%H:%M:%S"))
            });
        response.context_menu(|ui| {
            if ui.button("Read full passage").clicked() {
                self.inspect("observation", p.id, None);
                ui.close();
            }
        });
        if matches!(p.lane(), Lane::Saves | Lane::Passive) {
            let end = galley.pos_from_cursor(galley.end()).min;
            let left = (pos.x + end.x + 4.).clamp(r.left(), (r.right() - 20.).max(r.left()));
            let top = (pos.y + end.y).clamp(r.top(), (r.bottom() - 16.).max(r.top()));
            let icon_rect = Rect::from_min_size(pos2(left, top), vec2(18., 16.));
            self.original_chip(ui, icon_rect, p);
        }
        hits.push(Hit {
            id: p.id,
            lane: p.lane(),
            at: p.at,
            end: p.end,
            rect: text_rect,
            pos,
            text: galley,
            len: p.body().chars().count(),
        });
    }
    fn original_chip(&mut self, ui: &mut egui::Ui, rect: Rect, p: &Passage) {
        let colors = design::paper(self.reading.night);
        let button = ui.interact(rect, ui.id().with(("original", p.id)), Sense::click());
        ui.painter()
            .rect_stroke(rect, 4., Stroke::new(0.8, colors.line), StrokeKind::Inside);
        let glyph = match (p.kind.as_str(), p.source.as_str()) {
            ("screen", "passive_screen" | "screen" | "vision" | "screenshot") => {
                design::Icon::System
            }
            ("screen", "clipboard") => design::Icon::Photo,
            ("note", "clipboard") => design::Icon::Clipboard,
            _ => design::Icon::Note,
        };
        design::icon(
            ui.painter(),
            rect.center(),
            12.,
            glyph,
            source_color(p.lane(), self.reading.night),
        );
        if p.lane() == Lane::Passive {
            button.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Open capture layout")
            });
            if button.clicked() {
                self.open_capture(p.id);
            }
            button.on_hover_ui(|ui| {
                ui.label("Open capture layout");
                if let Some(id) = p.capture_image {
                    let (area, _) = ui.allocate_exact_size(vec2(255., 154.), Sense::hover());
                    self.thumbnails
                        .paint_from(ui, area, &self.base, ("passive_image", id));
                }
            });
            return;
        }
        button.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("Open original {}", p.label()),
            )
        });
        if button.clicked() {
            self.inspect("observation", p.id, None);
        }
        button.on_hover_ui(|ui| {
            ui.label(p.label());
            if let Some(id) = p.image_id {
                let (rect, _) = ui.allocate_exact_size(vec2(255., 154.), Sense::hover());
                self.thumbnails.paint(ui, rect, &self.base, id);
            } else {
                ui.set_max_width(325.);
                ui.label(p.body());
            }
        });
    }
    pub(super) fn word_drag(&mut self, ui: &egui::Ui, r: Span, hits: &[Hit]) {
        let Some((id, anchor, owner)) = self.parallel.drag_start else {
            return;
        };
        if owner != ui.id() {
            return;
        }
        let Some(start) = hits.iter().find(|h| h.id == id) else {
            return;
        };
        let Some(pos) = ui.input(|i| i.pointer.interact_pos()) else {
            return;
        };
        let same: Vec<_> = hits.iter().filter(|h| h.lane == start.lane).collect();
        let nearest = same.iter().enumerate().min_by(|(_, a), (_, b)| {
            a.rect
                .distance_to_pos(pos)
                .total_cmp(&b.rect.distance_to_pos(pos))
        });
        if let Some((end_i, end)) = nearest {
            let begin = same.iter().position(|h| h.id == id).unwrap();
            let tip = end.text.cursor_from_pos(pos - end.pos).index.0.min(end.len);
            let released = !ui.input(|i| i.pointer.primary_down());
            if released {
                self.parallel.drag_start = None;
            }
            if tip == anchor && begin == end_i {
                return;
            }
            for (i, h) in same
                .iter()
                .enumerate()
                .take(begin.max(end_i) + 1)
                .skip(begin.min(end_i))
            {
                let (a, b) = if begin == end_i {
                    (anchor.min(tip), anchor.max(tip))
                } else if i == begin {
                    if begin < end_i {
                        (anchor, h.len)
                    } else {
                        (0, anchor)
                    }
                } else if i == end_i {
                    if begin < end_i {
                        (0, tip)
                    } else {
                        (tip, h.len)
                    }
                } else {
                    (0, h.len)
                };
                if a >= b {
                    continue;
                }
                if released {
                    self.parallel.pin(r);
                    self.parallel
                        .mark(h.id, h.lane, h.at, h.end, Pick::Chars([a, b]));
                    continue;
                }
                let left = h.text.pos_from_cursor(egui::text::CCursor::new(a));
                let right = h.text.pos_from_cursor(egui::text::CCursor::new(b));
                for row in &h.text.rows {
                    let line = row.rect();
                    if line.center().y < left.top() || line.center().y > right.bottom() {
                        continue;
                    }
                    let x = if line.y_range().contains(left.center().y) {
                        left.left()
                    } else {
                        line.left()
                    };
                    let last = if line.y_range().contains(right.center().y) {
                        right.left()
                    } else {
                        line.right()
                    };
                    let mark = Rect::from_min_max(pos2(x, line.top()), pos2(last, line.bottom()))
                        .translate(h.pos.to_vec2())
                        .intersect(h.rect);
                    if mark.is_positive() {
                        ui.painter().rect_filled(
                            mark,
                            2.,
                            source_color(h.lane, self.reading.night).gamma_multiply(0.22),
                        );
                    }
                }
            }
        }
    }
    pub(super) fn draw_columns(&mut self, ui: &mut egui::Ui, w: &Window) {
        let colors = design::paper(self.reading.night);
        let mid = if self.reading.passive_screen {
            Lane::Passive
        } else {
            Lane::System
        };
        let lanes: Vec<_> = [Lane::Mic, mid, Lane::Saves]
            .into_iter()
            .filter(|l| {
                w.passages.iter().any(|p| p.lane() == *l) || w.clusters.iter().any(|b| b.lane == *l)
            })
            .collect();
        let width = ui.available_width();
        let height = (ui.available_height() - 65.).max(112.);
        let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let gutter = if w.range.len() > 86400 { 85. } else { 61. };
        let col = ((width - gutter) / lanes.len().max(1) as f32).max(28.);
        let y0 = rect.top() + 39.;
        let height = (rect.bottom() - y0 - 19.).max(32.);
        if ui.rect_contains_pointer(rect)
            && !ui.input(|i| i.pointer.primary_down())
            && let Some(p) = ui.input(|i| i.pointer.hover_pos())
            && let Some(next) = time_ruler::wheel_range(
                ui,
                ui.id().with("passage-wheel"),
                w.range,
                (p.y - y0) / height,
            )
        {
            self.parallel.reveal(next);
            ui.ctx().request_repaint();
        }
        ui.painter().text(
            rect.min + vec2(1., 5.),
            Align2::LEFT_TOP,
            "Time",
            FontId::proportional(12.),
            colors.muted,
        );
        let ticks = ((height / 78.).floor() as i32)
            .clamp(2, 5)
            .min(w.range.len().min(5) as i32);
        for i in 0..=ticks {
            let f = i as f32 / ticks as f32;
            let fmt = if w.range.len() > 86400 {
                "%b %-d %Hh"
            } else {
                "%H:%M:%S"
            };
            ui.painter().text(
                pos2(rect.left() + 1., y0 + height * f),
                Align2::LEFT_CENTER,
                clock(time_at(f, w.range), fmt),
                FontId::proportional(11.),
                colors.muted,
            );
        }
        if lanes.is_empty() {
            ui.painter().text(
                pos2(rect.left() + gutter + 12., y0 + 16.),
                Align2::LEFT_CENTER,
                "No text in this time range",
                design::serif(21.),
                colors.ink,
            );
            return;
        }
        let mut hits = vec![];
        for (i, lane) in lanes.iter().copied().enumerate() {
            let x = rect.left() + gutter + col * i as f32 + 5.;
            let right = (x + col - 11.).max(x + 15.);
            let head = Rect::from_min_max(pos2(x, rect.top()), pos2(right, rect.top() + 26.));
            let copy_rect = Rect::from_min_max(pos2(right - 33., head.top()), head.max);
            let title_rect =
                Rect::from_min_max(head.min, pos2(copy_rect.left() - 2., head.bottom()));
            let picked = self.parallel.picks.lanes.contains(&lane);
            let title = ui.interact(
                title_rect,
                ui.id().with(("select-column", lane as u8)),
                Sense::click(),
            );
            title.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, lane.label())
            });
            let text = ui
                .painter()
                .with_clip_rect(title_rect.intersect(ui.clip_rect()))
                .text(
                    title_rect.left_center(),
                    Align2::LEFT_CENTER,
                    lane.label(),
                    FontId::proportional(if col < 174. { 12. } else { 14. }),
                    source_color(lane, self.reading.night),
                );
            if picked || title.hovered() {
                ui.painter().hline(
                    text.x_range(),
                    text.bottom(),
                    Stroke::new(1., source_color(lane, self.reading.night)),
                );
            }
            if title.clicked() {
                self.parallel.pin(w.range);
                if picked {
                    self.parallel.picks.lanes.remove(&lane);
                } else {
                    self.parallel.picks.lanes.insert(lane);
                }
                self.parallel
                    .picks
                    .entries
                    .retain(|id, _| self.parallel.marks.get(id).is_none_or(|m| m.0 != lane));
                self.parallel.marks.retain(|_, m| m.0 != lane);
            }
            if ui
                .put(
                    copy_rect,
                    egui::Button::new(RichText::new("Copy").size(12.).color(colors.muted))
                        .frame(false),
                )
                .on_hover_text(format!("Copy {}", lane.label()))
                .clicked()
            {
                let chosen = picked || self.parallel.marked(w.range, lane);
                self.copy_parallel(w.range, Some(lane), chosen);
            }
            let groups = bundles(w, lane, height);
            for (j, b) in groups.iter().enumerate() {
                let y = y0 + fraction(b.first, w.range) * height;
                let next = groups
                    .get(j + 1)
                    .map(|g| y0 + fraction(g.first, w.range) * height)
                    .unwrap_or(rect.bottom());
                let cell = Rect::from_min_max(pos2(x, y), pos2(right, (next - 2.).max(y + 1.)));
                let mut small = true;
                if let Some(index) = b.index {
                    let p = &w.passages[index];
                    let wrap = (cell.width()
                        - if matches!(lane, Lane::Saves | Lane::Passive) {
                            26.
                        } else {
                            7.
                        })
                    .max(12.);
                    let ceiling = (self.reading.size as f32 * 0.77)
                        * (60. / w.range.len() as f32).sqrt().clamp(0.37, 1.1);
                    let mut size = ceiling.max(6.);
                    let mut galley = self.passage_layout(ui, p, size, wrap, colors.ink);
                    for _ in 0..3 {
                        if galley.size().y + 2. <= cell.height() || size <= 6. {
                            break;
                        }
                        size = (size * (cell.height() / (galley.size().y + 3.)).sqrt())
                            .clamp(6., size);
                        galley = self.passage_layout(ui, p, size, wrap, colors.ink);
                    }
                    if galley.size().y + 1. <= cell.height() && size >= 12.5 {
                        let row = Rect::from_min_size(
                            cell.min,
                            vec2(
                                cell.width(),
                                (galley.size().y + 3.).max(16.).min(cell.height()),
                            ),
                        );
                        self.paint_passage(ui, row, galley, p, w.range, &mut hits);
                        small = false;
                    }
                }
                if small {
                    let excerpt = b.index.map(|i| w.passages[i].body()).unwrap_or(&b.example);
                    let parts = crate::keywords::parts(excerpt);
                    let word = parts.1;
                    let lens_width = if b.count > 1 { 41. } else { 25. };
                    let row =
                        Rect::from_min_size(cell.min, vec2(cell.width(), cell.height().min(22.)));
                    let lens = Rect::from_min_size(
                        pos2((right - lens_width).max(x), y),
                        vec2(lens_width, row.height()),
                    );
                    let original = b
                        .index
                        .map(|i| &w.passages[i])
                        .filter(|p| matches!(p.lane(), Lane::Saves | Lane::Passive));
                    let text_end = lens.left() - if original.is_some() { 22. } else { 2. };
                    let text_rect =
                        Rect::from_min_max(cell.min, pos2(text_end.max(x + 1.), row.bottom()));
                    let ink = source_color(lane, self.reading.night);
                    if !word.is_empty() {
                        let tiny = (8.5 * (60. / w.range.len() as f32).powf(0.17)).clamp(4., 9.);
                        let text =
                            compact_layout(ui, text_rect.width(), parts, tiny, ink, colors.muted);
                        ui.painter()
                            .with_clip_rect(text_rect.intersect(ui.clip_rect()))
                            .galley(text_rect.min, text, ink);
                    }
                    ui.painter()
                        .rect_filled(lens, 4., colors.line.gamma_multiply(0.32));
                    let dots_x = if b.count > 1 {
                        lens.left() + 10.
                    } else {
                        lens.center().x
                    };
                    for dx in [-3., 0., 3.] {
                        ui.painter().circle_filled(
                            pos2(dots_x + dx, lens.center().y),
                            0.85,
                            colors.muted,
                        );
                    }
                    if b.count > 1 {
                        ui.painter().text(
                            pos2(lens.left() + 18., lens.center().y),
                            Align2::LEFT_CENTER,
                            b.count,
                            FontId::proportional(11.),
                            colors.muted,
                        );
                    }
                    let response = ui.interact(
                        row,
                        egui::Id::new(("parallel-unfold", lane as u8, b.first)),
                        Sense::click(),
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            true,
                            format!("Read {word}, {} passages", b.count),
                        )
                    });
                    if response
                        .on_hover_cursor(egui::CursorIcon::ZoomIn)
                        .on_hover_text("Read passages at full size")
                        .clicked()
                    {
                        self.details_at(
                            w.range,
                            Span {
                                from: b.first,
                                until: b.last + 1,
                            },
                            lane,
                        );
                    }
                    if let Some(p) = original {
                        let chip = Rect::from_min_size(
                            pos2(lens.left() - 21., row.center().y - 8.),
                            vec2(18., 16.),
                        );
                        self.original_chip(ui, chip, p);
                    }
                }
            }
        }
        self.word_drag(ui, w.range, &hits);
    }
}
