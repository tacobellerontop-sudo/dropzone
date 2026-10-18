//! The explorer window: state, layout, keyboard, clipboard and the job queue.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dropzone_core::archive::{ArchiveRef, Listing};
use dropzone_core::jobs::{JobInfo, JobQueue, JobSpec, JobStatus, QueueEvent};
use dropzone_core::ops::Operation;
use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText};
use serde::{Deserialize, Serialize};

use crate::common::{self, Task};
use crate::details::Details;
use crate::nav::{Item, Loaded, Location, SortKey};
use crate::rename_win::RenameTab;
use crate::thumbs::Thumbs;
use crate::tools::{ConvertSettings, ToolPanel};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ViewMode {
    #[default]
    List,
    Grid,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub view: ViewMode,
    pub sort_key: SortKey,
    pub sort_asc: bool,
    pub show_hidden: bool,
    pub details_open: bool,
    pub pinned: Vec<PathBuf>,
    pub last_folder: Option<PathBuf>,
    pub convert: ConvertSettings,
    pub rename: dropzone_core::rename::RenameRule,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            view: ViewMode::List,
            sort_key: SortKey::Name,
            sort_asc: true,
            show_hidden: false,
            details_open: true,
            pinned: Vec::new(),
            last_folder: None,
            convert: ConvertSettings::default(),
            rename: Default::default(),
        }
    }
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

/// Inline rename in progress: which item, the edited name, and whether focus was requested.
pub struct Renaming {
    pub key: String,
    pub text: String,
    pub focused: bool,
}

pub struct App {
    // Where we are and what's there.
    pub loc: Location,
    pub back: Vec<Location>,
    pub fwd: Vec<Location>,
    pub items: Vec<Item>,
    pub visible: Vec<usize>,
    pub visible_dirty: bool,
    pub load: Task<Loaded>,
    pub load_error: Option<String>,
    pub archive_listing: Option<(ArchiveRef, Listing)>,
    pub archive_password: String,
    pub needs_password: bool,

    // View state.
    pub settings: Settings,
    pub filter: String,
    pub path_edit: Option<String>,
    pub focus_search: bool,
    pub selection: BTreeSet<String>,
    pub anchor: Option<String>,
    pub focus: Option<String>,
    pub renaming: Option<Renaming>,
    pub scroll_to_focus: bool,
    /// Scroll offset and viewport height of the file view last frame.
    pub last_scroll: (f32, f32),
    pub thumbs: Thumbs,
    pub details: Details,

    // Clipboard: paths and whether they were cut.
    pub clip: Option<(Vec<PathBuf>, bool)>,

    // Tools.
    pub tool: Option<ToolPanel>,
    pub tool_inputs_cache: Option<(String, Vec<crate::tools::Input>, String)>,
    pub rename_win: Option<RenameTab>,
    pub compare: Option<crate::archive_ops::CompareState>,
    pub side_task: Task<String>,
    pub tracked: Vec<crate::archive_ops::Tracked>,
    pub banner: Option<crate::archive_ops::Banner>,
    pub reload_pending: bool,

    // Queue.
    pub queue: JobQueue,
    events: Arc<Mutex<Vec<QueueEvent>>>,
    pub jobs: Vec<JobInfo>,
    pub queue_open: bool,
    pub quit_when_done: bool,
    allow_close: bool,
    close_prompt: bool,
    pub toasts: Vec<Toast>,
    pub has_ffmpeg: bool,
    pub confirm_delete: Option<Vec<PathBuf>>,
}

