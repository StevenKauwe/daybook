//! Serve original journal images to the controls without exporting a file or blocking the recorder.
use crate::store::Result;
use rusqlite::{Connection, MAIN_DB, OpenFlags};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};
use tiny_http::{Header, Request, Response, StatusCode};

const MAX_FILE: usize = 64 * 1024 * 1024;
pub struct Images(mpsc::SyncSender<(i64, Request)>);

impl Images {
    pub fn new(root: PathBuf) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<(i64, Request)>(1);
        thread::Builder::new()
            .name("archive-images".into())
            .spawn(move || {
                for (id, request) in rx {
                    let Ok(db) = Connection::open_with_flags(
                        root.join("daybook.sqlite"),
                        OpenFlags::SQLITE_OPEN_READ_ONLY,
                    ) else {
                        let _ = request.respond(Response::empty(503));
                        continue;
                    };
                    match original(&db, &root, id) {
                        Ok((reader, mime, len)) => {
                            let headers = vec![
                                Header::from_bytes("Content-Type", mime).unwrap(),
                                Header::from_bytes("Cache-Control", "no-store").unwrap(),
                                Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap(),
                            ];
                            let _ = request.respond(Response::new(
                                StatusCode(200),
                                headers,
                                reader,
                                Some(len),
                                None,
                            ));
                        }
                        Err(_) => {
                            let _ = request.respond(Response::empty(410));
                        }
                    };
                }
            })?;
        Ok(Self(tx))
    }

    pub fn send(&self, suffix: &str, request: Request) {
        let Some(id) = suffix.parse::<i64>().ok().filter(|id| *id > 0) else {
            let _ = request.respond(Response::empty(404));
            return;
        };
        if let Err(e) = self.0.try_send((id, request)) {
            let (mpsc::TrySendError::Full((_, request))
            | mpsc::TrySendError::Disconnected((_, request))) = e;
            let _ = request.respond(Response::empty(503));
        }
    }
}

fn original<'a>(
    db: &'a Connection,
    root: &Path,
    id: i64,
) -> Result<(Box<dyn Read + 'a>, &'static str, usize)> {
    let (saved, relative): (bool, Option<String>) = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM saved_images WHERE observation_id=?1),json_extract(details,'$.path')
         FROM observations WHERE id=?1 AND kind='screen'", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    if saved {
        let blob = db.blob_open(MAIN_DB, "saved_images", "png", id, true)?;
        let len = blob.len();
        if len == 0 || len > MAX_FILE {
            return Err("Invalid saved image size".into());
        }
        return Ok((Box::new(blob), "image/png", len));
    }
    let relative = relative.ok_or("Image unavailable")?;
    if !crate::worker::valid_relative(&relative, "images") {
        return Err("Invalid image path".into());
    }
    let path = root.join(&relative);
    let directory = root.join("images");
    if path.is_symlink() || directory.is_symlink() || !path.is_file() {
        return Err("Image unavailable".into());
    }
    let mime = match path.extension().and_then(|s| s.to_str()) {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        _ => return Err("Unsupported image type".into()),
    };
    use std::os::unix::fs::OpenOptionsExt;
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let len = usize::try_from(file.metadata()?.len())?;
    if len == 0 || len > MAX_FILE {
        return Err("Invalid image size".into());
    }
    Ok((Box::new(file), mime, len))
}
