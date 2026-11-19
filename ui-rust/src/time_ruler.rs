//! The activity strip browses time. The band below edits the selected transcript.
use super::*;
use crate::{parallel::clock, parallel_canvas::source_color, transcript_range::*};
use egui::{
    Align2, CursorIcon, Event, FontId, Id, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind,
    pos2, vec2,
};
use std::sync::Arc;

// Keep fractional seconds while navigating. Rounding every trackpad event slowly
// moves the instant under the cursor, especially near the middle of the ruler.
#[derive(Clone, Copy)]
struct Scale {
    start: f64,
    seconds: f64,
}
impl Scale {
    fn for_span(span: Span) -> Self {
        Self {
            start: span.from as f64,
            seconds: span.len() as f64,
        }
    }
    fn bounded(start: f64, seconds: f64) -> Self {
        let seconds = seconds.clamp(1., (MAX_TIME - MIN_TIME) as f64);
        Self {
            start: start.clamp(MIN_TIME as f64, MAX_TIME as f64 - seconds),
            seconds,
        }
    }
    fn span(self) -> Span {
        let from = self.start.round() as i64;
        Span {
            from,
            until: ((self.start + self.seconds).round() as i64).max(from + 1),
        }
    }
    fn zoom(self, at: f64, factor: f64) -> Self {
        let len = (self.seconds * factor).clamp(1., (MAX_TIME - MIN_TIME) as f64);
        Self::bounded(self.start + at.clamp(0., 1.) * (self.seconds - len), len)
    }
    fn time(self, x: f32, rect: Rect) -> i64 {
        (self.start + position(x, rect) * self.seconds).round() as i64
    }
    fn x(self, at: i64, rect: Rect) -> f32 {
        rect.left() + ((at as f64 - self.start) / self.seconds).clamp(0., 1.) as f32 * rect.width()
    }
    fn cached(ui: &egui::Ui, id: Id, span: Span) -> Self {
        ui.data(|d| d.get_temp::<Self>(id))
            .filter(|s| s.span() == span)
            .unwrap_or_else(|| Self::for_span(span))
    }
}
fn position(x: f32, rect: Rect) -> f64 {
    f64::from(((x - rect.left()) / rect.width().max(1.)).clamp(0., 1.))
}
fn wheel_factor(ui: &egui::Ui) -> Option<f64> {
    let zoom = ui.input(|i| {
        i.events
            .iter()
            .map(|e| match e {
                Event::MouseWheel { unit, delta, .. } => {
                    let scale = match unit {
                        egui::MouseWheelUnit::Point => 1.,
                        egui::MouseWheelUnit::Line => 32.,
                        egui::MouseWheelUnit::Page => 160.,
                    };
                    f64::from(delta.y) * scale / 180.
                }
                Event::Zoom(factor) if *factor > 0. => f64::from(*factor).log2(),
                _ => 0.,
            })
            .sum::<f64>()
    });
    (zoom.is_finite() && zoom != 0.).then(|| (-zoom.clamp(-8., 8.)).exp2())
}
pub(super) fn wheel_range(ui: &egui::Ui, id: Id, range: Span, at: f32) -> Option<Span> {
    let scale = Scale::cached(ui, id, range).zoom(f64::from(at), wheel_factor(ui)?);
    ui.data_mut(|d| d.insert_temp(id, scale));
    (scale.span() != range).then(|| scale.span())
}

#[derive(Clone, Copy)]
enum Gesture {
    Draw,
    Move,
    From,
    Until,
}
pub(super) struct Brush {
    pub domain: Arc<Window>,
    original: Span,
    scale: Scale,
    track: Rect,
    origin: Pos2,
    gesture: Gesture,
    moved: bool,
}
impl Brush {
    fn sample(&mut self, p: Pos2) -> Option<Span> {
        self.moved |= (p.x - self.origin.x).abs() >= 3.;
        if !self.moved {
            return None;
        }
        let first = self.scale.time(self.origin.x, self.track);
        let tip = self.scale.time(p.x, self.track);
        let delta = tip - first;
        let r = self.original;
        let axis = self.scale.span();
        let dragged_end = |at| {
            if (axis.from..=axis.until).contains(&at) {
                at + delta
            } else {
                tip
            }
        };
        Some(match self.gesture {
            Gesture::Draw => {
                let from = tip.min(first).min(axis.until - 1);
                Span {
                    from,
                    until: tip.max(first).max(from + 1),
                }
            }
            Gesture::Move => Span::ending(r.until + delta, r.len()),
            Gesture::From => Span {
                from: dragged_end(r.from).clamp(MIN_TIME, r.until - 1),
                ..r
            },
            Gesture::Until => Span {
                until: dragged_end(r.until).clamp(r.from + 1, MAX_TIME),
                ..r
            },
        })
    }
}

