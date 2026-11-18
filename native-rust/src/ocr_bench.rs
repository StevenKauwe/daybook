use crate::{clipboard, screen};
use serde_json::{Value, json};
use std::time::Instant;

fn peak_rss() -> Result<i64, String> {
    let mut usage = unsafe { std::mem::zeroed::<libc::rusage>() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(usage.ru_maxrss)
}

pub fn fixture(args: &[String]) -> Result<(), String> {
    let [width, height, lines, path] = args else {
        return Err("Expected width height lines output-path".into());
    };
    let number = |s: &str| s.parse::<usize>().map_err(|e| e.to_string());
    let (width, height, lines) = (number(width)?, number(height)?, number(lines)?);
    if !(640..=4096).contains(&width) || !(360..=2160).contains(&height) || lines > 20 {
        return Err("Benchmark dimensions out of range".into());
    }
    unsafe {
        clipboard::write_png(
            &screen::drawn_image(width, height, lines)?,
            std::path::Path::new(path),
        )
    }
}

pub fn run(args: &[String]) -> Result<Value, String> {
    let [path, count] = args else {
        return Err("Expected PNG path and iterations".into());
    };
    let repeats = count.parse::<usize>().map_err(|e| e.to_string())?;
    if !(2..=20).contains(&repeats) {
        return Err("Benchmark repetitions out of range".into());
    }
    let image = unsafe { clipboard::load_image(std::path::Path::new(path))? };
    let before = peak_rss()?;
    let mut ms = Vec::with_capacity(repeats);
    let mut words = 0;
    for _ in 0..repeats {
        let start = Instant::now();
        let text = unsafe { screen::saved_text(&image)? };
        ms.push(start.elapsed().as_secs_f64() * 1000.0);
        words = text.split_whitespace().count();
    }
    Ok(json!({"words":words,"ms":ms,"before_peak_rss":before,"peak_rss":peak_rss()?}))
}
