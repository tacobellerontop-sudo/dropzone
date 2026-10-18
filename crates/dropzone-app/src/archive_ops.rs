//! Actions when browsing inside an archive: password, extract, test, compare, open an entry.

use std::path::PathBuf;

use dropzone_core::archive::{self, ArchiveRef, CompareRow, CompareStatus, ExtractOptions};
use dropzone_core::jobs::JobStatus;
use dropzone_core::ops::Operation;
use eframe::egui::{self, Color32, RichText};

use crate::app::App;
use crate::common::{self, Task};
use crate::nav::Location;

pub enum Tracked {
    Extract(u64),
    Verify(u64),
}

#[derive(Clone)]
pub struct Banner {
    pub text: String,
    pub color: Color32,
    pub folder: Option<PathBuf>,
}

pub struct CompareState {
    pub folder: PathBuf,
    pub task: Task<Vec<CompareRow>>,
    pub rows: Option<Vec<CompareRow>>,
    pub filter: Option<CompareStatus>,
}

impl App {
    fn current_archive(&self) -> Option<ArchiveRef> {
        match &self.loc {
            Location::Archive { archive, .. } => Some(archive.clone()),
            Location::Folder(_) => None,
        }
    }

    fn password(&self) -> Option<String> {
        Some(self.archive_password.clone()).filter(|p| !p.is_empty())
    }

