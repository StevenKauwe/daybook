//! Type, color and marks shared by the recorder, reading surface and navigation.
use eframe::egui::{
    self, Align2, Color32, FontFamily, FontId, Pos2, Rect, RichText, Stroke, StrokeKind,
};

pub const BG: Color32 = Color32::from_rgb(246, 245, 238);
pub const CARD: Color32 = Color32::from_rgb(255, 254, 249);
pub const TEXT: Color32 = Color32::from_rgb(31, 43, 42);
pub const MUTED: Color32 = Color32::from_rgb(80, 101, 95);
pub const LINE: Color32 = Color32::from_rgb(207, 215, 202);
pub const MINT: Color32 = Color32::from_rgb(13, 102, 83);
pub const VIOLET: Color32 = Color32::from_rgb(98, 67, 133);
pub const CORAL: Color32 = Color32::from_rgb(168, 55, 38);
pub const INK: Color32 = Color32::from_rgb(19, 34, 35);
pub const CONSOLE: Color32 = Color32::from_rgb(25, 45, 45);
pub const SIGNAL: Color32 = Color32::from_rgb(132, 241, 193);
pub const LILAC: Color32 = Color32::from_rgb(211, 184, 253);
pub const DIM: Color32 = Color32::from_rgb(169, 192, 180);
pub const AMBER: Color32 = Color32::from_rgb(249, 168, 120);
const NAV_ACTIVE: Color32 = Color32::from_rgb(42, 72, 65);

pub fn serif(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("Journal".into()))
}

#[derive(Clone, Copy)]
pub struct Paper {
    pub ground: Color32,
    pub sheet: Color32,
    pub ink: Color32,
    pub muted: Color32,
    pub line: Color32,
    pub mine: Color32,
    pub remote: Color32,
    pub fault: Color32,
}
pub fn paper(night: bool) -> Paper {
    if night {
        Paper {
            ground: Color32::from_rgb(15, 26, 28),
            sheet: INK,
            ink: BG,
            muted: DIM,
            line: Color32::from_rgb(51, 73, 70),
            mine: SIGNAL,
            remote: LILAC,
            fault: AMBER,
        }
    } else {
        Paper {
            ground: BG,
            sheet: CARD,
            ink: TEXT,
            muted: MUTED,
            line: LINE,
            mine: MINT,
            remote: VIOLET,
            fault: CORAL,
        }
    }
}
pub fn passage_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("Passage".into()))
}

pub fn setup(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let journal = FontFamily::Name("Journal".into());
    fonts.families.insert(
        journal.clone(),
        fonts.families[&FontFamily::Proportional].clone(),
    );
    let passage = FontFamily::Name("Passage".into());
    fonts.families.insert(
        passage.clone(),
        fonts.families[&FontFamily::Proportional].clone(),
    );
    for (name, family, file) in [
        ("System", FontFamily::Proportional, "SFNS.ttf"),
        ("Journal", journal, "NewYork.ttf"),
        ("Passage", passage, "Supplemental/Georgia.ttf"),
        ("Instrument", FontFamily::Monospace, "SFNSMono.ttf"),
    ] {
        if let Ok(bytes) = std::fs::read(std::path::Path::new("/System/Library/Fonts").join(file)) {
            fonts
                .font_data
                .insert(name.into(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, name.into());
        }
    }
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::ThemePreference::Light);
    let mut s = (*ctx.style_of(egui::Theme::Light)).clone();
    let v = &mut s.visuals;
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.extreme_bg_color = CARD;
    v.faint_bg_color = Color32::from_rgb(237, 239, 230);
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = MINT;
    v.selection.bg_fill = Color32::from_rgb(188, 222, 204);
    v.selection.stroke = Stroke::new(1.0, TEXT);
    v.window_stroke = Stroke::new(1.0, LINE);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = 7.into();
        w.bg_stroke = Stroke::new(1.0, LINE);
        w.fg_stroke = Stroke::new(1.2, TEXT);
    }
    v.widgets.inactive.bg_fill = CARD;
    v.widgets.inactive.weak_bg_fill = CARD;
    v.widgets.hovered.bg_fill = Color32::from_rgb(224, 235, 223);
    v.widgets.hovered.weak_bg_fill = v.widgets.hovered.bg_fill;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, MINT);
    v.widgets.active.bg_fill = Color32::from_rgb(203, 225, 207);
    v.widgets.active.weak_bg_fill = v.widgets.active.bg_fill;
    v.widgets.open.bg_fill = v.widgets.active.bg_fill;
    v.widgets.open.weak_bg_fill = v.widgets.active.bg_fill;
    s.spacing.item_spacing = egui::vec2(8.0, 8.0);
    s.spacing.button_padding = egui::vec2(12.0, 6.0);
    s.spacing.interact_size.y = 30.0;
    s.animation_time = 0.14;
    for (kind, size) in [
        (egui::TextStyle::Small, 12.0),
        (egui::TextStyle::Body, 15.0),
        (egui::TextStyle::Button, 14.0),
        (egui::TextStyle::Heading, 22.0),
    ] {
        s.text_styles.insert(kind, FontId::proportional(size));
    }
    let mut dark = s.clone();
    dark.visuals = egui::Visuals::dark();
    let night = paper(true);
    let v = &mut dark.visuals;
    v.panel_fill = night.ground;
    v.window_fill = night.sheet;
    v.extreme_bg_color = INK;
    v.override_text_color = Some(night.ink);
    v.hyperlink_color = SIGNAL;
    v.selection.bg_fill = NAV_ACTIVE;
    v.selection.stroke = Stroke::new(1.0, SIGNAL);
    v.window_stroke = Stroke::new(1., night.line);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = 7.into();
        w.fg_stroke = Stroke::new(1.2, night.ink);
        w.bg_stroke = Stroke::new(1., night.line);
    }
    v.widgets.inactive.weak_bg_fill = CONSOLE;
    v.widgets.inactive.bg_fill = CONSOLE;
    v.widgets.hovered.weak_bg_fill = NAV_ACTIVE;
    v.widgets.hovered.bg_fill = NAV_ACTIVE;
    ctx.set_style_of(egui::Theme::Dark, dark);
    ctx.set_style_of(egui::Theme::Light, s);
}

