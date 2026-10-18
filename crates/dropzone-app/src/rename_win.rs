//! Batch rename window: a pattern, a live old → new table, and an undoable apply.

use std::path::PathBuf;

use dropzone_core::rename::{self, Applied, CaseChange, PlanStatus, RenameItem, RenameRule};
use eframe::egui::{self, RichText};

use crate::common;
use crate::tools::labeled;

pub struct RenameTab {
    pub rule: RenameRule,
    files: Vec<PathBuf>,
    plan: Result<Vec<RenameItem>, String>,
    planned_for: Option<(RenameRule, usize)>,
    last_applied: Option<Vec<Applied>>,
    message: Option<(String, bool)>,
}

impl RenameTab {
    pub fn new(rule: RenameRule, files: Vec<PathBuf>) -> Self {
        Self { rule, files, plan: Ok(Vec::new()), planned_for: None, last_applied: None, message: None }
    }

    fn replan(&mut self) {
        let key = (self.rule.clone(), self.files.len());
        if self.planned_for.as_ref().is_some_and(|k| rule_eq(&k.0, &key.0) && k.1 == key.1) {
            return;
        }
        self.plan = rename::plan(&self.files, &self.rule).map_err(|e| format!("{e:#}"));
        self.planned_for = Some(key);
    }

    /// Shows the rename window. Returns false once it's closed; `renamed` is set after a rename or undo.
    pub fn window(&mut self, ctx: &egui::Context, renamed: &mut bool) -> bool {
        let mut open = true;
        let before = self.last_applied.as_ref().map(|a| a.len());
        let before_msg = self.message.clone();
        egui::Window::new(format!("Batch rename {} item{}", self.files.len(), if self.files.len() == 1 { "" } else { "s" }))
            .open(&mut open)
            .default_size([900.0, 520.0])
            .min_size([640.0, 360.0])
            .collapsible(false)
            .show(ctx, |ui| {
                self.replan();
                egui::Panel::bottom("rename-actions")
                    .resizable(false)
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin { top: 8, ..Default::default() }))
                    .show(ui, |ui| self.actions(ui));
                egui::Panel::left("rename-rule")
                    .resizable(false)
                    .exact_size(300.0)
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin { right: 12, ..Default::default() }))
                    .show(ui, |ui| self.rule_panel(ui));
                self.table(ui);
            });
        if self.last_applied.as_ref().map(|a| a.len()) != before || self.message != before_msg {
            *renamed = true;
        }
        open
    }

    fn rule_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let r = &mut self.rule;
            labeled(ui, "New name", |ui| ui.add(egui::TextEdit::singleline(&mut r.template).font(egui::TextStyle::Monospace)));
            ui.horizontal_wrapped(|ui| {
                for t in ["{name}", "{n:3}", "{date}", "{time}", "{parent}", "{ext}"] {
                    if ui.small_button(RichText::new(t).monospace()).on_hover_text(token_help(t)).clicked() {
                        r.template.push_str(t);
                    }
                }
            });
            ui.add_space(6.0);
            labeled(ui, "Find", |ui| ui.add(egui::TextEdit::singleline(&mut r.find).font(egui::TextStyle::Monospace)));
            labeled(ui, "Replace with", |ui| ui.add(egui::TextEdit::singleline(&mut r.replace).font(egui::TextStyle::Monospace)));
            ui.horizontal(|ui| {
                ui.checkbox(&mut r.use_regex, "Regular expression");
                ui.checkbox(&mut r.case_sensitive, "Match case");
            });
            ui.add_space(6.0);
            labeled(ui, "Name case", |ui| case_combo(ui, "name-case", &mut r.case, true));
            labeled(ui, "Extension case", |ui| case_combo(ui, "ext-case", &mut r.extension_case, false));
            let mut ext = r.new_extension.clone().unwrap_or_default();
            labeled(ui, "New extension", |ui| ui.add(egui::TextEdit::singleline(&mut ext).hint_text("keep")));
            r.new_extension = Some(ext).filter(|e| !e.trim().is_empty());
            ui.horizontal(|ui| {
                ui.label("Counter starts at");
                ui.add(egui::DragValue::new(&mut r.start).range(0..=1_000_000));
                ui.label("step");
                ui.add(egui::DragValue::new(&mut r.step).range(1..=1000));
            });
        });
    }

    fn actions(&mut self, ui: &mut egui::Ui) {
        let (ready, problems) = match &self.plan {
            Ok(p) => (
                p.iter().filter(|i| i.status == PlanStatus::Ok).count(),
                p.iter().filter(|i| matches!(i.status, PlanStatus::Conflict | PlanStatus::Invalid)).count(),
            ),
            Err(_) => (0, 1),
        };
        ui.horizontal(|ui| {
            let label = format!("Rename {ready} file{}", if ready == 1 { "" } else { "s" });
            let btn = egui::Button::new(RichText::new(label).strong()).fill(ui.visuals().selection.bg_fill);
            if ui.add_enabled(ready > 0 && problems == 0, btn).clicked() {
                self.apply();
            }
            if ui.add_enabled(self.last_applied.is_some(), egui::Button::new("Undo")).clicked() {
                self.undo();
            }
        });
        if problems > 0 {
            ui.label(RichText::new(format!("{problems} name(s) need fixing first (shown in red).")).small().color(common::ERR));
        }
        if let Err(e) = &self.plan {
            ui.label(RichText::new(e).small().color(common::ERR));
        }
        if let Some((m, err)) = &self.message {
            ui.label(RichText::new(m).small().color(if *err { common::ERR } else { common::OK }));
        }
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        let Ok(plan) = &self.plan else { return };
        let row_h = 24.0;
        let mut remove = None;
        egui::Frame::group(ui.style()).inner_margin(6.0).show(ui, |ui| {
            let w = ui.available_width();
            let col = ((w - 110.0) / 2.0).max(80.0);
            ui.horizontal(|ui| {
                common::cell(ui, [col, 18.0], egui::Label::new(RichText::new("Current name").small().strong()));
                common::cell(ui, [col, 18.0], egui::Label::new(RichText::new("New name").small().strong()));
                ui.add_sized([80.0, 18.0], egui::Label::new(RichText::new("Status").small().strong()));
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, plan.len(), |ui, range| {
                for i in range {
                    let item = &plan[i];
                    ui.horizontal(|ui| {
                        ui.set_height(row_h);
                        let from = common::file_name(&item.from);
                        let to = common::file_name(&item.to);
                        common::cell(ui, [col, row_h], egui::Label::new(&from).truncate()).on_hover_text(item.from.display().to_string());
                        let to_text = match item.status {
                            PlanStatus::Ok => RichText::new(&to).color(ui.visuals().selection.bg_fill),
                            PlanStatus::Unchanged => RichText::new(&to).weak(),
                            _ => RichText::new(&to).color(common::ERR),
                        };
                        common::cell(ui, [col, row_h], egui::Label::new(to_text).truncate());
                        let (label, color) = match item.status {
                            PlanStatus::Ok => ("rename", common::OK),
                            PlanStatus::Unchanged => ("same", ui.visuals().weak_text_color()),
                            PlanStatus::Conflict => ("conflict", common::ERR),
                            PlanStatus::Invalid => ("invalid", common::ERR),
                        };
                        let r = ui.add_sized([60.0, row_h], egui::Label::new(RichText::new(label).small().color(color)));
                        if let Some(m) = &item.message {
                            r.on_hover_text(m);
                        }
                        if ui.small_button("🗙").clicked() {
                            remove = Some(i);
                        }
                    });
                }
            });
        });
        if let Some(i) = remove {
            self.files.remove(i);
            self.planned_for = None;
        }
    }

    fn apply(&mut self) {
        let Ok(plan) = &self.plan else { return };
        match rename::apply(plan) {
            Ok(applied) => {
                // Keep working with the renamed files.
                for a in &applied {
                    if let Some(f) = self.files.iter_mut().find(|f| **f == a.from) {
                        *f = a.to.clone();
                    }
                }
                self.message = Some((format!("Renamed {} file(s).", applied.len()), false));
                self.last_applied = Some(applied);
            }
            Err(e) => self.message = Some((format!("Nothing was renamed: {e:#}"), true)),
        }
        self.planned_for = None;
    }

    fn undo(&mut self) {
        let Some(applied) = self.last_applied.take() else { return };
        match rename::undo(&applied) {
            Ok(done) => {
                for a in &done {
                    if let Some(f) = self.files.iter_mut().find(|f| **f == a.from) {
                        *f = a.to.clone();
                    }
                }
                self.message = Some((format!("Restored {} original name(s).", done.len()), false));
            }
            Err(e) => {
                self.message = Some((format!("Undo failed: {e:#}"), true));
                self.last_applied = Some(applied);
            }
        }
        self.planned_for = None;
    }
}