// Handle centers stay inside the selection row, even for clipped and one-second spans.
struct Band {
    fill: Option<Rect>,
    from: Option<Rect>,
    until: Option<Rect>,
}
impl Band {
    fn new(track: Rect, scale: Scale, r: Span) -> Self {
        let axis = scale.span();
        let w = 12_f32.min(track.width() / 2.);
        let grip = |at| {
            let x = scale
                .x(at, track)
                .clamp(track.left() + w / 2., track.right() - w / 2.);
            Rect::from_center_size(pos2(x, track.center().y), vec2(w, track.height()))
        };
        let mut from = (r.until > axis.from).then(|| grip(r.from));
        let mut until = (r.from < axis.until).then(|| grip(r.until));
        // Leave a place to grab the middle even when the interval is one pixel wide.
        let body = (track.width() - 2. * w).clamp(0., 18.);
        let margin = w + body / 2.;
        if let (Some(a), Some(b)) = (&mut from, &mut until)
            && b.left() - a.right() < body
        {
            let mid = ((a.center().x + b.center().x) / 2.).clamp(
                track.left() + margin,
                (track.right() - margin).max(track.left() + margin),
            );
            *a = a.translate(vec2(mid - body / 2. - a.right(), 0.));
            *b = b.translate(vec2(mid + body / 2. - b.left(), 0.));
        }
        let fill = from
            .zip(until)
            .map(|(a, b)| Rect::from_min_max(a.min, b.max));
        Self { fill, from, until }
    }
    fn gesture(&self, p: Pos2) -> Gesture {
        let dist = |r: Option<Rect>| {
            r.filter(|r| r.expand(3.).contains(p))
                .map_or(f32::INFINITY, |r| (r.center().x - p.x).abs())
        };
        let (a, b) = (dist(self.from), dist(self.until));
        if a.is_finite() && a <= b {
            Gesture::From
        } else if b.is_finite() {
            Gesture::Until
        } else if self.fill.is_some_and(|r| r.contains(p)) {
            Gesture::Move
        } else {
            Gesture::Draw
        }
    }
    fn paint(&self, painter: &egui::Painter, colors: design::Paper, scale: Scale, r: Span) {
        if let Some(fill) = self.fill {
            painter.rect_filled(fill, 4., colors.mine.gamma_multiply(0.20));
            painter.rect_stroke(fill, 4., Stroke::new(1., colors.mine), StrokeKind::Inside);
            if fill.width() > 90. {
                painter.text(
                    fill.center(),
                    Align2::CENTER_CENTER,
                    duration(r.len()),
                    FontId::proportional(12.),
                    colors.mine,
                );
            }
        }
        let axis = scale.span();
        for (grip, time) in [(self.from, r.from), (self.until, r.until)] {
            let Some(rect) = grip else { continue };
            painter.rect_filled(rect, 4., colors.mine);
            let c = rect.center();
            let stroke = Stroke::new(1.4, colors.sheet);
            if self.fill.is_none() || !(axis.from..=axis.until).contains(&time) {
                let dir = if time <= axis.from { -1. } else { 1. };
                painter.add(egui::Shape::line(
                    vec![
                        c + vec2(-dir * 2., -3.),
                        c + vec2(dir * 2., 0.),
                        c + vec2(-dir * 2., 3.),
                    ],
                    stroke,
                ));
            } else {
                painter.vline(c.x, c.y - 4. ..=c.y + 4., stroke);
            }
        }
    }
}
fn duration(seconds: i64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        3600..86400 => format!("{}h {:02}m", seconds / 3600, seconds / 60 % 60),
        _ => format!("{}d {}h", seconds / 86400, seconds / 3600 % 24),
    }
}
fn row(
    ui: &mut egui::Ui,
    name: &str,
    height: f32,
    id: Id,
    colors: design::Paper,
) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let left = rect.left() + 67_f32.min(rect.width() * 0.25);
    ui.painter().text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(13.),
        colors.muted,
    );
    let track = Rect::from_min_max(pos2(left, rect.top()), rect.max);
    ui.painter().rect_filled(track, 4., colors.sheet);
    ui.painter()
        .rect_stroke(track, 4., Stroke::new(1., colors.line), StrokeKind::Inside);
    let response = ui.interact(track, id, Sense::click_and_drag());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, name));
    response
}
fn paint_activity(p: &egui::Painter, rect: Rect, scale: Scale, w: &Window, night: bool) {
    let p = p.with_clip_rect(p.clip_rect().intersect(rect));
    let colors = design::paper(night);
    let axis = scale.span();
    for band in &w.bands {
        if band.range.until <= axis.from || band.range.from >= axis.until {
            continue;
        }
        let lane = match band.lane {
            Lane::Mic => 0.,
            Lane::Saves => 2.,
            _ => 1.,
        };
        let y = rect.top() + 4. + lane * 4.;
        let left = scale.x(band.range.from, rect);
        let right = scale.x(band.range.until, rect).max(left + 1.5);
        p.rect_filled(
            Rect::from_min_max(pos2(left, y), pos2(right, y + 3.)),
            1.,
            source_color(band.lane, night),
        );
    }
    // Ticks belong to clock times. As the camera moves, the same label travels with
    // its mark rather than four fixed pixels continually acquiring new times.
    let want = scale.seconds / (rect.width() / 105.).max(1.) as f64;
    let steps = [
        1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 10800, 21600, 43200, 86400,
        604800, 2592000, 31536000,
    ];
    let step = steps
        .into_iter()
        .find(|s| *s as f64 >= want)
        .unwrap_or_else(|| (want / 31536000.).ceil() as i64 * 31536000);
    let fmt = if step >= 31536000 {
        "%Y"
    } else if step >= 86400 {
        "%b %-d"
    } else if scale.seconds > 86400. {
        "%a %H:%M"
    } else if step >= 60 {
        "%H:%M"
    } else {
        "%H:%M:%S"
    };
    let mut time = axis.from.div_euclid(step) * step;
    let mut next_label = rect.left() + 2.;
    while time <= axis.until {
        if time as f64 >= scale.start {
            let x = scale.x(time, rect);
            p.vline(
                x,
                rect.top() + 18. ..=rect.top() + 22.,
                Stroke::new(1., colors.line),
            );
            let label = clock(time, fmt);
            let galley = p.layout_no_wrap(label, FontId::proportional(11.), colors.muted);
            let width = galley.size().x;
            let left = (x - width / 2.).clamp(
                rect.left() + 2.,
                (rect.right() - width - 2.).max(rect.left() + 2.),
            );
            if left >= next_label && left + width <= rect.right() {
                p.galley(pos2(left, rect.bottom() - 15.), galley, colors.muted);
                next_label = left + width + 6.;
            }
        }
        time += step;
    }
}

