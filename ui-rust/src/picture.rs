use eframe::egui;
use image::{ImageDecoder, ImageReader};
use std::{
    collections::VecDeque,
    io::{Cursor, Read},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::Duration,
};

struct Picture {
    pixels: egui::ColorImage,
    original: [u32; 2],
}
type Loaded = Result<Picture, String>;

fn decode(bytes: &[u8], side: u32) -> Loaded {
    let mut decoder = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > 40_000_000 {
        return Err("Image dimensions exceed the viewer limit".into());
    }
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut image = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    image.apply_orientation(orientation);
    let original = [image.width(), image.height()];
    if original[0].max(original[1]) > side {
        image = image.thumbnail(side, side);
    }
    let pixels = image.into_rgba8();
    Ok(Picture {
        pixels: egui::ColorImage::from_rgba_unmultiplied(
            [pixels.width() as usize, pixels.height() as usize],
            &pixels,
        ),
        original,
    })
}

fn fetch(base: &str, id: i64, side: u32) -> Loaded {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .redirects(0)
        .build();
    let response = agent
        .get(&format!("{base}/image/{id}"))
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(410, _) => "This image is no longer available.".to_owned(),
            _ => "Couldn’t load the image from Daybook.".to_owned(),
        })?;
    let mut bytes = Vec::new();
    const MAX: u64 = 64 * 1024 * 1024;
    response
        .into_reader()
        .take(MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX {
        return Err("Image exceeds the viewer limit".into());
    }
    decode(&bytes, side)
}

fn load(ctx: &egui::Context, base: &str, id: i64, side: u32) -> Receiver<Loaded> {
    let (tx, rx) = mpsc::channel();
    let base = base.to_owned();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(fetch(&base, id, side));
        ctx.request_repaint();
    });
    rx
}

/// Share small previews between visible Archive rows and source lists.
#[derive(Default)]
pub struct Thumbnails {
    cache: VecDeque<(i64, Option<egui::TextureHandle>)>,
    loading: Option<(i64, Receiver<Loaded>)>,
}
impl Thumbnails {
    pub fn paint(&mut self, ui: &egui::Ui, rect: egui::Rect, base: &str, id: i64) {
        if !ui.is_rect_visible(rect) {
            return;
        }
        if let Some((loading_id, rx)) = &self.loading {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                result => {
                    let texture = result.ok().and_then(Result::ok).map(|image| {
                        ui.ctx().load_texture(
                            format!("thumbnail-{loading_id}"),
                            image.pixels,
                            egui::TextureOptions::LINEAR,
                        )
                    });
                    self.cache.push_back((*loading_id, texture));
                    if self.cache.len() > 32 {
                        self.cache.pop_front();
                    }
                    self.loading = None;
                }
            }
        }
        if let Some(i) = self.cache.iter().position(|(key, _)| *key == id) {
            let entry = self.cache.remove(i).unwrap();
            if let Some(tex) = &entry.1 {
                let size = tex.size_vec2();
                let fit = (rect.width() / size.x).min(rect.height() / size.y).min(1.);
                ui.painter().image(
                    tex.id(),
                    egui::Rect::from_min_size(rect.min, size * fit),
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1., 1.)),
                    egui::Color32::WHITE,
                );
            } else {
                ui.painter().text(
                    rect.left_center(),
                    egui::Align2::LEFT_CENTER,
                    "Preview unavailable",
                    egui::FontId::proportional(13.),
                    super::MUTED,
                );
            }
            self.cache.push_back(entry);
        } else {
            ui.painter()
                .rect_filled(rect, 3., super::design::LINE.gamma_multiply(0.5));
            if self.loading.is_none() {
                self.loading = Some((id, load(ui.ctx(), base, id, 256)));
            }
        }
    }
}

