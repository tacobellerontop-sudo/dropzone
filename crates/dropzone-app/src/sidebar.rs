//! Left sidebar: quick access folders, pinned folders and drives.

use std::path::PathBuf;

use eframe::egui::{self, Color32, RichText};

use crate::app::App;
use crate::nav::Location;
use crate::{common, icons, theme};

fn places() -> Vec<(&'static str, &'static str, Option<PathBuf>)> {
    vec![
        (icons::HOUSE, "Home", dirs::home_dir()),
        (icons::DESKTOP, "Desktop", dirs::desktop_dir()),
        (icons::FILE_TEXT, "Documents", dirs::document_dir()),
        (icons::DOWNLOAD_SIMPLE, "Downloads", dirs::download_dir()),
        (icons::IMAGE, "Pictures", dirs::picture_dir()),
        (icons::MUSIC_NOTES, "Music", dirs::audio_dir()),
        (icons::FILM_STRIP, "Videos", dirs::video_dir()),
    ]
}

fn drives() -> Vec<(String, PathBuf)> {
    #[cfg(windows)]
    {
        (b'A'..=b'Z')
            .map(|c| format!("{}:\\", c as char))
            .filter(|d| std::path::Path::new(d).exists())
            .map(|d| (format!("{} drive", &d[..2]), PathBuf::from(d)))
            .collect()
    }
    #[cfg(not(windows))]
    {
        let mut out = vec![("Computer".to_string(), PathBuf::from("/"))];
        let user = std::env::var("USER").unwrap_or_default();
        for base in [format!("/media/{user}"), "/run/media".to_string(), "/mnt".to_string(), "/Volumes".to_string()] {
            if let Ok(rd) = std::fs::read_dir(&base) {
                for e in rd.flatten() {
                    if e.path().is_dir() {
                        out.push((e.file_name().to_string_lossy().into_owned(), e.path()));
                    }
                }
            }
        }
        out
    }
}

impl App {
    pub fn sidebar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let current = self.loc.folder().map(|p| p.to_path_buf());
        let mut go: Option<PathBuf> = None;
        let mut unpin: Option<PathBuf> = None;

        // Brand mark.
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 7.0, theme::ACCENT);
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icons::TRAY_ARROW_DOWN, egui::FontId::proportional(16.0), Color32::WHITE);
            ui.add_space(2.0);
            ui.label(RichText::new("Dropzone").font(theme::strong(15.0)).color(theme::TEXT));
        });
        ui.add_space(4.0);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let mut entry = |ui: &mut egui::Ui, icon: &str, label: &str, path: &PathBuf| -> egui::Response {
                let r = theme::nav_row(ui, icon, label, current.as_ref() == Some(path));
                if r.clicked() {
                    go = Some(path.clone());
                }
                r.on_hover_text(path.display().to_string())
            };

            theme::section(ui, "Places");
            for (icon, label, path) in places() {
                if let Some(p) = path.filter(|p| p.is_dir()) {
                    entry(ui, icon, label, &p);
                }
            }

            if !self.settings.pinned.is_empty() {
                theme::section(ui, "Pinned");
                for p in self.settings.pinned.clone() {
                    let r = entry(ui, icons::PUSH_PIN, &common::file_name(&p), &p);
                    r.context_menu(|ui| {
                        if ui.add(theme::menu_item(icons::X, "Unpin")).clicked() {
                            unpin = Some(p.clone());
                            ui.close();
                        }
                    });
                }
            }

            theme::section(ui, "Drives");
            for (label, p) in drives() {
                entry(ui, icons::HARD_DRIVE, &label, &p);
            }
        });

        if let Some(p) = unpin {
            self.settings.pinned.retain(|x| *x != p);
        }
        if let Some(p) = go {
            self.navigate(&ctx, Location::Folder(p));
        }
    }
}