impl Controls {
    pub(super) fn overview_ruler(&mut self, ui: &mut egui::Ui, w: &Arc<Window>) {
        let colors = design::paper(self.reading.night);
        let range = self.parallel.fixed.unwrap_or(w.range);
        let id = Id::new("parallel-time-ruler");
        let mut scale = Scale::cached(
            ui,
            id.with("scale"),
            self.parallel.ruler.unwrap_or(w.overview),
        );
        let mut next = None;
        ui.horizontal(|ui| {
            ui.strong("Timeline");
            for (label, factor, title) in [
                ("−", 2., "Zoom timeline out"),
                ("+", 0.5, "Zoom timeline in"),
            ] {
                if ui.small_button(label).on_hover_text(title).clicked() {
                    next = Some(scale.zoom(0.5, factor));
                }
            }
            if ui
                .small_button("Fit selection")
                .on_hover_text("Show both ends of the selected time")
                .clicked()
            {
                next = Some(Scale::for_span(range).zoom(0.5, 1.25));
            }
            for (label, dir) in [("Earlier", -0.5), ("Later", 0.5)] {
                if ui.small_button(label).clicked() {
                    next = Some(Scale::bounded(
                        scale.start + dir * scale.seconds,
                        scale.seconds,
                    ));
                }
            }
            if ui.available_width() > 190. {
                ui.label(
                    RichText::new("Drag across activity to select time")
                        .size(12.)
                        .color(colors.muted),
                );
            }
        });
        let activity = row(ui, "Activity", 39., id, colors);
        let selection = row(
            ui,
            "Selection",
            23.,
            Id::new("parallel-selection-track"),
            colors,
        );
        let top = activity.rect;
        let bottom = selection.rect;
        if self.parallel.overview_drag.is_none()
            && !ui.input(|i| i.pointer.any_down())
            && ui.rect_contains_pointer(top.union(bottom))
            && let Some(p) = ui.input(|i| i.pointer.hover_pos())
            && let Some(factor) = wheel_factor(ui)
        {
            next = Some(scale.zoom(position(p.x, top), factor));
        }
        if let Some(view) = next {
            scale = view;
            ui.data_mut(|d| d.insert_temp(id.with("scale"), scale));
            self.parallel.pin(range);
            self.parallel.ruler = Some(view.span());
            self.parallel.last_browse = Some(Instant::now());
            ui.ctx().request_repaint_after(Duration::from_millis(120));
        }
        self.ruler_input(ui, top, bottom, scale, w);
        let selected = self.parallel.fixed.unwrap_or(w.range);
        paint_activity(ui.painter(), top, scale, w, self.reading.night);
        let band = Band::new(bottom, scale, selected);
        band.paint(ui.painter(), colors, scale, selected);
        activity
            .on_hover_cursor(CursorIcon::Crosshair)
            .on_hover_text("Drag across activity to select time. Scroll to zoom the timeline.");
        if let Some(p) = selection.hover_pos() {
            let (cursor, tip) = match band.gesture(p) {
                Gesture::From => (CursorIcon::ResizeHorizontal, "Drag to change the start"),
                Gesture::Until => (CursorIcon::ResizeHorizontal, "Drag to change the end"),
                Gesture::Move => (CursorIcon::Grab, "Drag the selection to move it"),
                Gesture::Draw => (CursorIcon::Crosshair, "Drag to select time"),
            };
            selection.on_hover_cursor(cursor).on_hover_text(tip);
        }
    }