fn rule_eq(a: &RenameRule, b: &RenameRule) -> bool {
    // RenameRule has no PartialEq; its JSON form is a faithful stand-in.
    serde_json::to_string(a).ok() == serde_json::to_string(b).ok()
}

fn case_combo(ui: &mut egui::Ui, id: &str, value: &mut CaseChange, title: bool) -> egui::Response {
    let label = |c: CaseChange| match c {
        CaseChange::None => "Unchanged",
        CaseChange::Lower => "lowercase",
        CaseChange::Upper => "UPPERCASE",
        CaseChange::Title => "Title Case",
    };
    egui::ComboBox::from_id_salt(id)
        .selected_text(label(*value))
        .show_ui(ui, |ui| {
            for c in [CaseChange::None, CaseChange::Lower, CaseChange::Upper, CaseChange::Title] {
                if c == CaseChange::Title && !title {
                    continue;
                }
                ui.selectable_value(value, c, label(c));
            }
        })
        .response
}

fn token_help(t: &str) -> &'static str {
    match t {
        "{name}" => "Original name without extension",
        "{n:3}" => "Counter, zero-padded to 3 digits ({n} for no padding)",
        "{date}" => "Modified date, YYYY-MM-DD",
        "{time}" => "Modified time, HHMMSS",
        "{parent}" => "Name of the containing folder",
        "{ext}" => "Original extension",
        _ => "",
    }
}