#[derive(Clone, Copy)]
pub enum Icon {
    Live,
    Archive,
    Brief,
    Health,
    Settings,
    Mic,
    System,
    Note,
    Focus,
    Lock,
    Book,
}

pub fn icon(p: &egui::Painter, center: Pos2, size: f32, glyph: Icon, color: Color32) {
    let xy = |x: f32, y: f32| center + egui::vec2(x - 12.0, y - 12.0) * (size / 24.0);
    let stroke = Stroke::new((size / 13.0).max(1.15), color);
    let line = |coords: &[(f32, f32)]| {
        p.add(egui::Shape::line(
            coords.iter().map(|&(x, y)| xy(x, y)).collect(),
            stroke,
        ))
    };
    let rect = |x, y, w, h, r| {
        p.rect_stroke(
            Rect::from_min_max(xy(x, y), xy(x + w, y + h)),
            r,
            stroke,
            StrokeKind::Middle,
        )
    };
    match glyph {
        Icon::Live => {
            line(&[
                (1., 12.),
                (5., 12.),
                (8., 5.),
                (12., 19.),
                (16., 7.),
                (18., 12.),
                (23., 12.),
            ]);
        }
        Icon::Archive => {
            rect(3., 5., 18., 4., 1);
            rect(5., 9., 14., 11., 2);
            line(&[(10., 13.), (14., 13.)]);
        }
        Icon::Brief => {
            rect(5., 3., 14., 18., 2);
            line(&[(8., 8.), (16., 8.)]);
            line(&[(8., 12.), (16., 12.)]);
            line(&[(8., 16.), (13., 16.)]);
        }
        Icon::Health => {
            line(&[(3., 3.), (3., 20.), (22., 20.)]);
            line(&[(6., 15.), (10., 10.), (14., 13.), (20., 5.)]);
        }
        Icon::Settings => {
            for (y, x) in [(6., 9.), (12., 16.), (18., 7.)] {
                line(&[(3., y), (x - 2., y)]);
                p.circle_stroke(xy(x, y), size / 12.0, stroke);
                line(&[(x + 2., y), (21., y)]);
            }
        }
        Icon::Mic => {
            rect(9., 2., 6., 13., 3);
            line(&[
                (5., 10.),
                (5., 13.),
                (7., 16.),
                (10., 18.),
                (14., 18.),
                (17., 16.),
                (19., 13.),
                (19., 10.),
            ]);
            line(&[(12., 18.), (12., 22.)]);
            line(&[(9., 22.), (15., 22.)]);
        }
        Icon::System => {
            rect(2., 4., 20., 13., 2);
            line(&[(12., 17.), (12., 21.)]);
            line(&[(8., 21.), (16., 21.)]);
        }
        Icon::Note => {
            line(&[
                (4., 16.),
                (16., 4.),
                (20., 8.),
                (8., 20.),
                (3., 21.),
                (4., 16.),
            ]);
            line(&[(14., 6.), (18., 10.)]);
        }
        Icon::Focus => {
            for (x, y, a, b) in [
                (3., 3., 1., 1.),
                (21., 3., -1., 1.),
                (3., 21., 1., -1.),
                (21., 21., -1., -1.),
            ] {
                line(&[(x + 5. * a, y), (x, y), (x, y + 5. * b)]);
            }
        }
        Icon::Lock => {
            rect(5., 10., 14., 11., 2);
            line(&[
                (8., 10.),
                (8., 6.),
                (10., 3.),
                (14., 3.),
                (16., 6.),
                (16., 10.),
            ]);
            line(&[(12., 14.), (12., 17.)]);
        }
        Icon::Book => {
            rect(4., 3., 17., 18., 2);
            line(&[(8., 3.), (8., 21.)]);
            line(&[(12., 8.), (17., 8.)]);
        }
    }
}

