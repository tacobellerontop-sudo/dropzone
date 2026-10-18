//! Right-hand details pane: a preview and the facts about the selected item.

use std::io::Read;
use std::path::Path;

use dropzone_core::archive::{self, ArchiveRef, EntryPreview};
use dropzone_core::{media, pdf};
use eframe::egui::{self, RichText};

use crate::app::App;
use crate::common::{self, Kind, Task};
use crate::nav::{Item, Location};
use crate::theme;

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

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            // Preview box: the image itself, or a big file icon.
            let w = ui.available_width();
            let image = match &self.details.shown {
                Some(Shown::Image(tex, _, _)) => Some(tex.clone()),
                _ => None,
            };
            let box_h = match &image {
                Some(tex) => (tex.size_vec2().y * (w / tex.size_vec2().x).min(1.0)).clamp(120.0, 320.0),
                None => 150.0,
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(w, box_h), egui::Sense::hover());
            ui.painter().rect_filled(rect, 10.0, theme::FIELD);
            match &image {
                Some(tex) => {
                    let size = tex.size_vec2();
                    let scale = ((rect.width() - 16.0) / size.x).min((rect.height() - 16.0) / size.y).min(1.0);
                    let r = egui::Rect::from_center_size(rect.center(), size * scale);
                    egui::Image::from_texture(tex).corner_radius(6).paint_at(ui, r);
                }
                None if self.details.task.running() => {
                    ui.put(egui::Rect::from_center_size(rect.center(), egui::vec2(24.0, 24.0)), egui::Spinner::new().color(theme::TEXT_3));
                }
                None => theme::paint_file_icon(ui.painter(), rect.center(), 64.0, item.kind, item.is_archive),
            }
            ui.add_space(12.0);
            ui.add(egui::Label::new(RichText::new(&item.name).font(theme::strong(15.0)).color(theme::TEXT)).wrap());
            ui.label(RichText::new(item.type_label()).color(theme::TEXT_3));
            ui.add_space(10.0);

            match &self.details.shown {
                Some(Shown::Text(t, truncated)) => {
                    egui::Frame::NONE.fill(theme::FIELD).corner_radius(8).inner_margin(10).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(
                            egui::Label::new(RichText::new(t.lines().take(80).collect::<Vec<_>>().join("\n")).monospace().size(11.5).color(theme::TEXT_2))
                                .selectable(true),
                        );
                    });
                    if *truncated || t.lines().count() > 80 {
                        ui.label(RichText::new("Beginning shown").small().color(theme::TEXT_3));
                    }
                    ui.add_space(6.0);
                }
                Some(Shown::Hex(h)) => {
                    egui::Frame::NONE.fill(theme::FIELD).corner_radius(8).inner_margin(10).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(h).monospace().size(10.5).color(theme::TEXT_2));
                    });
                    ui.add_space(6.0);
                }
                Some(Shown::Error(e)) => {
                    ui.label(RichText::new(e).small().color(common::ERR));
                }
                _ => {}
            }

            divider(ui);
            egui::Grid::new("facts").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                if let Some(Shown::Image(_, w, h)) = &self.details.shown {
                    fact(ui, "Dimensions", &format!("{w} × {h}"));
                }
                if let Some(s) = item.size.filter(|_| !item.is_dir) {
                    fact(ui, "Size", &format!("{} ({} bytes)", common::human_size(s), group_digits(s)));
                }
                if let Some(p) = item.packed {
                    fact(ui, "Packed", &common::human_size(p));
                }
                if let Some(m) = item.modified {
                    fact(ui, "Modified", &m.format("%b %-d, %Y at %H:%M").to_string());
                }
                if self.loc.folder().is_some() {
                    if let Ok(md) = std::fs::metadata(&item.path)
                        && let Ok(c) = md.created()
                    {
                        fact(ui, "Created", &chrono::DateTime::<chrono::Local>::from(c).format("%b %-d, %Y at %H:%M").to_string());
                    }
                    fact(ui, "Where", &item.path.parent().map(|p| p.display().to_string()).unwrap_or_default());
                }
            });
            if let Some(Shown::Lines(lines)) = &self.details.shown {
                divider(ui);
                for l in lines {
                    let indented = l.starts_with("  ");
                    let color = if indented { theme::TEXT_3 } else { theme::TEXT_2 };
                    ui.add(egui::Label::new(RichText::new(l.trim_start()).color(color)).truncate());
                }
            }
        });
    }

    fn folder_summary(&mut self, ui: &mut egui::Ui) {
        let w = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 150.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 10.0, theme::FIELD);
        let in_archive = matches!(self.loc, Location::Archive { .. });
        theme::paint_file_icon(ui.painter(), rect.center(), 64.0, if in_archive { Kind::Archive } else { Kind::Folder }, in_archive);
        ui.add_space(12.0);
        ui.add(egui::Label::new(RichText::new(self.loc.title()).font(theme::strong(15.0)).color(theme::TEXT)).wrap());
        let files = self.visible.iter().filter(|&&i| !self.items[i].is_dir).count();
        let folders = self.visible.len() - files;
        let size: u64 = self.visible.iter().filter_map(|&i| self.items[i].size.filter(|_| !self.items[i].is_dir)).sum();
        ui.label(RichText::new(format!("{folders} folders, {files} files")).color(theme::TEXT_3));
        ui.add_space(10.0);
        divider(ui);
        egui::Grid::new("folder-facts").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
            fact(ui, "Files", &common::human_size(size));
            if let (Location::Archive { .. }, Some((_, l))) = (&self.loc, &self.archive_listing) {
                fact(ui, "Format", l.kind.label());
                fact(ui, "Unpacked", &format!("{} in {} files", common::human_size(l.total_size), l.file_count));
                if let Some(c) = l.total_compressed.filter(|c| *c > 0 && l.total_size > 0) {
                    fact(ui, "Packed", &format!("{} ({:.0}%)", common::human_size(c), c as f64 / l.total_size as f64 * 100.0));
                }
            }
        });
        ui.add_space(16.0);
        ui.add(
            egui::Label::new(RichText::new("Select a file to preview it. Right-click for conversion, archive and rename tools.").color(theme::TEXT_3)).wrap(),
        );
    }

    fn multi_summary(&mut self, ui: &mut egui::Ui, sel: &[Item]) {
        let w = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 150.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 10.0, theme::FIELD);
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, crate::icons::FILES, egui::FontId::proportional(60.0), theme::TEXT_2);
        let badge = egui::Rect::from_center_size(rect.center() + egui::vec2(30.0, -26.0), egui::vec2(30.0, 22.0));
        ui.painter().rect_filled(badge, 11.0, theme::ACCENT);
        ui.painter().text(badge.center(), egui::Align2::CENTER_CENTER, sel.len().to_string(), theme::strong(12.0), egui::Color32::WHITE);
        ui.add_space(12.0);
        ui.label(RichText::new(format!("{} items selected", sel.len())).font(theme::strong(15.0)).color(theme::TEXT));
        let size: u64 = sel.iter().filter(|i| !i.is_dir).filter_map(|i| i.size).sum();
        ui.label(RichText::new(common::human_size(size)).color(theme::TEXT_3));
        ui.add_space(10.0);
        divider(ui);
        let mut counts: Vec<(Kind, bool, usize)> = Vec::new();
        for i in sel {
            match counts.iter_mut().find(|c| c.0 == i.kind) {
                Some(c) => c.2 += 1,
                None => counts.push((i.kind, i.is_archive, 1)),
            }
        }
        for (k, archive, n) in counts {
            let label = k.label();
            let plural = if n == 1 || matches!(k, Kind::Audio | Kind::ChecksumList) { label.to_string() } else { format!("{label}s") };
            let (icon, color) = theme::file_icon(k, archive);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).family(theme::fill_family()).size(15.0).color(color));
                ui.label(RichText::new(format!("{n} {plural}")).color(theme::TEXT_2));
            });
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
    ui.label(RichText::new(k).size(12.5).color(theme::TEXT_3));
    ui.add(egui::Label::new(RichText::new(v).size(12.5).color(theme::TEXT_2)).wrap());
    ui.end_row();
}

fn divider(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, egui::Stroke::new(1.0, theme::BORDER));
    ui.add_space(10.0);
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
