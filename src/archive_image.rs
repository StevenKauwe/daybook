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

enum Original {
    Saved(i64),
    Retained(i64, PathBuf, &'static str),
}

fn locate(db: &Connection, root: &Path, id: i64) -> Result<Option<Original>> {
    // A generated screen description can refer to the original capture by observation ID.
    let mut q = db.prepare_cached("SELECT id,EXISTS(SELECT 1 FROM saved_images WHERE observation_id=o.id),
        json_extract(details,'$.path') FROM observations o WHERE kind='screen' AND id IN
        (?1,(SELECT json_extract(details,'$.observation_id') FROM observations WHERE id=?1 AND kind='screen' AND source='vision'))
        ORDER BY (id=?1)")?;
    let mut rows = q.query([id])?;
    while let Some(row) = rows.next()? {
        let image_id: i64 = row.get(0)?;
        if row.get::<_, bool>(1)? {
            return Ok(Some(Original::Saved(image_id)));
        }
        let Some(relative) = row.get::<_, Option<String>>(2)? else {
            continue;
        };
        if !crate::worker::valid_relative(&relative, "images") {
            continue;
        }
        let path = root.join(relative);
        let mime = match path.extension().and_then(|s| s.to_str()) {
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("png") => "image/png",
            _ => continue,
        };
        if !root.join("images").is_symlink() && !path.is_symlink() && path.is_file() {
            return Ok(Some(Original::Retained(image_id, path, mime)));
        }
    }
    Ok(None)
}

pub fn retained_id(db: &Connection, root: &Path, id: i64) -> Result<Option<i64>> {
    Ok(locate(db, root, id)?.map(|original| match original {
        Original::Saved(id) | Original::Retained(id, ..) => id,
    }))
}

fn original<'a>(
    db: &'a Connection,
    root: &Path,
    id: i64,
) -> Result<(Box<dyn Read + 'a>, &'static str, usize)> {
    let original = locate(db, root, id)?.ok_or("Image unavailable")?;
    let (path, mime) = match original {
        Original::Saved(image_id) => {
            let blob = db.blob_open(MAIN_DB, "saved_images", "png", image_id, true)?;
            let len = blob.len();
            if len == 0 || len > MAX_FILE {
                return Err("Invalid saved image size".into());
            }
            return Ok((Box::new(blob), "image/png", len));
        }
        Original::Retained(_, path, mime) => (path, mime),
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