pub fn brand(ui: &mut egui::Ui, compact: bool) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(34.0, 34.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(r, 10, Color32::from_rgb(211, 236, 207));
        let center = r.center();
        icon(p, center, 25.0, Icon::Live, INK);
        p.line_segment(
            [
                r.left_top() + egui::vec2(8., 5.),
                r.left_top() + egui::vec2(21., 5.),
            ],
            Stroke::new(1., MINT),
        );
        if !compact {
            ui.add_space(2.0);
            ui.label(RichText::new("Daybook").size(24.0).strong().color(BG));
        }
    });
}

pub fn nav(
    ui: &mut egui::Ui,
    glyph: Icon,
    name: &str,
    selected: bool,
    compact: bool,
    hint: &str,
) -> egui::Response {
    let (r, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, name)
    });
    let p = ui.painter();
    let fg = if selected {
        SIGNAL
    } else if response.hovered() {
        BG
    } else {
        DIM
    };
    if selected || response.hovered() || response.has_focus() {
        p.rect_filled(r, 8, if selected { NAV_ACTIVE } else { CONSOLE });
    }
    if response.has_focus() {
        p.rect_stroke(
            r.shrink(1.0),
            8,
            Stroke::new(1.3, SIGNAL),
            StrokeKind::Inside,
        );
    }
    let center = if compact {
        r.center()
    } else {
        egui::pos2(r.left() + 19., r.center().y)
    };
    icon(p, center, 19., glyph, fg);
    if !compact {
        p.text(
            egui::pos2(r.left() + 38., r.center().y),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(15.),
            fg,
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("{name}   {hint}"))
}

// Static contour artwork for the binder spine, independent of incoming signal.
pub fn imprint(ui: &mut egui::Ui) {
    use std::sync::LazyLock;
    static LINES: LazyLock<Vec<Vec<egui::Vec2>>> = LazyLock::new(|| {
        (0..23)
            .map(|layer| {
                let l = layer as f32 / 22.;
                (0..=96)
                    .map(|step| {
                        let t = step as f32 * std::f32::consts::TAU / 96.;
                        let radius = (1. - (2. * l - 1.).powi(2)).max(0.).sqrt();
                        let x = radius * (t.cos() + 0.15 * (3. * t + 4. * l).cos());
                        let y = radius * (t.sin() * 0.23 + 0.13 * (2. * t + 5. * l).sin())
                            + 0.96 * (l - 0.5);
                        egui::vec2(x * 57. + y * 22., y * 91. - x * 14.)
                    })
                    .collect()
            })
            .collect()
    });
    let (r, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 126.), egui::Sense::hover());
    let p = ui.painter_at(r);
    let c = r.center();
    for (i, curve) in LINES.iter().enumerate() {
        let t = i as f32 / (LINES.len() - 1) as f32;
        let color = Color32::from_rgb(
            (71. + 74. * t) as u8,
            (112. + 57. * t) as u8,
            (102. + 32. * t) as u8,
        );
        p.add(egui::Shape::line(
            curve.iter().map(|point| c + *point).collect(),
            Stroke::new(0.8, color),
        ));
    }
    let star = c + egui::vec2(50., -44.);
    p.line_segment(
        [star - egui::vec2(4., 0.), star + egui::vec2(4., 0.)],
        Stroke::new(1., AMBER),
    );
    p.line_segment(
        [star - egui::vec2(0., 4.), star + egui::vec2(0., 4.)],
        Stroke::new(1., AMBER),
    );
}

pub fn icon_slot(ui: &mut egui::Ui, glyph: Icon, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    icon(ui.painter(), rect.center(), size, glyph, color);
}

pub fn eyebrow(ui: &mut egui::Ui, label: &str, color: Color32) {
    ui.label(
        RichText::new(label)
            .font(FontId::monospace(11.))
            .color(color),
    );
}
