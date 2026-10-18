//! Small shared helpers: background tasks, file kinds, OS integration, formatting.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use dropzone_core::{archive, checksum, image_ops, media, pdf};
use eframe::egui::{self, Color32};

/// One-shot background computation whose result is picked up on a later frame.
pub struct Task<T> {
    rx: Option<Receiver<Result<T, String>>>,
}

impl<T> Default for Task<T> {
    fn default() -> Self {
        Self { rx: None }
    }
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn(ctx: &egui::Context, f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("internal error")))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        Self { rx: Some(rx) }
    }

    pub fn running(&self) -> bool {
        self.rx.is_some()
    }

    /// Returns the result once, when it arrives.
    pub fn poll(&mut self) -> Option<Result<T, String>> {
        let rx = self.rx.as_ref()?;
        match rx.try_recv() {
            Ok(v) => {
                self.rx = None;
                Some(v)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                Some(Err("background task stopped".into()))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    Audio,
    Pdf,
    Archive,
    ChecksumList,
    Folder,
    Other,
}

impl Kind {
    /// Classifies by name alone (no disk access), for listings that already know `is_dir`.
    pub fn of_name(name: &str, is_dir: bool) -> Kind {
        let p = Path::new(name);
        if is_dir {
            Kind::Folder
        } else if image_ops::is_image(p) {
            Kind::Image
        } else if media::is_video(p) {
            Kind::Video
        } else if media::is_audio(p) {
            Kind::Audio
        } else if pdf::is_pdf(p) {
            Kind::Pdf
        } else if archive::is_archive(p) {
            Kind::Archive
        } else if checksum::is_checksum_list(p) {
            Kind::ChecksumList
        } else {
            Kind::Other
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Pdf => "PDF",
            Kind::Archive => "archive",
            Kind::ChecksumList => "checksums",
            Kind::Folder => "folder",
            Kind::Other => "file",
        }
    }
}

/// A fixed-size table cell with its content aligned left (`add_sized` centers).
pub fn cell(ui: &mut egui::Ui, size: impl Into<egui::Vec2>, widget: impl egui::Widget) -> egui::Response {
    let size = size.into();
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(size);
        ui.add(widget)
    })
    .inner
}

pub fn human_size(bytes: u64) -> String {
    dropzone_core::util::human_size(bytes)
}

pub fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// Opens a file or folder with its default application.
pub fn open_path(p: &Path) {
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(p).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(p).spawn();
    let _ = r;
}

/// Shows a file selected in the system file manager.
pub fn reveal(p: &Path) {
    #[cfg(windows)]
    {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(p);
        let _ = std::process::Command::new("explorer").arg(arg).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(p).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if p.is_dir() { p.to_path_buf() } else { p.parent().map(Path::to_path_buf).unwrap_or_default() };
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

pub fn pick_folder(title: &str) -> Option<PathBuf> {
    rfd::FileDialog::new().set_title(title).pick_folder()
}

pub fn texture_from_rgba(ctx: &egui::Context, name: &str, img: &image::RgbaImage) -> egui::TextureHandle {
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    ctx.load_texture(name, color, egui::TextureOptions::LINEAR)
}

/// "1:23.5" / "83.5" / "0:01:23" → seconds.
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total = 0.0;
    for part in s.split(':') {
        total = total * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some(total)
}

pub const ERR: Color32 = Color32::from_rgb(248, 113, 113);
pub const OK: Color32 = Color32::from_rgb(74, 222, 128);
pub const WARN: Color32 = Color32::from_rgb(251, 191, 36);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn times() {
        assert_eq!(parse_time("1:30"), Some(90.0));
        assert_eq!(parse_time("0:01:02.5"), Some(62.5));
        assert_eq!(parse_time("12"), Some(12.0));
        assert_eq!(parse_time("x"), None);
        assert_eq!(parse_time(""), None);
    }
}
