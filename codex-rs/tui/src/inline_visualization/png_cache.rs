//! Immutable content-addressed storage for prepared inline PNGs.

use image::DynamicImage;
use image::ImageFormat;
use image::RgbaImage;
use sha2::Digest as _;
use sha2::Sha256;
use std::fs;
use std::io::Cursor;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use tempfile::NamedTempFile;

#[derive(Clone, Debug)]
pub(super) struct CachedPng {
    pub(super) path: PathBuf,
    pub(super) digest: [u8; 32],
    pub(super) width: u32,
    pub(super) height: u32,
}

pub(super) fn persist(image: RgbaImage, cache_dir: &Path) -> Option<CachedPng> {
    let width = image.width();
    let height = image.height();
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .ok()?;
    let hash = Sha256::digest(&bytes);
    fs::create_dir_all(cache_dir).ok()?;
    let path = cache_dir.join(format!("{hash:x}.png"));
    if path.is_file() {
        if fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
            return None;
        }
    } else {
        let mut staged = NamedTempFile::new_in(cache_dir).ok()?;
        staged.write_all(&bytes).ok()?;
        staged.flush().ok()?;
        if let Err(error) = staged.persist_noclobber(&path)
            && (error.error.kind() != std::io::ErrorKind::AlreadyExists
                || fs::read(&path).ok().as_deref() != Some(bytes.as_slice()))
        {
            return None;
        }
    }
    Some(CachedPng {
        path,
        digest: hash.into(),
        width,
        height,
    })
}
