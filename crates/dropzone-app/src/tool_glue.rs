//! Connects the tools panel and the rename window to the explorer's selection.

use std::path::PathBuf;

use eframe::egui;

use crate::app::App;
use crate::common::Kind;
use crate::nav::{Item, Location};
use crate::rename_win::RenameTab;
use crate::tools::{Input, OpKind, Request, ToolPanel};

impl App {
    pub fn open_tool(&mut self, op: OpKind) {
        let settings = match &self.tool {
            Some(t) => t.settings.clone(),
            None => self.settings.convert.clone(),
        };
        self.tool = Some(ToolPanel::new(settings, op));
    }

    /// The tool's inputs: the selection (folders expanded), or everything here if nothing is selected.
    fn tool_inputs(&self) -> (Vec<Input>, String) {
        if self.loc.folder().is_none() {
            return (Vec::new(), "this archive".into());
        }
        let sel: Vec<&Item> = self.selected_items();
        let (items, scope): (Vec<&Item>, String) = if sel.is_empty() {
            (self.visible.iter().map(|&i| &self.items[i]).collect(), "the files in this folder".into())
        } else {
            let n = sel.len();
            (sel, format!("{n} selected item{}", if n == 1 { "" } else { "s" }))
        };
        let mut out = Vec::new();
        for it in items {
            if it.is_dir {
                for f in dropzone_core::util::expand_paths(std::slice::from_ref(&it.path)) {
                    out.push(Input { kind: Kind::of_name(&crate::common::file_name(&f), false), path: f, root: Some(it.path.clone()) });
                }
            } else {
                out.push(Input { path: it.path.clone(), kind: it.kind, root: None });
            }
        }
        (out, scope)
    }

    pub fn tool_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Expanding folders walks the disk, so only redo it when the selection changes.
        let sig = format!("{}|{:?}|{}", self.loc_text(), self.selection, self.items.len());
        if self.tool_inputs_cache.as_ref().is_none_or(|c| c.0 != sig) {
            let (inputs, scope) = self.tool_inputs();
            self.tool_inputs_cache = Some((sig, inputs, scope));
        }
        let (_, inputs, scope) = self.tool_inputs_cache.clone().unwrap();
        let has_ffmpeg = self.has_ffmpeg;
        let Some(tool) = &mut self.tool else { return };
        match tool.ui(ui, &inputs, &scope, has_ffmpeg) {
            Some(Request::Close) => {
                self.settings.convert = tool.settings.clone();
                self.tool = None;
            }
            Some(Request::Enqueue(paths, op, out)) => {
                let n = self.enqueue(paths, op, out).len();
                self.toast(format!("Started {n} job{}", if n == 1 { "" } else { "s" }));
                self.queue_open = true;
            }
            Some(Request::OpenArchive(p)) => {
                self.navigate(&ctx, Location::Archive { archive: dropzone_core::archive::ArchiveRef::file(p), dir: String::new() });
            }
            None => {}
        }
    }

    pub fn open_rename_window(&mut self) {
        if self.loc.folder().is_none() {
            return;
        }
        let mut files: Vec<PathBuf> = self.selected_paths();
        if files.is_empty() {
            files = self.visible.iter().map(|&i| self.items[i].path.clone()).collect();
        }
        if files.is_empty() {
            return;
        }
        self.rename_win = Some(RenameTab::new(self.settings.rename.clone(), files));
    }

    pub fn rename_window(&mut self, ctx: &egui::Context) {
        let Some(win) = &mut self.rename_win else { return };
        let mut renamed = false;
        let open = win.window(ctx, &mut renamed);
        self.settings.rename = win.rule.clone();
        if renamed {
            self.reload(ctx);
        }
        if !open {
            self.rename_win = None;
        }
    }
}