    fn ruler_input(
        &mut self,
        ui: &egui::Ui,
        top: Rect,
        bottom: Rect,
        scale: Scale,
        w: &Arc<Window>,
    ) {
        // A busy frame can contain an entire drag. Process the native pointer events
        // in order and retain the press geometry until release, even outside the row.
        let events = ui.input(|i| {
            i.events
                .iter()
                .filter(|e| {
                    matches!(
                        e,
                        Event::PointerButton {
                            button: PointerButton::Primary,
                            ..
                        } | Event::PointerMoved(_)
                            | Event::WindowFocused(false)
                    )
                })
                .cloned()
                .collect::<Vec<_>>()
        });
        for event in events {
            match event {
                Event::PointerButton {
                    pressed: true,
                    pos,
                    modifiers,
                    ..
                } if ui.is_enabled()
                    && ui.clip_rect().contains(pos)
                    && (top.contains(pos) || bottom.contains(pos))
                    && ui.ctx().layer_id_at(pos) == Some(ui.layer_id()) =>
                {
                    let original = self.parallel.fixed.unwrap_or(w.range);
                    let gesture = if top.contains(pos) || modifiers.shift {
                        Gesture::Draw
                    } else {
                        Band::new(bottom, scale, original).gesture(pos)
                    };
                    self.parallel.overview_drag = Some(Brush {
                        domain: w.clone(),
                        original,
                        scale,
                        track: top,
                        origin: pos,
                        gesture,
                        moved: false,
                    });
                }
                Event::PointerMoved(pos)
                | Event::PointerButton {
                    pressed: false,
                    pos,
                    ..
                } => {
                    let Some(brush) = self.parallel.overview_drag.as_mut() else {
                        continue;
                    };
                    let axis = brush.scale.span();
                    let original = brush.original;
                    let edge = matches!(brush.gesture, Gesture::From | Gesture::Until);
                    let release = matches!(event, Event::PointerButton { .. });
                    if let Some(selected) = brush.sample(pos) {
                        self.parallel.held = Some(brush.domain.clone());
                        self.parallel.ruler = Some(axis);
                        if self.parallel.fixed != Some(selected) {
                            self.parallel.set_range(selected);
                        }
                    } else if release
                        && edge
                        && (original.until <= axis.from || original.from >= axis.until)
                    {
                        self.parallel.reveal(original);
                    }
                    if release {
                        self.parallel.overview_drag = None;
                    }
                }
                Event::WindowFocused(false) => self.parallel.overview_drag = None,
                _ => {}
            }
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            self.parallel.overview_drag = None;
        }
    }
}