const SETTINGS_KEY: &str = "dropzone-settings";

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Vec<PathBuf>) -> Self {
        style(&cc.egui_ctx);
        let settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, SETTINGS_KEY)).unwrap_or_default();

        let events: Arc<Mutex<Vec<QueueEvent>>> = Arc::default();
        let queue = {
            let events = events.clone();
            let ctx = cc.egui_ctx.clone();
            JobQueue::new(JobQueue::default_workers(), move |e| {
                events.lock().unwrap().push(e);
                ctx.request_repaint();
            })
        };

        // Start where the user left off, or in their home folder.
        let start = settings.last_folder.clone().filter(|p| p.is_dir()).or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from("/"));

        let mut app = Self {
            loc: Location::Folder(start),
            back: Vec::new(),
            fwd: Vec::new(),
            items: Vec::new(),
            visible: Vec::new(),
            visible_dirty: true,
            load: Task::default(),
            load_error: None,
            archive_listing: None,
            archive_password: String::new(),
            needs_password: false,
            settings,
            filter: String::new(),
            path_edit: None,
            focus_search: false,
            selection: BTreeSet::new(),
            anchor: None,
            focus: None,
            renaming: None,
            scroll_to_focus: false,
            last_scroll: (0.0, 0.0),
            thumbs: Thumbs::new(cc.egui_ctx.clone()),
            details: Details::default(),
            clip: None,
            tool: None,
            tool_inputs_cache: None,
            rename_win: None,
            compare: None,
            side_task: Task::default(),
            tracked: Vec::new(),
            banner: None,
            reload_pending: false,
            queue,
            events,
            jobs: Vec::new(),
            queue_open: false,
            quit_when_done: false,
            allow_close: false,
            close_prompt: false,
            toasts: Vec::new(),
            has_ffmpeg: dropzone_core::media::ffmpeg_path().is_ok(),
            confirm_delete: None,
        };

        // "Open with Dropzone" on a folder or archive opens it; on files, opens their folder with them selected.
        match initial.first() {
            Some(p) if p.is_dir() => app.loc = Location::Folder(p.clone()),
            Some(p) if dropzone_core::archive::is_archive(p) => app.loc = Location::Archive { archive: ArchiveRef::file(p), dir: String::new() },
            Some(p) => {
                if let Some(parent) = p.parent() {
                    app.loc = Location::Folder(parent.to_path_buf());
                }
                for f in &initial {
                    if let Some(n) = f.file_name() {
                        app.selection.insert(n.to_string_lossy().into_owned());
                    }
                }
            }
            None => {}
        }
        app.reload(&cc.egui_ctx);
        app
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: false, at: Instant::now() });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: true, at: Instant::now() });
    }

    pub fn enqueue(&mut self, inputs: Vec<PathBuf>, operation: Operation, output_dir: Option<PathBuf>) -> Vec<u64> {
        self.queue.enqueue(JobSpec { inputs, operation, output_dir })
    }

    fn pump_queue_events(&mut self, ctx: &egui::Context) {
        let events: Vec<QueueEvent> = std::mem::take(&mut *self.events.lock().unwrap());
        let mut refresh = false;
        for e in events {
            match e {
                QueueEvent::Job(info) => {
                    if info.status.is_finished() {
                        // Anything that wrote into the folder we're looking at should show up.
                        if let Some(dir) = self.loc.folder() {
                            refresh |= info.outputs.iter().any(|o| o.parent() == Some(dir) || o == dir) || info.inputs.iter().any(|i| i.parent() == Some(dir));
                        }
                    }
                    match self.jobs.iter_mut().find(|j| j.id == info.id) {
                        Some(j) => *j = info,
                        None => self.jobs.push(info),
                    }
                }
                QueueEvent::Idle { done, failed } => {
                    if failed > 0 {
                        self.error(format!("All jobs finished: {done} done, {failed} failed"));
                        self.queue_open = true;
                    } else if done > 1 {
                        self.toast(format!("All {done} jobs done"));
                    }
                    if self.quit_when_done {
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }
        if refresh {
            self.reload(ctx);
        }
        self.poll_tracked();
    }

    fn handle_drop(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if dropped.is_empty() {
            return;
        }
        // Dropping into a window copies into the current folder, like Explorer.
        let Some(dir) = self.loc.folder().map(|p| p.to_path_buf()) else {
            self.error("Can't drop into an archive yet. Open a folder first.");
            return;
        };
        let (here, elsewhere): (Vec<PathBuf>, Vec<PathBuf>) = dropped.into_iter().partition(|p| p.parent() == Some(dir.as_path()));
        if !elsewhere.is_empty() {
            let n = elsewhere.len();
            self.enqueue(elsewhere, Operation::Copy { dest: dir }, None);
            self.toast(format!("Copying {n} item(s) here"));
        }
        for p in here {
            if let Some(n) = p.file_name() {
                self.selection.insert(n.to_string_lossy().into_owned());
            }
        }
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.queue.is_busy() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_prompt = true;
        }
        if !self.close_prompt {
            return;
        }
        let modal = egui::Modal::new(egui::Id::unique("close-prompt")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Jobs are still running");
            ui.add_space(6.0);
            ui.label("Closing now would stop them. What should happen?");
            ui.add_space(10.0);
            ui.vertical_centered_justified(|ui| {
                if ui.button("Quit when the queue finishes").clicked() {
                    self.quit_when_done = true;
                    self.close_prompt = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                if ui.button("Cancel jobs and quit now").clicked() {
                    self.queue.shutdown();
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Keep working").clicked() {
                    self.close_prompt = false;
                }
            });
        });
        if modal.should_close() {
            self.close_prompt = false;
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        // Leave typing alone: shortcuts only apply when no text field has focus.
        if ctx.memory(|m| m.focused().is_some()) && ctx.egui_wants_keyboard_input() {
            return;
        }
        if self.rename_win.is_some() || self.confirm_delete.is_some() {
            return;
        }
        let cmd = Modifiers::COMMAND;
        let pressed = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
        let shortcut = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));

        if shortcut(Modifiers::ALT, Key::ArrowLeft) || ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra1)) {
            self.go_back(ctx);
        }
        if shortcut(Modifiers::ALT, Key::ArrowRight) || ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra2)) {
            self.go_forward(ctx);
        }
        if shortcut(Modifiers::ALT, Key::ArrowUp) || pressed(Key::Backspace) {
            self.go_up(ctx);
        }
        if pressed(Key::F5) || shortcut(cmd, Key::R) {
            self.reload(ctx);
        }
        if shortcut(cmd, Key::L) {
            self.path_edit = Some(self.loc_text());
        }
        if shortcut(cmd, Key::F) {
            self.focus_search = true;
        }
        if shortcut(cmd, Key::A) {
            self.selection = self.visible.iter().map(|&i| self.items[i].key.clone()).collect();
        }
        if shortcut(cmd, Key::C) {
            self.copy_selection(false);
        }
        if shortcut(cmd, Key::X) {
            self.copy_selection(true);
        }
        if shortcut(cmd, Key::V) {
            self.paste();
        }
        if shortcut(cmd | Modifiers::SHIFT, Key::N) {
            self.new_folder(ctx);
        }
        if pressed(Key::F2) {
            self.start_rename();
        }
        if pressed(Key::Delete) {
            self.ask_delete();
        }
        if pressed(Key::Escape) {
            self.selection.clear();
        }
        if pressed(Key::Enter)
            && let Some(f) = self.focus.clone().or_else(|| self.selection.iter().next().cloned())
        {
            self.open_item(ctx, &f);
        }
        let down = pressed(Key::ArrowDown) || (self.settings.view == crate::app::ViewMode::Grid && pressed(Key::ArrowRight));
        let up = pressed(Key::ArrowUp) || (self.settings.view == crate::app::ViewMode::Grid && pressed(Key::ArrowLeft));
        if (down || up) && !self.visible.is_empty() {
            let cur = self.focus.as_ref().and_then(|f| self.visible.iter().position(|&i| &self.items[i].key == f));
            let next = match (cur, down) {
                (None, _) => 0,
                (Some(c), true) => (c + 1).min(self.visible.len() - 1),
                (Some(c), false) => c.saturating_sub(1),
            };
            let key = self.items[self.visible[next]].key.clone();
            self.select_only(key);
            self.scroll_to_focus = true;
        }
    }

    pub fn loc_text(&self) -> String {
        match &self.loc {
            Location::Folder(p) => p.display().to_string(),
            Location::Archive { archive, dir } => {
                let mut s = archive.path.display().to_string();
                for i in &archive.inner {
                    s.push_str(" › ");
                    s.push_str(i);
                }
                if !dir.is_empty() {
                    s.push_str(" › ");
                    s.push_str(dir);
                }
                s
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let n = self.visible.len();
            ui.label(RichText::new(format!("{n} item{}", if n == 1 { "" } else { "s" })).small().weak());
            let sel = self.selected_items();
            if !sel.is_empty() {
                let size: u64 = sel.iter().filter_map(|i| i.size).sum();
                ui.label(RichText::new(format!("· {} selected · {}", sel.len(), common::human_size(size))).small().weak());
            }
            if let Some((paths, cut)) = &self.clip {
                ui.label(RichText::new(format!("· {} {} on clipboard", paths.len(), if *cut { "cut" } else { "copied" })).small().weak());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let active: Vec<&JobInfo> = self.jobs.iter().filter(|j| !j.status.is_finished()).collect();
                let failed = self.jobs.iter().filter(|j| j.status == JobStatus::Failed).count();
                let text = if !active.is_empty() {
                    let p = active.iter().map(|j| j.progress).sum::<f32>() / active.len() as f32;
                    format!("⟳ {} job{} · {:.0}%", active.len(), if active.len() == 1 { "" } else { "s" }, p * 100.0)
                } else if failed > 0 {
                    format!("⚠ {failed} failed")
                } else if !self.jobs.is_empty() {
                    "✔ Queue done".into()
                } else {
                    "Queue".into()
                };
                let color = if failed > 0 && active.is_empty() { common::ERR } else { ui.visuals().text_color() };
                if ui.add(egui::Button::new(RichText::new(text).small().color(color)).frame(false)).clicked() {
                    self.queue_open = !self.queue_open;
                }
                if !self.has_ffmpeg {
                    ui.label(RichText::new("FFmpeg not found: audio/video conversion off").small().color(common::WARN))
                        .on_hover_text("Put ffmpeg.exe next to dropzone.exe or on your PATH.");
                }
                if self.quit_when_done {
                    ui.label(RichText::new("Will quit when the queue finishes").small().color(common::WARN));
                }
            });
        });
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.at.elapsed() < Duration::from_secs(if t.error { 10 } else { 4 }));
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        egui::Area::new(egui::Id::unique("toasts")).anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -40.0]).order(egui::Order::Foreground).show(ctx, |ui| {
            let mut dismiss = None;
            for (i, t) in self.toasts.iter().enumerate() {
                let fill = if t.error { common::ERR } else { Color32::from_gray(38) };
                egui::Frame::NONE.fill(fill).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
                    ui.set_max_width(380.0);
                    let r = ui.add(egui::Label::new(RichText::new(&t.text).color(Color32::WHITE)).wrap().sense(egui::Sense::click()));
                    if r.clicked() {
                        dismiss = Some(i);
                    }
                });
                ui.add_space(6.0);
            }
            if let Some(i) = dismiss {
                self.toasts.remove(i);
            }
        });
    }

    fn drag_overlay(&self, ctx: &egui::Context, rect: egui::Rect) {
        if !ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            return;
        }
        let text = match &self.loc {
            Location::Folder(p) => format!("Copy into {}", common::file_name(p)),
            Location::Archive { .. } => "Open a folder to drop files into it".into(),
        };
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop-overlay")));
        let accent = ctx.global_style().visuals.selection.bg_fill;
        painter.rect_filled(rect, 0.0, accent.gamma_multiply(0.12));
        painter.rect_stroke(rect.shrink(2.0), 6.0, egui::Stroke::new(2.0, accent), egui::StrokeKind::Inside);
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(20.0), accent);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump_queue_events(&ctx);
        self.poll_load();
        self.poll_side_task();
        if std::mem::take(&mut self.reload_pending) {
            self.reload(&ctx);
        }
        self.thumbs.poll();
        self.handle_drop(&ctx);
        self.keyboard(&ctx);
        self.refresh_visible();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{} – Dropzone", self.loc.title())));

        let panel_fill = ctx.global_style().visuals.panel_fill;
        let side_fill = if ctx.global_style().visuals.dark_mode { Color32::from_gray(24) } else { Color32::from_gray(243) };

        egui::Panel::top("toolbar").frame(egui::Frame::NONE.fill(panel_fill).inner_margin(egui::Margin::symmetric(10, 8))).show(ui, |ui| self.toolbar(ui));

        egui::Panel::bottom("status").frame(egui::Frame::NONE.fill(side_fill).inner_margin(egui::Margin::symmetric(12, 4))).show(ui, |ui| self.status_bar(ui));

        if self.queue_open {
            egui::Panel::bottom("queue")
                .resizable(true)
                .default_size(240.0)
                .size_range(120.0..=600.0)
                .frame(egui::Frame::NONE.fill(panel_fill).inner_margin(egui::Margin::symmetric(12, 8)))
                .show(ui, |ui| crate::queue_panel::ui(ui, self));
        }

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(190.0)
            .size_range(150.0..=320.0)
            .frame(egui::Frame::NONE.fill(side_fill).inner_margin(egui::Margin::symmetric(8, 10)))
            .show(ui, |ui| self.sidebar(ui));

        if self.tool.is_some() {
            egui::Panel::right("tool")
                .resizable(false)
                .exact_size(330.0)
                .frame(egui::Frame::NONE.fill(side_fill).inner_margin(egui::Margin::symmetric(12, 10)))
                .show(ui, |ui| self.tool_panel(ui));
        } else if self.settings.details_open {
            egui::Panel::right("details")
                .resizable(true)
                .default_size(280.0)
                .size_range(200.0..=520.0)
                .frame(egui::Frame::NONE.fill(side_fill).inner_margin(egui::Margin::symmetric(12, 10)))
                .show(ui, |ui| self.details_panel(ui));
        }

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(panel_fill).inner_margin(egui::Margin { left: 4, right: 4, top: 0, bottom: 0 })).show(
            ui,
            |ui| {
                let rect = ui.max_rect();
                self.main_view(ui);
                self.drag_overlay(&ctx, rect);
            },
        );

        self.rename_window(&ctx);
        self.compare_window(&ctx);
        self.delete_prompt(&ctx);
        self.handle_close(&ctx);
        self.toasts(&ctx);

        if self.jobs.iter().any(|j| j.status == JobStatus::Running) {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some(t) = &self.tool {
            self.settings.convert = t.settings.clone();
        }
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
    }
}

fn style(ctx: &egui::Context) {
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.visuals.window_corner_radius = 10.0.into();
        s.visuals.menu_corner_radius = 8.0.into();
        use egui::{FontFamily, FontId, TextStyle};
        s.text_styles.insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(14.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Heading, FontId::new(19.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Small, FontId::new(12.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace));
    });
}
