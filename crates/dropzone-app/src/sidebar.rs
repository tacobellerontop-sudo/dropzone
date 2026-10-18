//! Left sidebar: quick access folders, pinned folders and drives.

use std::path::PathBuf;

use eframe::egui::{self, RichText};

use crate::app::App;
use crate::common;
use crate::nav::Location;

fn places() -> Vec<(&'static str, &'static str, Option<PathBuf>)> {
    vec![
        ("🏠", "Home", dirs::home_dir()),
        ("🖥", "Desktop", dirs::desktop_dir()),
        ("📄", "Documents", dirs::document_dir()),
        ("⬇", "Downloads", dirs::download_dir()),
        ("🖼", "Pictures", dirs::picture_dir()),
        ("🎵", "Music", dirs::audio_dir()),
        ("🎞", "Videos", dirs::video_dir()),
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

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("⬇").size(20.0).color(ui.visuals().selection.bg_fill));
                ui.label(RichText::new("Dropzone").size(16.0).strong());
            });
            ui.add_space(10.0);

            let entry = |ui: &mut egui::Ui, icon: &str, label: &str, path: &PathBuf, go: &mut Option<PathBuf>| -> egui::Response {
                let selected = current.as_ref() == Some(path);
                let r = ui.add_sized([ui.available_width(), 26.0], egui::Button::selectable(selected, format!("{icon}  {label}")).right_text(""));
                if r.clicked() {
                    *go = Some(path.clone());
                }
                r.on_hover_text(path.display().to_string())
            };

            section(ui, "Quick access");
            for (icon, label, path) in places() {
                if let Some(p) = path.filter(|p| p.is_dir()) {
                    entry(ui, icon, label, &p, &mut go);
                }
            }

            if !self.settings.pinned.is_empty() {
                section(ui, "Pinned");
                for p in self.settings.pinned.clone() {
                    let r = entry(ui, "📌", &common::file_name(&p), &p, &mut go);
                    r.context_menu(|ui| {
                        if ui.button("Unpin").clicked() {
                            unpin = Some(p.clone());
                            ui.close();
                        }
                    });
                }
            }

            section(ui, "Drives");
            for (label, p) in drives() {
                entry(ui, "💽", &label, &p, &mut go);
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

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title.to_uppercase()).small().strong().color(ui.visuals().weak_text_color()));
    ui.add_space(2.0);
}