    pub fn archive_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(archive) = self.current_archive() else { return };
        ui.add_space(4.0);
        if self.needs_password {
            egui::Frame::NONE.fill(common::WARN.gamma_multiply(0.15)).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}  Password-protected", crate::icons::LOCK_SIMPLE)).color(common::WARN));
                    let r = ui.add(egui::TextEdit::singleline(&mut self.archive_password).password(true).hint_text("Password").desired_width(180.0));
                    if ui.button("Unlock").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                        self.archive_listing = None;
                        let pw = self.archive_password.clone();
                        self.reload(&ctx);
                        self.archive_password = pw;
                    }
                });
            });
        }
        if let Some(b) = self.banner.clone() {
            egui::Frame::NONE.fill(b.color.gamma_multiply(0.15)).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(&b.text).color(b.color)).wrap());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(crate::icons::X).clicked() {
                            self.banner = None;
                        }
                        if let Some(f) = &b.folder
                            && ui.small_button("Open folder").clicked()
                        {
                            common::open_path(f);
                        }
                    });
                });
            });
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            let n = self.selection.len();
            if ui.add_enabled(n > 0, egui::Button::new(format!("{}  Extract {n} selected", crate::icons::TRAY_ARROW_DOWN))).clicked() {
                self.extract_from_archive(Some(self.selection.iter().cloned().collect()), None);
            }
            if ui.button(format!("{}  Extract all", crate::icons::TRAY_ARROW_DOWN)).clicked() {
                self.extract_from_archive(None, None);
            }
            if ui.button(format!("{}  Extract here", crate::icons::FOLDER_OPEN)).on_hover_text("Into a new folder next to the archive").clicked() {
                let dest = archive.path.parent().map(|p| p.to_path_buf());
                self.extract_from_archive(None, dest);
            }
            if ui
                .button(format!("{}  Test", crate::icons::SHIELD_CHECK))
                .on_hover_text("Decompress everything and check every CRC, including nested archives")
                .clicked()
            {
                let ids = self.enqueue(vec![archive.path.clone()], Operation::VerifyArchive { password: self.password(), recursive: true }, None);
                self.track_verify(ids);
            }
            if ui
                .button(format!("{}  Compare…", crate::icons::GIT_DIFF))
                .on_hover_text("Find files that differ from, or are missing in, a folder on disk")
                .clicked()
                && let Some(folder) = common::pick_folder("Compare with folder")
            {
                let (r, pw, f) = (archive.clone(), self.password(), folder.clone());
                let task = Task::spawn(&ctx, move || archive::compare(&r, pw.as_deref(), &f, &dropzone_core::util::NoProgress, &dropzone_core::Cancel::new()));
                self.compare = Some(CompareState { folder, task, rows: None, filter: None });
            }
        });
        ui.add_space(4.0);
    }

    /// Queues extraction of `entries` (or everything) from the current archive.
    pub fn extract_from_archive(&mut self, entries: Option<Vec<String>>, dest: Option<PathBuf>) {
        let Some(archive) = self.current_archive() else { return };
        let dest = match dest {
            Some(d) => d,
            None => {
                let mut dialog = rfd::FileDialog::new().set_title("Extract to");
                if let Some(d) = archive.path.parent() {
                    dialog = dialog.set_directory(d);
                }
                match dialog.pick_folder() {
                    Some(d) => d,
                    None => return,
                }
            }
        };
        let options = ExtractOptions { password: self.password(), entries, into_subfolder: true, flatten: false, overwrite: false };
        let ids = self.enqueue(vec![archive.path.clone()], Operation::ExtractArchive { options, inner: archive.inner.clone() }, Some(dest));
        self.tracked.extend(ids.into_iter().map(Tracked::Extract));
        self.banner = Some(Banner { text: "Extracting…".into(), color: Color32::GRAY, folder: None });
    }

    pub fn track_verify(&mut self, ids: Vec<u64>) {
        self.tracked.extend(ids.into_iter().map(Tracked::Verify));
        self.banner = Some(Banner { text: "Testing…".into(), color: Color32::GRAY, folder: None });
        self.toast("Testing archive integrity");
    }

    pub fn poll_tracked(&mut self) {
        let mut messages = Vec::new();
        self.tracked.retain(|t| {
            let id = match t {
                Tracked::Extract(id) | Tracked::Verify(id) => *id,
            };
            let Some(job) = self.jobs.iter().find(|j| j.id == id) else { return true };
            if !job.status.is_finished() {
                return true;
            }
            let summary = job.summary.clone().unwrap_or_default();
            let b = match (t, job.status) {
                (Tracked::Extract(_), JobStatus::Done) => {
                    Banner { text: format!("Extracted {summary}"), color: common::OK, folder: job.outputs.first().cloned() }
                }
                (Tracked::Verify(_), JobStatus::Done) => Banner { text: format!("{}: healthy. {summary}", job.label), color: common::OK, folder: None },
                (_, JobStatus::Cancelled) => Banner { text: "Cancelled".into(), color: common::WARN, folder: None },
                (Tracked::Verify(_), _) => {
                    Banner { text: format!("{} is damaged: {}", job.label, job.error.clone().unwrap_or_default()), color: common::ERR, folder: None }
                }
                (_, _) => Banner { text: job.error.clone().unwrap_or_else(|| "Failed".into()), color: common::ERR, folder: None },
            };
            messages.push(b);
            false
        });
        for b in messages {
            // Outside an archive the banner isn't visible, so say it in a toast too.
            if self.loc.folder().is_some() {
                if b.color == common::ERR {
                    self.error(b.text.clone());
                } else {
                    self.toast(b.text.clone());
                }
            }
            self.banner = Some(b);
        }
    }

    /// Extracts one entry to a temp folder and opens it with its default app, like 7-Zip.
    pub fn open_archive_entry(&mut self, ctx: &egui::Context, archive: ArchiveRef, key: String) {
        let pw = self.password();
        self.toast(format!("Opening {}", key.rsplit('/').next().unwrap_or(&key)));
        self.side_task = Task::spawn(ctx, move || {
            let dir = std::env::temp_dir().join("dropzone-open").join(format!("{}", std::process::id()));
            std::fs::create_dir_all(&dir)?;
            let opts = ExtractOptions { password: pw, entries: Some(vec![key]), into_subfolder: false, flatten: true, overwrite: true };
            let rep = archive::extract(&archive, Some(&dir), &opts, &dropzone_core::util::NoProgress, &dropzone_core::Cancel::new())?;
            let f = rep.files.first().ok_or_else(|| anyhow::anyhow!("nothing extracted"))?;
            common::open_path(f);
            Ok(String::new())
        });
    }

    pub fn poll_side_task(&mut self) {
        if let Some(r) = self.side_task.poll() {
            match r {
                Ok(msg) if !msg.is_empty() => self.toast(msg),
                Ok(_) => {}
                Err(e) => self.error(e),
            }
            // Side tasks (delete, open) usually change what's on disk.
            if self.loc.folder().is_some() {
                self.reload_pending = true;
            }
        }
    }

    pub fn compare_window(&mut self, ctx: &egui::Context) {
        let Some(c) = &mut self.compare else { return };
        if let Some(r) = c.task.poll() {
            match r {
                Ok(rows) => c.rows = Some(rows),
                Err(e) => {
                    self.compare = None;
                    self.error(format!("Compare failed: {e}"));
                    return;
                }
            }
        }
        let mut open = true;
        egui::Window::new(format!("Compare with {}", common::file_name(&c.folder))).open(&mut open).default_size([760.0, 480.0]).collapsible(false).show(
            ctx,
            |ui| {
                let Some(rows) = &c.rows else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Comparing… files with matching sizes are checksummed");
                    });
                    return;
                };
                let count = |s: CompareStatus| rows.iter().filter(|r| r.status == s).count();
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut c.filter, None, format!("All ({})", rows.len()));
                    for (s, label) in [
                        (CompareStatus::Different, "Different"),
                        (CompareStatus::OnlyInArchive, "Only in archive"),
                        (CompareStatus::OnlyInFolder, "Only in folder"),
                        (CompareStatus::Same, "Identical"),
                    ] {
                        ui.selectable_value(&mut c.filter, Some(s), format!("{label} ({})", count(s)));
                    }
                });
                ui.separator();
                let shown: Vec<&CompareRow> = rows.iter().filter(|r| c.filter.is_none_or(|f| f == r.status)).collect();
                let w = ui.available_width();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, 22.0, shown.len(), |ui, range| {
                    for r in &shown[range] {
                        ui.horizontal(|ui| {
                            let (label, color) = match r.status {
                                CompareStatus::Same => ("identical", common::OK),
                                CompareStatus::Different => ("different", common::ERR),
                                CompareStatus::OnlyInArchive => ("only in archive", common::WARN),
                                CompareStatus::OnlyInFolder => ("only in folder", common::WARN),
                            };
                            ui.add_sized([110.0, 20.0], egui::Label::new(RichText::new(label).small().color(color)));
                            common::cell(ui, [(w - 330.0).max(100.0), 20.0], egui::Label::new(&r.path).truncate());
                            ui.add_sized(
                                [90.0, 20.0],
                                egui::Label::new(RichText::new(r.archive_size.map(common::human_size).unwrap_or_default()).small().weak()),
                            );
                            ui.add_sized(
                                [90.0, 20.0],
                                egui::Label::new(RichText::new(r.folder_size.map(common::human_size).unwrap_or_default()).small().weak()),
                            );
                        });
                    }
                });
            },
        );
        if !open {
            self.compare = None;
        }
    }
}