#[derive(Default)]
pub struct Viewer {
    id: Option<i64>,
    loading: Option<(i64, Receiver<Loaded>)>,
    texture: Option<(egui::TextureHandle, [u32; 2])>,
    error: Option<String>,
    zoom: bool,
}
impl Viewer {
    pub fn clear(&mut self) {
        self.id = None;
        self.texture = None;
        self.error = None;
        self.zoom = false;
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        let Some((id, rx)) = &self.loading else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Image decoding stopped.".into()),
        };
        if self.id == Some(*id) {
            match result {
                Ok(image) => {
                    self.texture = Some((
                        ctx.load_texture(
                            format!("archive-{id}"),
                            image.pixels,
                            egui::TextureOptions::LINEAR,
                        ),
                        image.original,
                    ))
                }
                Err(e) => self.error = Some(e),
            }
        }
        self.loading = None;
    }
    pub fn show(&mut self, ui: &mut egui::Ui, base: &str, id: i64) {
        if self.id != Some(id) {
            self.clear();
            self.id = Some(id);
        }
        if let Some((texture, original)) = &self.texture {
            let available = egui::vec2(
                ui.available_width().max(80.),
                (ui.ctx().content_rect().height() * 0.48).clamp(150., 520.),
            );
            let size = texture.size_vec2();
            let scale = (available.x / size.x).min(available.y / size.y).min(1.);
            ui.horizontal(|ui| {
                ui.weak(format!("{} × {} pixels", original[0], original[1]));
                if ui
                    .small_button(if self.zoom { "Fit image" } else { "Zoom in" })
                    .clicked()
                {
                    self.zoom = !self.zoom;
                }
            });
            if self.zoom {
                egui::ScrollArea::both()
                    .id_salt(("image_zoom", id))
                    .max_width(available.x)
                    .max_height(available.y)
                    .show(ui, |ui| {
                        ui.add(egui::Image::new((texture.id(), size)));
                    });
            } else {
                ui.add(egui::Image::new((texture.id(), size * scale)));
            }
        } else if let Some(error) = &self.error {
            ui.label(error);
            if ui.small_button("Try loading again").clicked() {
                self.error = None;
            }
        } else {
            ui.label("Loading image…");
            if self.loading.is_none() {
                let side = ui.ctx().input(|i| i.max_texture_side).clamp(1, 4096) as u32;
                self.loading = Some((id, load(ui.ctx(), base, id, side)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_images_decode_at_display_size_and_late_results_cannot_replace_the_selection() {
        for format in [image::ImageFormat::Png, image::ImageFormat::Jpeg] {
            let input = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(80, 40, |x, _| {
                if x < 40 {
                    image::Rgb([220, 30, 20])
                } else {
                    image::Rgb([20, 150, 190])
                }
            }));
            let mut file = Cursor::new(Vec::new());
            input.write_to(&mut file, format).unwrap();
            let output = decode(file.get_ref(), 40).unwrap();
            assert_eq!(output.original, [80, 40]);
            assert_eq!(output.pixels.size, [40, 20]);
            assert!(output.pixels.pixels[4].r() > 190 && output.pixels.pixels[35].b() > 155);
            let ctx = egui::Context::default();
            let mut viewer = Viewer::default();
            let (tx, rx) = mpsc::channel();
            viewer.id = Some(10);
            viewer.loading = Some((9, rx));
            tx.send(Ok(output)).ok().unwrap();
            viewer.poll(&ctx);
            assert!(viewer.texture.is_none() && viewer.loading.is_none());
            let (tx, rx) = mpsc::channel();
            viewer.loading = Some((10, rx));
            tx.send(decode(file.get_ref(), 40)).ok().unwrap();
            viewer.poll(&ctx);
            let texture = viewer.texture.as_ref().unwrap().0.id();
            let out = ctx.run_ui(Default::default(), |ui| {
                viewer.show(ui, "synthetic://image", 10)
            });
            let rendered = out.shapes.iter().any(|s| s.shape.texture_id() == texture);
            out.drop_without_applying_deltas();
            assert!(
                rendered,
                "The normal entry viewer must paint the selected image"
            );
        }
    }
}
