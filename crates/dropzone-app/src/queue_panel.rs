//! Bottom drawer listing queued jobs: progress, results, and "quit when done".

use dropzone_core::jobs::{JobInfo, JobQueue, JobStatus};
use eframe::egui::{self, RichText};

use crate::app::App;
use crate::common;

pub fn ui(ui: &mut egui::Ui, app: &mut App) {
    let count = |s: JobStatus| app.jobs.iter().filter(|j| j.status == s).count();
    let (queued, running, done, failed) = (count(JobStatus::Queued), count(JobStatus::Running), count(JobStatus::Done), count(JobStatus::Failed));
    let empty = app.jobs.is_empty();
    let any_finished = app.jobs.iter().any(|j| j.status.is_finished());

    ui.horizontal(|ui| {
        ui.label(RichText::new("Queue").strong());
        let summary = if empty {
            "Nothing queued. Right-click files or use Tools to start something.".to_string()
        } else {
            format!("{running} running · {queued} waiting · {done} done · {failed} failed")
        };
        ui.label(RichText::new(summary).small().weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(egui::Button::new("🗙").frame(false)).on_hover_text("Hide").clicked() {
                app.queue_open = false;
            }
            if ui.add_enabled(any_finished, egui::Button::new("Clear finished").small()).clicked() {
                app.queue.clear_finished();
                app.jobs.retain(|j| !j.status.is_finished());
            }
            if ui.add_enabled(queued + running > 0, egui::Button::new("Cancel all").small()).clicked() {
                app.queue.cancel_all();
            }
            ui.checkbox(&mut app.quit_when_done, RichText::new("Quit when finished").small())
                .on_hover_text("Queue up work, tick this, and walk away. Dropzone closes itself once everything is done.");
        });
    });
    ui.separator();

    let mut order: Vec<&JobInfo> = app.jobs.iter().collect();
    order.sort_by_key(|j| (j.status != JobStatus::Running, j.status != JobStatus::Queued, std::cmp::Reverse(j.id)));
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for job in order {
            job_card(ui, &app.queue, job);
        }
    });
}

fn job_card(ui: &mut egui::Ui, queue: &JobQueue, job: &JobInfo) {
    egui::Frame::NONE.fill(ui.visuals().faint_bg_color).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (label, color) = match job.status {
                JobStatus::Queued => ("waiting", ui.visuals().weak_text_color()),
                JobStatus::Running => ("running", ui.visuals().selection.bg_fill),
                JobStatus::Done => ("done", common::OK),
                JobStatus::Failed => ("failed", common::ERR),
                JobStatus::Cancelled => ("cancelled", ui.visuals().weak_text_color()),
            };
            ui.add_sized([64.0, 18.0], egui::Label::new(RichText::new(label).small().strong().color(color)));
            ui.add(egui::Label::new(RichText::new(&job.label).strong()).truncate());
            ui.label(RichText::new(&job.operation).small().weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !job.status.is_finished() && ui.small_button("Cancel").clicked() {
                    queue.cancel(job.id);
                }
                if let Some(out) = job.outputs.first() {
                    if ui.small_button("Show").on_hover_text(out.display().to_string()).clicked() {
                        common::reveal(out);
                    }
                    if ui.small_button("Open").clicked() {
                        common::open_path(out);
                    }
                }
                if job.details.is_some() && ui.small_button("Copy result").clicked() {
                    ui.ctx().copy_text(details_text(job));
                }
                if let Some(s) = &job.summary {
                    ui.label(RichText::new(s).small());
                }
            });
        });
        if job.status == JobStatus::Running {
            let mut bar = egui::ProgressBar::new(job.progress).desired_height(5.0);
            if job.progress <= 0.0 {
                bar = bar.animate(true);
            }
            ui.add(bar);
        }
        if let Some(e) = &job.error {
            ui.add(egui::Label::new(RichText::new(e).small().color(common::ERR)).wrap().selectable(true));
        }
        if job.status == JobStatus::Done
            && let Some(text) = hashes_text(job)
        {
            ui.add(egui::Label::new(RichText::new(text).monospace().size(11.5)).selectable(true));
        }
    });
    ui.add_space(3.0);
}

fn hashes_text(job: &JobInfo) -> Option<String> {
    let d = job.details.as_ref()?.as_object()?;
    if !d.contains_key("sha256") && !d.contains_key("md5") && !d.contains_key("crc32") {
        return None;
    }
    let mut lines = Vec::new();
    for (k, name) in [("crc32", "CRC32 "), ("md5", "MD5   "), ("sha1", "SHA1  "), ("sha256", "SHA256")] {
        if let Some(v) = d.get(k).and_then(|v| v.as_str()) {
            lines.push(format!("{name}  {v}"));
        }
    }
    Some(lines.join("\n"))
}

fn details_text(job: &JobInfo) -> String {
    hashes_text(job).unwrap_or_else(|| job.details.as_ref().map(|d| serde_json::to_string_pretty(d).unwrap_or_default()).unwrap_or_default())
}
