use ring::digest::{self, Digest, SHA256};
use speechworks_inference::{Error, LoadOptions, Model, Result};
use std::{fs::File, os::unix::fs::FileExt, path::Path};

const MODEL_BYTE_LIMIT: u64 = 512 * 1024 * 1024;

// The descriptor pins the selected file across renames or atomic replacements.
// On device loss we verify the original bytes before rebuilding CPU weights.
struct ModelFile {
    file: File,
    length: usize,
    digest: Digest,
}

fn io_error(e: std::io::Error) -> Error {
    Error::Model(format!("reading the local speech model: {e}"))
}

impl ModelFile {
    fn open(path: &Path) -> Result<(Self, Vec<u8>)> {
        let file = File::open(path).map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MODEL_BYTE_LIMIT {
            return Err(Error::Model(
                "Choose a nonempty local model file of up to 512 MiB".into(),
            ));
        }
        let length = metadata.len() as usize;
        let mut bytes = vec![0; length];
        file.read_exact_at(&mut bytes, 0).map_err(io_error)?;
        let digest = digest::digest(&SHA256, &bytes);
        Ok((
            Self {
                file,
                length,
                digest,
            },
            bytes,
        ))
    }

    fn reload(&self) -> Result<Vec<u8>> {
        if self.file.metadata().map_err(io_error)?.len() == self.length as u64 {
            let mut bytes = vec![0; self.length];
            self.file.read_exact_at(&mut bytes, 0).map_err(io_error)?;
            if digest::digest(&SHA256, &bytes).as_ref() == self.digest.as_ref() {
                return Ok(bytes);
            }
        }
        Err(Error::Model(
            "The model file changed while it was loaded. Reopen Daybook to load the new file."
                .into(),
        ))
    }
}

pub fn load(path: &Path, options: LoadOptions) -> Result<Model> {
    let (source, bytes) = ModelFile::open(path)?;
    Ok(pollster::block_on(Model::from_bytes_with(bytes, options))?
        .with_recovery_loader(move || source.reload()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_uses_the_open_file_and_detects_in_place_edits() {
        let dir = std::env::temp_dir().join(format!("daybook-model-fd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("model");
        std::fs::write(&path, b"original").unwrap();
        let (source, first) = ModelFile::open(&path).unwrap();
        let moved = dir.join("renamed");
        std::fs::rename(&path, &moved).unwrap();
        std::fs::write(&path, b"new file").unwrap();
        assert_eq!(source.reload().unwrap(), first);
        std::fs::write(&moved, b"modified").unwrap();
        assert!(source.reload().is_err());
        std::fs::write(&moved, b"x").unwrap();
        assert!(source.reload().is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
