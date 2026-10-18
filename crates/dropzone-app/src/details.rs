//! Right-hand details pane: a preview and the facts about the selected item.

use std::io::Read;
use std::path::Path;

use dropzone_core::archive::{self, ArchiveRef, EntryPreview};
use dropzone_core::{media, pdf};
use eframe::egui::{self, RichText};

use crate::app::App;
use crate::common::{self, Kind, Task};
use crate::nav::{Item, Location};

pub enum PreviewData {
    Image(image::RgbaImage, u32, u32),
    Text(String, bool),
    Lines(Vec<String>),
    Hex(String),
}

enum Shown {
    Image(egui::TextureHandle, u32, u32),
    Text(String, bool),
    Lines(Vec<String>),
    Hex(String),
    Error(String),
}

#[derive(Default)]
pub struct Details {
    for_key: Option<String>,
    task: Task<PreviewData>,
    shown: Option<Shown>,
}

const TEXT_LIMIT: u64 = 64 * 1024;

fn preview_file(path: &Path, kind: Kind) -> anyhow::Result<PreviewData> {
    match kind {
        Kind::Image => {
            let img = dropzone_core::image_ops::load_image(path)?;
            let (w, h) = (img.width(), img.height());
            Ok(PreviewData::Image(img.thumbnail(720, 720).to_rgba8(), w, h))
        }
        Kind::Pdf => {
            let i = pdf::info(path)?;
            Ok(PreviewData::Lines(vec![format!("{} pages", i.pages), format!("{} embedded images", i.image_count), format!("PDF {}", i.version)]))
        }
        Kind::Video | Kind::Audio => {
            let i = media::probe(path)?;
            let mut lines = Vec::new();
            if let Some(d) = i.duration_seconds {
                let t = d.round() as u64;
                lines.push(format!("Duration {}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60));
            }
            lines.extend(i.streams);
            Ok(PreviewData::Lines(lines))
        }
        Kind::Archive => {
            let l = archive::list(&ArchiveRef::file(path), None)?;
            let mut lines = vec![format!("{} archive", l.kind.label()), format!("{} files, {} unpacked", l.file_count, common::human_size(l.total_size))];
            if l.encrypted {
                lines.push("Password-protected".into());
            }
            if l.solid {
                lines.push("Solid".into());
            }
            for e in l.entries.iter().filter(|e| !e.is_dir).take(12) {
                lines.push(format!("  {}", e.path));
            }
            if l.file_count > 12 {
                lines.push(format!("  … and {} more", l.file_count - 12));
            }
            Ok(PreviewData::Lines(lines))
        }
        _ => {
            let mut buf = Vec::new();
            std::fs::File::open(path)?.take(TEXT_LIMIT + 1).read_to_end(&mut buf)?;
            let truncated = buf.len() as u64 > TEXT_LIMIT;
            buf.truncate(TEXT_LIMIT as usize);
            match std::str::from_utf8(&buf) {
                Ok(t) if !t.contains('\0') => Ok(PreviewData::Text(t.to_string(), truncated)),
                Err(e) if truncated && e.valid_up_to() + 4 > buf.len() => {
                    Ok(PreviewData::Text(String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned(), true))
                }
                _ => {
                    let head: Vec<String> = buf.iter().take(256).map(|b| format!("{b:02x}")).collect();
                    Ok(PreviewData::Hex(head.chunks(16).map(|c| c.join(" ")).collect::<Vec<_>>().join("\n")))
                }
            }
        }
    }
}

impl App {
    pub fn details_panel(&mut self, ui: &mut egui::Ui) {
        // Keep the pane at its set width whatever the content.
        ui.set_min_width(ui.available_width());
        let sel: Vec<Item> = self.selected_items().into_iter().cloned().collect();
        let item = match sel.len() {
            0 => None,
            1 => sel.first().cloned(),
            _ => {
                self.multi_summary(ui, &sel);
                return;
            }
        };
        let Some(item) = item else {
            self.folder_summary(ui);
            return;
        };
        self.load_details(ui.ctx(), &item);

        ui.label(RichText::new(item.kind.icon(item.is_archive)).size(28.0).color(item.kind.color()));
        ui.add(egui::Label::new(RichText::new(&item.name).strong().size(15.0)).wrap());
        ui.label(RichText::new(item.type_label()).small().weak());
        ui.add_space(8.0);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            match &self.details.shown {
                _ if self.details.task.running() => {
                    ui.spinner();
                }
                Some(Shown::Image(tex, w, h)) => {
                    let size = tex.size_vec2();
                    let scale = (ui.available_width() / size.x).min(1.0);
                    ui.image((tex.id(), size * scale));
                    ui.label(RichText::new(format!("{w} × {h} pixels")).small().weak());
                }
                Some(Shown::Text(t, truncated)) => {
                    egui::Frame::NONE.fill(ui.visuals().extreme_bg_color).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(t.lines().take(80).collect::<Vec<_>>().join("\n")).monospace().size(11.5)).selectable(true));
                    });
                    if *truncated || t.lines().count() > 80 {
                        ui.label(RichText::new("(beginning shown)").small().weak());
                    }
                }
                Some(Shown::Lines(lines)) => {
                    for l in lines {
                        ui.label(RichText::new(l).small());
                    }
                }
                Some(Shown::Hex(h)) => {
                    ui.label(RichText::new("Binary file").small().weak());
                    ui.label(RichText::new(h).monospace().size(10.5));
                }
                Some(Shown::Error(e)) => {
                    ui.label(RichText::new(e).small().color(common::ERR));
                }
                None => {}
            }
            ui.add_space(10.0);
            egui::Grid::new("facts").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                if let Some(s) = item.size.filter(|_| !item.is_dir) {
                    fact(ui, "Size", &format!("{} ({} bytes)", common::human_size(s), group_digits(s)));
                }
                if let Some(p) = item.packed {
                    fact(ui, "Packed", &common::human_size(p));
                }
                if let Some(m) = item.modified {
                    fact(ui, "Modified", &m.format("%Y-%m-%d %H:%M").to_string());
                }
                if self.loc.folder().is_some() {
                    if let Ok(md) = std::fs::metadata(&item.path)
                        && let Ok(c) = md.created()
                    {
                        fact(ui, "Created", &chrono::DateTime::<chrono::Local>::from(c).format("%Y-%m-%d %H:%M").to_string());
                    }
                    fact(ui, "Location", &item.path.parent().map(|p| p.display().to_string()).unwrap_or_default());
                }
            });
        });
    }

    fn folder_summary(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("🗀").size(28.0));
        ui.add(egui::Label::new(RichText::new(self.loc.title()).strong().size(15.0)).wrap());
        let files = self.visible.iter().filter(|&&i| !self.items[i].is_dir).count();
        let folders = self.visible.len() - files;
        let size: u64 = self.visible.iter().filter_map(|&i| self.items[i].size.filter(|_| !self.items[i].is_dir)).sum();
        ui.label(RichText::new(format!("{folders} folders, {files} files, {}", common::human_size(size))).small().weak());
        if let (Location::Archive { .. }, Some((_, l))) = (&self.loc, &self.archive_listing) {
            ui.add_space(8.0);
            ui.label(RichText::new(format!("{} archive", l.kind.label())).small());
            ui.label(RichText::new(format!("{} files, {} unpacked", l.file_count, common::human_size(l.total_size))).small());
            if let Some(c) = l.total_compressed.filter(|c| *c > 0 && l.total_size > 0) {
                ui.label(RichText::new(format!("{} packed ({:.0}%)", common::human_size(c), c as f64 / l.total_size as f64 * 100.0)).small());
            }
        }
        ui.add_space(14.0);
        ui.label(RichText::new("Select a file to preview it. Right-click for conversion, archive and rename tools.").small().weak());
    }

    fn multi_summary(&mut self, ui: &mut egui::Ui, sel: &[Item]) {
        ui.label(RichText::new("🗐").size(28.0));
        ui.label(RichText::new(format!("{} items selected", sel.len())).strong().size(15.0));
        let size: u64 = sel.iter().filter(|i| !i.is_dir).filter_map(|i| i.size).sum();
        ui.label(RichText::new(common::human_size(size)).small().weak());
        ui.add_space(10.0);
        let mut counts: Vec<(Kind, usize)> = Vec::new();
        for i in sel {
            match counts.iter_mut().find(|c| c.0 == i.kind) {
                Some(c) => c.1 += 1,
                None => counts.push((i.kind, 1)),
            }
        }
        for (k, n) in counts {
            let label = k.label();
            let plural = if n == 1 || matches!(k, Kind::Audio | Kind::ChecksumList) { label.to_string() } else { format!("{label}s") };
            ui.label(RichText::new(format!("{} {n} {plural}", k.icon(false))).small());
        }
    }

    fn load_details(&mut self, ctx: &egui::Context, item: &Item) {
        let id = format!("{}|{}", self.loc_text(), item.key);
        if self.details.for_key.as_deref() == Some(id.as_str()) {
            if let Some(r) = self.details.task.poll() {
                self.details.shown = Some(match r {
                    Ok(PreviewData::Image(img, w, h)) => Shown::Image(common::texture_from_rgba(ctx, "details", &img), w, h),
                    Ok(PreviewData::Text(t, tr)) => Shown::Text(t, tr),
                    Ok(PreviewData::Lines(l)) => Shown::Lines(l),
                    Ok(PreviewData::Hex(h)) => Shown::Hex(h),
                    Err(e) => Shown::Error(e),
                });
            }
            return;
        }
        self.details.for_key = Some(id);
        self.details.shown = None;
        if item.is_dir {
            self.details.task = Task::default();
            return;
        }
        match &self.loc {
            Location::Folder(_) => {
                let (p, k) = (item.path.clone(), item.kind);
                self.details.task = Task::spawn(ctx, move || preview_file(&p, k));
            }
            Location::Archive { archive, .. } => {
                let (a, key) = (archive.clone(), item.key.clone());
                let pw = Some(self.archive_password.clone()).filter(|p| !p.is_empty());
                self.details.task = Task::spawn(ctx, move || {
                    Ok(match archive::preview_entry(&a, &key, pw.as_deref())? {
                        EntryPreview::Text { text, truncated } => PreviewData::Text(text, truncated),
                        EntryPreview::Image { image, width, height } => PreviewData::Image(image, width, height),
                        EntryPreview::Archive => PreviewData::Lines(vec!["Archive inside an archive. Double-click to open it.".into()]),
                        EntryPreview::Binary { hex, .. } => PreviewData::Hex(hex.lines().take(16).collect::<Vec<_>>().join("\n")),
                    })
                });
            }
        }
    }
}

fn fact(ui: &mut egui::Ui, k: &str, v: &str) {
    ui.label(RichText::new(k).small().weak());
    ui.add(egui::Label::new(RichText::new(v).small()).wrap());
    ui.end_row();
}

fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
