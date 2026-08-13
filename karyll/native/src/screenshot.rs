//! Capturing the screen to a PNG.
//!
//! Ported from `sidle/native`. The point is not the feature — it is that the
//! interface can be *looked at* off the device. Without it, every change to
//! layout, type size or hit targets is a guess that costs a round trip and a
//! reboot to test.
//!
//! The capture itself is cheap: the window's backing store already holds
//! exactly what is on screen, so a screenshot is just encoding it.
//!
//! Files land in `/mnt/us/screenshots`, the same directory stock Kindle
//! screenshots use, so they appear where expected over USB.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::window::Window;

const DIR: &str = "/mnt/us/screenshots";

/// Write the current screen to a timestamped PNG and return its path.
///
/// Best-effort by construction: callers should log a failure rather than treat
/// it as fatal, because failing to save a screenshot is no reason to lose the
/// document.
pub fn capture(window: &Window) -> Result<PathBuf> {
    let dir = Path::new(DIR);
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("karyll_{secs}.png"));
    write_png(window, &path)?;
    eprintln!("screenshot: {}", path.display());
    Ok(path)
}

/// Encode the backing store as an 8-bit grayscale PNG.
///
/// Grayscale rather than RGB because that is what the backing store is — one
/// byte of luminance per pixel — so there is nothing to convert and the file is
/// a third of the size.
fn write_png(window: &Window, path: &Path) -> Result<()> {
    let (width, height) = (window.width() as u32, window.height() as u32);
    let image = image::GrayImage::from_raw(width, height, window.pixels().to_vec())
        .context("backing store size does not match the window")?;
    image
        .save_with_format(path, image::ImageFormat::Png)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshots_go_where_the_firmware_puts_its_own() {
        // So they turn up in the expected folder over USB rather than somewhere
        // only this app knows about.
        assert_eq!(DIR, "/mnt/us/screenshots");
    }
}
