//! The recent amplitude envelope. Only scalar levels and timestamps ever enter this view.
use super::{design, egui};
use egui::{Color32, Stroke};
const COLUMNS: usize = 64;
const TICKS_PER_SECOND: f64 = 20.;
const EMPTY: Bin = Bin {
    tick: i64::MIN,
    rms: 0.,
    peak: 0.,
};
#[derive(Clone, Copy)]
struct Bin {
    tick: i64,
    rms: f32,
    peak: f32,
}

pub struct Trace {
    bins: [Bin; COLUMNS],
    epoch: Option<u64>,
    last_at: Option<f64>,
}
impl Default for Trace {
    fn default() -> Self {
        Self {
            bins: [EMPTY; COLUMNS],
            epoch: None,
            last_at: None,
        }
    }
}
fn tick(at: f64) -> i64 {
    (at * TICKS_PER_SECOND).floor() as i64
}
fn amplitude(db: f32) -> f32 {
    ((db + 60.) / 60.).clamp(0., 1.)
}

impl Trace {
    pub fn clear(&mut self) {
        if self.last_at.is_some() {
            *self = Self::default();
        }
    }
    pub fn observe(&mut self, epoch: Option<u64>, at: f64, rms: f32, peak: f32) {
        if epoch != self.epoch {
            self.clear();
            self.epoch = epoch;
        }
        if self.last_at.is_some_and(|last| at <= last) {
            return;
        }
        self.last_at = Some(at);
        let t = tick(at);
        let slot = &mut self.bins[t.rem_euclid(COLUMNS as i64) as usize];
        if slot.tick != t {
            *slot = Bin { tick: t, ..EMPTY };
        }
        slot.rms = slot.rms.max(amplitude(rms));
        slot.peak = slot.peak.max(amplitude(peak));
    }
    fn bin(&self, tick: i64) -> Bin {
        let stored = self.bins[tick.rem_euclid(COLUMNS as i64) as usize];
        if stored.tick == tick { stored } else { EMPTY }
    }
    pub fn paint(
        &self,
        ui: &egui::Ui,
        rect: egui::Rect,
        now: f64,
        color: Color32,
        current: (f32, f32),
    ) {
        let p = ui.painter_at(rect);
        let grid = Color32::from_rgb(52, 76, 71);
        let gauge = egui::Rect::from_min_max(
            egui::pos2(rect.right() - 14., rect.top()),
            rect.right_bottom(),
        );
        let graph =
            egui::Rect::from_min_max(rect.min, egui::pos2(gauge.left() - 12., rect.bottom()));
        if graph.width() <= 0. {
            return;
        }
        let center = graph.center().y;
        let width = graph.width() / COLUMNS as f32;
        let radius = (graph.height() / 2. - 2.).max(1.);
        for i in 0..=8 {
            let x = graph.left() + graph.width() * i as f32 / 8.;
            p.line_segment(
                [egui::pos2(x, graph.top()), egui::pos2(x, graph.bottom())],
                Stroke::new(0.55, grid),
            );
        }
        p.hline(graph.x_range(), center, Stroke::new(0.8, grid));
        let latest = tick(now);
        let mut moving = false;
        for col in 0..COLUMNS {
            let b = self.bin(latest - (COLUMNS - 1 - col) as i64);
            if b.peak <= 0. {
                continue;
            }
            moving = true;
            let x = graph.left() + (col as f32 + 0.5) * width;
            let height = radius * b.peak;
            p.vline(
                x,
                center - height..=center + height,
                Stroke::new((width * 0.71).clamp(1., 4.), color.gamma_multiply(0.42)),
            );
            let inner = (radius * b.rms).max(0.65);
            p.vline(
                x,
                center - inner..=center + inner,
                Stroke::new((width * 0.62).clamp(1., 3.4), color),
            );
        }
        // The small meter on the right reacts immediately, independently of the history bins.
        for i in 0..7 {
            let step = gauge.height() / 7.;
            let y = gauge.bottom() - step * (i as f32 + 1.);
            let bar = egui::Rect::from_min_size(
                egui::pos2(gauge.left(), y),
                egui::vec2(gauge.width(), (step - 1.4).max(1.)),
            );
            p.rect_filled(
                bar,
                1,
                if current.0 * 7. > i as f32 {
                    color
                } else {
                    grid
                },
            );
        }
        if current.1 > 0. {
            p.hline(
                gauge.x_range(),
                gauge.bottom() - (gauge.height() - 2.) * current.1 - 1.,
                Stroke::new(1., design::BG),
            );
        }
        if moving {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_sessions_and_elapsed_time_never_create_signal() {
        let mut t = Trace::default();
        t.observe(Some(4), 100., -18., -5.);
        t.observe(Some(4), 100.01, -30., -12.);
        assert_eq!(t.bin(tick(100.)).peak, amplitude(-5.));
        t.observe(Some(4), 100.05, -120., -120.);
        assert_eq!(t.bin(tick(100.05)).peak, 0.);
        assert_eq!(t.bin(tick(103.2)).peak, 0.);
        t.observe(Some(5), 101., -120., -120.);
        assert_eq!(t.bin(tick(100.)).peak, 0.);
        t.observe(Some(5), 101.06, -9., -2.);
        t.clear();
        assert!(t.last_at.is_none());
        assert_eq!(t.bin(tick(101.06)).peak, 0.);
    }
}
