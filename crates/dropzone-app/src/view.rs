//! Toolbar, the file list/grid, selection, context menus and everyday file operations.

use std::path::PathBuf;

use dropzone_core::archive::{ArchiveRef, ExtractOptions};
use dropzone_core::ops::Operation;
use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Vec2};

use crate::app::{App, Renaming, ViewMode};
use crate::nav::{Item, Location, SortKey};
use crate::tools::OpKind;
use crate::{common, icons, theme};

const ROW_H: f32 = 32.0;
const TILE: Vec2 = Vec2::new(124.0, 146.0);

impl App {
    pub fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if theme::icon_button(ui, icons::ARROW_LEFT, !self.back.is_empty(), false, "Back (Alt+Left)").clicked() {
                self.go_back(&ctx);
            }
            if theme::icon_button(ui, icons::ARROW_RIGHT, !self.fwd.is_empty(), false, "Forward (Alt+Right)").clicked() {
                self.go_forward(&ctx);
            }
            if theme::icon_button(ui, icons::ARROW_UP, self.loc.parent().is_some(), false, "Up (Backspace)").clicked() {
                self.go_up(&ctx);
            }
            if theme::icon_button(ui, icons::ARROW_CLOCKWISE, true, false, "Refresh (F5)").clicked() {
                self.reload(&ctx);
            }
            ui.add_space(8.0);

            // Right side first so the path bar can take whatever is left.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let details_on = self.settings.details_open && self.tool.is_none();
                if theme::icon_button(ui, icons::SIDEBAR_SIMPLE, true, details_on, "Details pane").clicked() {
                    self.settings.details_open = !details_on;
                    self.tool = None;
                }

                // List / grid as a two-way segmented control.
                egui::Frame::NONE.fill(theme::FIELD).corner_radius(theme::RADIUS + 1).inner_margin(2).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let grid = self.settings.view == ViewMode::Grid;
                    if theme::icon_button(ui, icons::SQUARES_FOUR, true, grid, "Thumbnails").clicked() {
                        self.settings.view = ViewMode::Grid;
                    }
                    if theme::icon_button(ui, icons::ROWS, true, !grid, "Details list").clicked() {
                        self.settings.view = ViewMode::List;
                    }
                });

                let tools = egui::Button::new(format!("{}  Tools  {}", icons::MAGIC_WAND, icons::CARET_DOWN)).min_size(Vec2::new(0.0, 34.0));
                egui::containers::menu::MenuButton::from_button(tools).ui(ui, |ui| self.tools_menu(ui));

                self.search_box(ui);
                self.path_bar(ui);
            });
        });
    }

    fn search_box(&mut self, ui: &mut egui::Ui) {
        egui::Frame::NONE.fill(theme::FIELD).corner_radius(theme::RADIUS + 1).inner_margin(egui::Margin::symmetric(10, 0)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.allocate_ui_with_layout(Vec2::new(190.0, 34.0), Layout::left_to_right(Align::Center), |ui| {
                ui.set_min_size(Vec2::new(190.0, 34.0));
                ui.label(RichText::new(icons::MAGNIFYING_GLASS).size(15.0).color(theme::TEXT_3));
                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text(RichText::new("Search").color(theme::TEXT_3))
                        .frame(egui::Frame::NONE)
                        .align(egui::Align2::LEFT_CENTER)
                        .min_size(Vec2::new(0.0, 34.0))
                        .desired_width(160.0),
                );
                if self.focus_search {
                    search.request_focus();
                    self.focus_search = false;
                }
                if search.changed() {
                    self.visible_dirty = true;
                }
                if search.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.filter.clear();
                    self.visible_dirty = true;
                    search.surrender_focus();
                }
            });
        });
    }

    fn path_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let width = ui.available_width();
        let frame = egui::Frame::NONE.fill(theme::FIELD).corner_radius(theme::RADIUS + 1).inner_margin(egui::Margin::symmetric(10, 0));
        if let Some(mut text) = self.path_edit.take() {
            let mut keep = true;
            let mut entered = None;
            frame.stroke(egui::Stroke::new(1.0, theme::ACCENT)).show(ui, |ui| {
                ui.allocate_ui_with_layout(Vec2::new((width - 22.0).max(40.0), 32.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.set_min_size(Vec2::new((width - 22.0).max(40.0), 32.0));
                    let r = ui.add(egui::TextEdit::singleline(&mut text).frame(egui::Frame::NONE).desired_width(f32::INFINITY));
                    if !r.has_focus() && !r.lost_focus() {
                        r.request_focus();
                    }
                    if r.lost_focus() {
                        keep = false;
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            entered = Some(text.trim().to_string()).filter(|t| !t.is_empty());
                        }
                    }
                });
            });
            if keep {
                self.path_edit = Some(text);
            }
            if let Some(entered) = entered {
                let p = PathBuf::from(expand_home(&entered));
                if p.is_dir() {
                    self.navigate(&ctx, Location::Folder(p));
                } else if p.is_file() && dropzone_core::archive::is_archive(&p) {
                    self.navigate(&ctx, Location::Archive { archive: ArchiveRef::file(p), dir: String::new() });
                } else if p.is_file() {
                    common::open_path(&p);
                } else {
                    self.error(format!("Can't find {entered}"));
                }
            }
            return;
        }
        // Breadcrumbs in a pill; click the empty part to type a path.
        let resp = frame.show(ui, |ui| {
            let mut target = None;
            // The toolbar lays out right-to-left; crumbs read left-to-right.
            ui.allocate_ui_with_layout(Vec2::new((width - 20.0).max(40.0), 34.0), Layout::left_to_right(Align::Center), |ui| {
                ui.set_min_size(Vec2::new((width - 20.0).max(40.0), 34.0));
                ui.spacing_mut().item_spacing.x = 2.0;
                let in_archive = matches!(self.loc, Location::Archive { .. });
                let icon = if in_archive { icons::FILE_ZIP } else { icons::FOLDER_OPEN };
                ui.label(RichText::new(icon).size(15.0).color(theme::TEXT_3));
                ui.add_space(4.0);
                let crumbs = self.loc.crumbs();
                let last = crumbs.len().saturating_sub(1);
                // Keep the tail visible when the path is long.
                let skip = crumbs.len().saturating_sub(5);
                if skip > 0 {
                    ui.label(RichText::new("…").color(theme::TEXT_3));
                }
                for (i, (label, loc)) in crumbs.into_iter().enumerate().skip(skip) {
                    if i > skip || skip > 0 {
                        ui.label(RichText::new(icons::CARET_RIGHT).size(11.0).color(theme::TEXT_3));
                    }
                    let text = if i == last { RichText::new(label).color(theme::TEXT) } else { RichText::new(label).color(theme::TEXT_2) };
                    if ui.add(egui::Button::new(text).frame_when_inactive(false).min_size(Vec2::new(0.0, 26.0))).clicked() {
                        target = Some(loc);
                    }
                }
            });
            target
        });
        if let Some(t) = resp.inner {
            self.navigate(&ctx, t);
        } else if resp.response.interact(Sense::click()).clicked() {
            self.path_edit = Some(self.loc_text());
        }
    }

    pub fn main_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if matches!(self.loc, Location::Archive { .. }) {
            self.archive_bar(ui);
        }
        if let Some(e) = self.load_error.clone() {
            if !self.needs_password {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Can't open this location").strong());
                    ui.label(RichText::new(e).color(common::ERR));
                });
            }
            return;
        }
        if self.load.running() && self.items.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.spinner());
            return;
        }
        if self.visible.is_empty() {
            let msg = if !self.filter.is_empty() { "No matches" } else { "This folder is empty" };
            let r = ui.allocate_response(ui.available_size(), Sense::click());
            ui.painter().text(
                r.rect.center_top() + egui::vec2(0.0, 60.0),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(15.0),
                ui.visuals().weak_text_color(),
            );
            r.context_menu(|ui| self.background_menu(ui));
            return;
        }
        match self.settings.view {
            ViewMode::List => self.list_view(ui),
            ViewMode::Grid => self.grid_view(ui),
        }
        let _ = ctx;
    }

    fn header(&mut self, ui: &mut egui::Ui, widths: &[f32; 4], archive: bool) {
        let cols: [(SortKey, &str); 4] =
            [(SortKey::Name, "Name"), (SortKey::Modified, "Modified"), (SortKey::Type, if archive { "Packed" } else { "Type" }), (SortKey::Size, "Size")];
        // Painted at the same x positions as the rows so the columns line up.
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
        let mut x = rect.left() + 10.0;
        for (i, (key, label)) in cols.iter().enumerate() {
            let active = self.settings.sort_key == *key;
            let cell = egui::Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(widths[i], rect.height()));
            let r = ui.interact(cell, ui.scope_id().with(("header", i)), Sense::click());
            let color = if active || r.hovered() { theme::TEXT_2 } else { theme::TEXT_3 };
            let font = theme::strong(11.5);
            let label_x = if i == 0 { x + 30.0 } else { x };
            let galley = ui.painter().layout_no_wrap(label.to_string(), font, color);
            let w = galley.size().x;
            let pos = if i == 3 { egui::pos2(cell.right() - 10.0 - w, cell.center().y) } else { egui::pos2(label_x, cell.center().y) };
            ui.painter().galley(pos - Vec2::new(0.0, galley.size().y / 2.0), galley, color);
            if active {
                let caret = if self.settings.sort_asc { icons::CARET_UP } else { icons::CARET_DOWN };
                let cx = if i == 3 { pos.x - 9.0 } else { pos.x + w + 9.0 };
                ui.painter().text(egui::pos2(cx, cell.center().y), egui::Align2::CENTER_CENTER, caret, egui::FontId::proportional(10.0), color);
            }
            if r.clicked() {
                if active {
                    self.settings.sort_asc = !self.settings.sort_asc;
                } else {
                    self.settings.sort_key = *key;
                    self.settings.sort_asc = true;
                }
                self.visible_dirty = true;
            }
            x += widths[i];
        }
        ui.painter().hline(rect.x_range().shrink(4.0), rect.bottom(), egui::Stroke::new(1.0, theme::BORDER));
        ui.add_space(4.0);
    }

    fn list_view(&mut self, ui: &mut egui::Ui) {
        let archive = matches!(self.loc, Location::Archive { .. });
        let total_w = ui.available_width();
        let fixed = [150.0, 120.0, 90.0];
        let name_w = (total_w - fixed.iter().sum::<f32>() - 30.0).max(160.0);
        let widths = [name_w, fixed[0], fixed[1], fixed[2]];
        self.header(ui, &widths, archive);

        let n = self.visible.len();
        let mut scroll = egui::ScrollArea::vertical().id_salt("list").auto_shrink([false, false]);
        if let Some(offset) = self.scroll_offset_for_focus(ROW_H + ui.spacing().item_spacing.y, 1) {
            scroll = scroll.vertical_scroll_offset(offset);
        }
        let out = scroll.show_rows(ui, ROW_H, n, |ui, range| {
            for vi in range {
                let item = self.items[self.visible[vi]].clone();
                self.list_row(ui, &item, &widths, archive);
            }
        });
        self.last_scroll = (out.state.clamped_offset().y, out.inner_rect.height());
        // Clicking empty space below the rows clears the selection and offers folder actions.
        let rest = ui.allocate_response(ui.available_size().max(Vec2::new(0.0, 0.0)), Sense::click());
        if rest.clicked() {
            self.selection.clear();
        }
        rest.context_menu(|ui| self.background_menu(ui));
    }

    /// Scroll offset that brings the focused row into view, if it's currently off-screen.
    fn scroll_offset_for_focus(&mut self, row_h: f32, per_row: usize) -> Option<f32> {
        if !self.scroll_to_focus {
            return None;
        }
        self.scroll_to_focus = false;
        let f = self.focus.as_ref()?;
        let idx = self.visible.iter().position(|&i| &self.items[i].key == f)? / per_row.max(1);
        let top = idx as f32 * row_h;
        let (offset, height) = self.last_scroll;
        if top < offset {
            Some(top)
        } else if top + row_h > offset + height {
            Some(top + row_h - height)
        } else {
            None
        }
    }

    fn list_row(&mut self, ui: &mut egui::Ui, item: &Item, widths: &[f32; 4], archive: bool) {
        let ctx = ui.ctx().clone();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
        let selected = self.selection.contains(&item.key);
        let dim = self.is_cut(item) || item.hidden;
        paint_row_bg(ui, rect, selected, resp.hovered());

        let mut x = rect.left() + 10.0;
        let painter = ui.painter_at(rect);
        let text_color = if dim { theme::TEXT_3 } else { theme::TEXT };
        let font = egui::FontId::proportional(13.5);
        let small = egui::FontId::proportional(12.5);
        let cy = rect.center().y;

        // Name column (or the inline rename box).
        theme::paint_file_icon(&painter, egui::pos2(x + 10.0, cy), 18.0, item.kind, item.is_archive);
        let name_x = x + 30.0;
        if self.renaming.as_ref().is_some_and(|r| r.key == item.key) {
            let r = egui::Rect::from_min_size(egui::pos2(name_x - 4.0, rect.top() + 3.0), Vec2::new(widths[0] - 36.0, ROW_H - 6.0));
            self.rename_box(ui, r);
        } else {
            let galley = ui.painter().layout_no_wrap(item.name.clone(), font.clone(), text_color);
            let clip = egui::Rect::from_min_max(egui::pos2(name_x, rect.top()), egui::pos2(x + widths[0] - 12.0, rect.bottom()));
            let name_w = galley.size().x;
            ui.painter_at(clip).galley(egui::pos2(name_x, cy - galley.size().y / 2.0), galley, text_color);
            if item.encrypted {
                let lx = (name_x + name_w + 12.0).min(clip.right() - 4.0);
                painter.text(egui::pos2(lx, cy), egui::Align2::CENTER_CENTER, icons::LOCK_SIMPLE, egui::FontId::proportional(13.0), common::WARN);
            }
        }
        x += widths[0];
        let modified = item.modified.map(|m| m.format("%b %-d, %Y  %H:%M").to_string()).unwrap_or_default();
        painter.text(egui::pos2(x, cy), egui::Align2::LEFT_CENTER, modified, small.clone(), theme::TEXT_2);
        x += widths[1];
        let third = if archive { item.packed.map(common::human_size).unwrap_or_default() } else { item.type_label() };
        painter.text(egui::pos2(x, cy), egui::Align2::LEFT_CENTER, third, small.clone(), theme::TEXT_2);
        x += widths[2];
        let size = match (item.is_dir, item.size) {
            (true, Some(s)) if s > 0 => common::human_size(s),
            (true, _) => String::new(),
            (false, Some(s)) => common::human_size(s),
            _ => String::new(),
        };
        painter.text(egui::pos2(x + widths[3] - 10.0, cy), egui::Align2::RIGHT_CENTER, size, small, theme::TEXT_2);

        self.item_interaction(&ctx, ui, &resp, item);
    }

    fn grid_view(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let cols = ((width / TILE.x).floor() as usize).max(1);
        let rows = self.visible.len().div_ceil(cols);
        let mut scroll = egui::ScrollArea::vertical().id_salt("grid").auto_shrink([false, false]);
        if let Some(offset) = self.scroll_offset_for_focus(TILE.y, cols) {
            scroll = scroll.vertical_scroll_offset(offset);
        }
        let out = scroll.show_rows(ui, TILE.y - ui.spacing().item_spacing.y, rows, |ui, range| {
            for row in range {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for c in 0..cols {
                        let vi = row * cols + c;
                        let Some(&ii) = self.visible.get(vi) else { break };
                        let item = self.items[ii].clone();
                        self.tile(ui, &item);
                    }
                });
            }
        });
        self.last_scroll = (out.state.clamped_offset().y, out.inner_rect.height());
        let rest = ui.allocate_response(ui.available_size(), Sense::click());
        if rest.clicked() {
            self.selection.clear();
        }
        rest.context_menu(|ui| self.background_menu(ui));
    }

    fn tile(&mut self, ui: &mut egui::Ui, item: &Item) {
        let ctx = ui.ctx().clone();
        let (rect, resp) = ui.allocate_exact_size(TILE - Vec2::new(0.0, ui.spacing().item_spacing.y), Sense::click());
        let selected = self.selection.contains(&item.key);
        paint_row_bg(ui, rect.shrink(4.0), selected, resp.hovered());
        let thumb_rect = egui::Rect::from_center_size(egui::pos2(rect.center().x, rect.top() + 54.0), Vec2::splat(84.0));
        let thumb = if item.kind == common::Kind::Image && self.loc.folder().is_some() { self.thumbs.get(&item.path, item.size.unwrap_or(0)) } else { None };
        match thumb {
            Some(tex) => {
                let size = tex.size_vec2();
                let scale = (84.0 / size.x).min(84.0 / size.y);
                let r = egui::Rect::from_center_size(thumb_rect.center(), size * scale);
                ui.painter().rect_filled(r.expand(1.0), 7.0, theme::BORDER);
                egui::Image::from_texture(&tex).corner_radius(6).paint_at(ui, r);
            }
            None => theme::paint_file_icon(ui.painter(), thumb_rect.center(), 58.0, item.kind, item.is_archive),
        }
        let name_rect = egui::Rect::from_min_max(egui::pos2(rect.left() + 8.0, rect.top() + 102.0), egui::pos2(rect.right() - 8.0, rect.bottom() - 4.0));
        if self.renaming.as_ref().is_some_and(|r| r.key == item.key) {
            self.rename_box(ui, name_rect.with_max_y(name_rect.top() + 26.0));
        } else {
            let color = if self.is_cut(item) || item.hidden { theme::TEXT_3 } else { theme::TEXT };
            let mut job = egui::text::LayoutJob::simple(item.name.clone(), egui::FontId::proportional(12.5), color, name_rect.width());
            job.wrap.max_rows = 2;
            job.wrap.break_anywhere = true;
            job.halign = Align::Center;
            let galley = ui.painter().layout_job(job);
            ui.painter().galley(egui::pos2(name_rect.center().x, name_rect.top()), galley, color);
        }
        self.item_interaction(&ctx, ui, &resp, item);
    }

    fn rename_box(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let ctx = ui.ctx().clone();
        let Some(r) = &mut self.renaming else { return };
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let edit = child.add(egui::TextEdit::singleline(&mut r.text).desired_width(rect.width()));
        if !r.focused {
            edit.request_focus();
            // Select the name without its extension, like Explorer does.
            let stem_len = r.text.rfind('.').filter(|&i| i > 0).unwrap_or(r.text.len());
            if let Some(mut state) = egui::TextEdit::load_state(&ctx, edit.id) {
                let range = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(r.text[..stem_len].chars().count()));
                state.cursor.set_char_range(Some(range));
                state.store(&ctx, edit.id);
            }
            r.focused = true;
        }
        if edit.lost_focus() {
            let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
            let r = self.renaming.take().unwrap();
            if !cancel {
                self.commit_rename(&ctx, r);
            }
        }
    }

    fn item_interaction(&mut self, ctx: &egui::Context, ui: &egui::Ui, resp: &egui::Response, item: &Item) {
        if resp.clicked() {
            let m = ui.input(|i| i.modifiers);
            if m.command {
                if !self.selection.remove(&item.key) {
                    self.selection.insert(item.key.clone());
                }
                self.anchor = Some(item.key.clone());
                self.focus = Some(item.key.clone());
            } else if m.shift && self.anchor.is_some() {
                let pos = |k: &str| self.visible.iter().position(|&i| self.items[i].key == k);
                if let (Some(a), Some(b)) = (pos(self.anchor.as_deref().unwrap()), pos(&item.key)) {
                    let (lo, hi) = (a.min(b), a.max(b));
                    self.selection = self.visible[lo..=hi].iter().map(|&i| self.items[i].key.clone()).collect();
                }
                self.focus = Some(item.key.clone());
            } else {
                self.select_only(item.key.clone());
            }
        }
        if resp.double_clicked() {
            self.open_item(ctx, &item.key);
        }
        if resp.secondary_clicked() && !self.selection.contains(&item.key) {
            self.select_only(item.key.clone());
        }
        if self.loc.folder().is_some() && !item.is_dir {
            resp.clone().on_hover_text_at_pointer(format!("{}\n{}", item.name, item.size.map(common::human_size).unwrap_or_default()));
        }
        resp.context_menu(|ui| self.item_menu(ui));
    }

    fn is_cut(&self, item: &Item) -> bool {
        matches!(&self.clip, Some((paths, true)) if self.loc.folder().is_some() && paths.contains(&item.path))
    }

    // --- Menus ----------------------------------------------------------------------

    fn item_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let sel: Vec<Item> = self.selected_items().into_iter().cloned().collect();
        let Some(first) = sel.first().cloned() else { return };
        let in_archive = matches!(self.loc, Location::Archive { .. });

        if ui.add(theme::menu_item_key(icons::ARROW_SQUARE_OUT, "Open", "Enter")).clicked() {
            for it in &sel {
                self.open_item(&ctx, &it.key);
            }
            ui.close();
        }
        if in_archive {
            if ui.add(theme::menu_item(icons::TRAY_ARROW_DOWN, "Extract…")).clicked() {
                self.extract_from_archive(Some(sel.iter().map(|i| i.key.clone()).collect()), None);
                ui.close();
            }
            if ui.add(theme::menu_item(icons::CLIPBOARD, "Copy name")).clicked() {
                ui.ctx().copy_text(sel.iter().map(|i| i.key.clone()).collect::<Vec<_>>().join("\n"));
                ui.close();
            }
            return;
        }
        if sel.len() == 1 && first.is_dir && ui.add(theme::menu_item(icons::FOLDER_OPEN, "Open in system file manager")).clicked() {
            common::open_path(&first.path);
            ui.close();
        }
        if ui.add(theme::menu_item(icons::FOLDER_OPEN, "Show in system file manager")).clicked() {
            common::reveal(&first.path);
            ui.close();
        }
        ui.separator();
        if ui.add(theme::menu_item_key(icons::SCISSORS, "Cut", "Ctrl+X")).clicked() {
            self.copy_selection(true);
            ui.close();
        }
        if ui.add(theme::menu_item_key(icons::COPY, "Copy", "Ctrl+C")).clicked() {
            self.copy_selection(false);
            ui.close();
        }
        if sel.len() == 1 && first.is_dir && self.clip.is_some() && ui.add(theme::menu_item(icons::CLIPBOARD, "Paste into folder")).clicked() {
            self.paste_into(first.path.clone());
            ui.close();
        }
        if ui.add(theme::menu_item(icons::LINK, "Copy path")).clicked() {
            ui.ctx().copy_text(sel.iter().map(|i| i.path.display().to_string()).collect::<Vec<_>>().join("\n"));
            ui.close();
        }
        ui.separator();
        if sel.len() == 1 && ui.add(theme::menu_item_key(icons::PENCIL_SIMPLE, "Rename", "F2")).clicked() {
            self.start_rename();
            ui.close();
        }
        if sel.len() > 1 && ui.add(theme::menu_item_key(icons::TEXT_AA, &format!("Batch rename {} items…", sel.len()), "F2")).clicked() {
            self.open_rename_window();
            ui.close();
        }
        if ui.add(theme::menu_item_key(icons::TRASH, "Delete", "Del")).clicked() {
            self.ask_delete();
            ui.close();
        }
        ui.separator();
        self.convert_entries(ui, &sel);
        ui.separator();
        let archives: Vec<PathBuf> = sel.iter().filter(|i| i.is_archive).map(|i| i.path.clone()).collect();
        if !archives.is_empty() {
            if ui.add(theme::menu_item(icons::TRAY_ARROW_DOWN, "Extract here")).clicked() {
                let opts = ExtractOptions { into_subfolder: true, ..Default::default() };
                let n = archives.len();
                self.enqueue(archives.clone(), Operation::ExtractArchive { options: opts, inner: vec![] }, None);
                self.toast(format!("Extracting {n} archive(s)"));
                ui.close();
            }
            if ui.add(theme::menu_item(icons::FOLDER_OPEN, "Extract to…")).clicked() {
                if let Some(dest) = common::pick_folder("Extract to") {
                    self.enqueue(archives.clone(), Operation::ExtractArchive { options: ExtractOptions::default(), inner: vec![] }, Some(dest));
                }
                ui.close();
            }
            if ui.add(theme::menu_item(icons::SHIELD_CHECK, "Test integrity")).clicked() {
                let ids = self.enqueue(archives.clone(), Operation::VerifyArchive { password: None, recursive: true }, None);
                self.track_verify(ids);
                ui.close();
            }
        }
        if ui.add(theme::menu_item(icons::FILE_ZIP, "Compress to ZIP")).clicked() {
            let paths: Vec<PathBuf> = sel.iter().map(|i| i.path.clone()).collect();
            self.enqueue(paths, Operation::CreateArchive { options: Default::default() }, None);
            self.toast("Creating ZIP");
            ui.close();
        }
        if ui.add(theme::menu_item(icons::PACKAGE, "Compress…")).clicked() {
            self.open_tool(OpKind::Create);
            ui.close();
        }
        if ui.add(theme::menu_item(icons::FINGERPRINT, "Checksums")).clicked() {
            let paths: Vec<PathBuf> = sel.iter().filter(|i| !i.is_dir).map(|i| i.path.clone()).collect();
            if !paths.is_empty() {
                self.enqueue(
                    paths,
                    Operation::Checksum {
                        algos: vec![dropzone_core::checksum::Algo::Crc32, dropzone_core::checksum::Algo::Md5, dropzone_core::checksum::Algo::Sha256],
                    },
                    None,
                );
                self.queue_open = true;
            }
            ui.close();
        }
        if sel.len() == 1 && first.is_dir {
            ui.separator();
            let pinned = self.settings.pinned.contains(&first.path);
            if ui.add(theme::menu_item(icons::PUSH_PIN, if pinned { "Unpin from sidebar" } else { "Pin to sidebar" })).clicked() {
                if pinned {
                    self.settings.pinned.retain(|p| *p != first.path);
                } else {
                    self.settings.pinned.push(first.path.clone());
                }
                ui.close();
            }
        }
    }

    /// Conversion entries that apply to the selection.
    fn convert_entries(&mut self, ui: &mut egui::Ui, sel: &[Item]) {
        let has = |k: common::Kind| sel.iter().any(|i| i.kind == k);
        if has(common::Kind::Image) && ui.add(theme::menu_item(icons::IMAGE, "Convert / compress images…")).clicked() {
            self.open_tool(OpKind::Image);
            ui.close();
        }
        if (has(common::Kind::Video) || has(common::Kind::Audio)) && ui.add(theme::menu_item(icons::FILM_STRIP, "Convert audio / video…")).clicked() {
            self.open_tool(OpKind::Media);
            ui.close();
        }
        if has(common::Kind::Pdf) {
            if ui.add(theme::menu_item(icons::FILE_PDF, "Extract or split PDF pages…")).clicked() {
                self.open_tool(OpKind::PdfPages);
                ui.close();
            }
            if ui.add(theme::menu_item(icons::IMAGE, "Extract images from PDF…")).clicked() {
                self.open_tool(OpKind::PdfImages);
                ui.close();
            }
        }
        if has(common::Kind::ChecksumList) && ui.add(theme::menu_item(icons::LIST_CHECKS, "Verify checksum list")).clicked() {
            let paths = sel.iter().filter(|i| i.kind == common::Kind::ChecksumList).map(|i| i.path.clone()).collect();
            self.enqueue(paths, Operation::VerifyChecksums, None);
            self.queue_open = true;
            ui.close();
        }
    }

    pub fn tools_menu(&mut self, ui: &mut egui::Ui) {
        let in_folder = self.loc.folder().is_some();
        ui.add_enabled_ui(in_folder, |ui| {
            for (op, icon, label) in [
                (OpKind::Image, icons::IMAGE, "Convert / compress images"),
                (OpKind::Media, icons::FILM_STRIP, "Convert audio / video"),
                (OpKind::PdfPages, icons::FILE_PDF, "Extract or split PDF pages"),
                (OpKind::PdfImages, icons::IMAGE, "Extract images from PDFs"),
                (OpKind::Extract, icons::TRAY_ARROW_DOWN, "Extract archives"),
                (OpKind::Verify, icons::SHIELD_CHECK, "Test archives"),
                (OpKind::Create, icons::PACKAGE, "Create archive"),
                (OpKind::Checksum, icons::FINGERPRINT, "Checksums"),
                (OpKind::VerifyChecksums, icons::LIST_CHECKS, "Verify checksum list"),
            ] {
                if ui.add(theme::menu_item(icon, label)).clicked() {
                    self.open_tool(op);
                    ui.close();
                }
            }
            ui.separator();
            if ui.add(theme::menu_item(icons::TEXT_AA, "Batch rename")).clicked() {
                self.open_rename_window();
                ui.close();
            }
        });
        ui.separator();
        let mut hidden = self.settings.show_hidden;
        if ui.checkbox(&mut hidden, "Show hidden files").changed() {
            self.settings.show_hidden = hidden;
            self.visible_dirty = true;
        }
        if ui.add(theme::menu_item(icons::STACK, "Queue")).clicked() {
            self.queue_open = true;
            ui.close();
        }
    }

    fn background_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(dir) = self.loc.folder().map(|p| p.to_path_buf()) else {
            if ui.add(theme::menu_item(icons::TRAY_ARROW_DOWN, "Extract all…")).clicked() {
                self.extract_from_archive(None, None);
                ui.close();
            }
            return;
        };
        if ui.add(theme::menu_item_key(icons::FOLDER_PLUS, "New folder", "Ctrl+Shift+N")).clicked() {
            self.new_folder(&ctx);
            ui.close();
        }
        if ui.add_enabled(self.clip.is_some(), theme::menu_item_key(icons::CLIPBOARD, "Paste", "Ctrl+V")).clicked() {
            self.paste();
            ui.close();
        }
        if ui.add(theme::menu_item_key(icons::ARROW_CLOCKWISE, "Refresh", "F5")).clicked() {
            self.reload(&ctx);
            ui.close();
        }
        ui.separator();
        let mut hidden = self.settings.show_hidden;
        if ui.checkbox(&mut hidden, "Show hidden files").changed() {
            self.settings.show_hidden = hidden;
            self.visible_dirty = true;
        }
        if ui.add(theme::menu_item(icons::FOLDER_OPEN, "Open in system file manager")).clicked() {
            common::open_path(&dir);
            ui.close();
        }
        if ui.add(theme::menu_item(icons::LINK, "Copy folder path")).clicked() {
            ui.ctx().copy_text(dir.display().to_string());
            ui.close();
        }
    }

    // --- File operations ------------------------------------------------------------

    pub fn copy_selection(&mut self, cut: bool) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        self.clip = Some((paths, cut));
    }

    pub fn paste(&mut self) {
        if let Some(dir) = self.loc.folder().map(|p| p.to_path_buf()) {
            self.paste_into(dir);
        }
    }

    fn paste_into(&mut self, dest: PathBuf) {
        let Some((paths, cut)) = self.clip.clone() else { return };
        let n = paths.len();
        if cut {
            self.enqueue(paths, Operation::Move { dest }, None);
            self.clip = None;
            self.toast(format!("Moving {n} item(s)"));
        } else {
            self.enqueue(paths, Operation::Copy { dest }, None);
            self.toast(format!("Copying {n} item(s)"));
        }
    }

    pub fn new_folder(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.loc.folder() else { return };
        let path = dropzone_core::util::unique_path(&dir.join("New folder"));
        match std::fs::create_dir(&path) {
            Ok(()) => {
                let name = common::file_name(&path);
                self.reload(ctx);
                self.select_only(name.clone());
                self.renaming = Some(Renaming { key: name.clone(), text: name, focused: false });
                self.scroll_to_focus = true;
            }
            Err(e) => self.error(format!("Couldn't create folder: {e}")),
        }
    }

    pub fn start_rename(&mut self) {
        if self.loc.folder().is_none() {
            return;
        }
        let sel = self.selected_items();
        match sel.len() {
            0 => {}
            1 => {
                let key = sel[0].key.clone();
                self.renaming = Some(Renaming { text: key.clone(), key, focused: false });
            }
            _ => self.open_rename_window(),
        }
    }

    fn commit_rename(&mut self, ctx: &egui::Context, r: Renaming) {
        let new = r.text.trim().to_string();
        if new.is_empty() || new == r.key {
            return;
        }
        let Some(item) = self.item(&r.key).cloned() else { return };
        let target = item.path.with_file_name(&new);
        if target.exists() && !new.eq_ignore_ascii_case(&r.key) {
            self.error(format!("\"{new}\" already exists here"));
            return;
        }
        let plan =
            vec![dropzone_core::rename::RenameItem { from: item.path.clone(), to: target, status: dropzone_core::rename::PlanStatus::Ok, message: None }];
        match dropzone_core::rename::apply(&plan) {
            Ok(_) => {
                self.reload(ctx);
                self.select_only(new);
            }
            Err(e) => self.error(format!("Couldn't rename: {e:#}")),
        }
    }

    pub fn ask_delete(&mut self) {
        let paths = self.selected_paths();
        if !paths.is_empty() {
            self.confirm_delete = Some(paths);
        }
    }

    pub fn delete_prompt(&mut self, ctx: &egui::Context) {
        let Some(paths) = self.confirm_delete.clone() else { return };
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("confirm-delete")).show(ctx, |ui| {
            ui.set_width(360.0);
            let what = if paths.len() == 1 { format!("\"{}\"", common::file_name(&paths[0])) } else { format!("{} items", paths.len()) };
            ui.heading(format!("Move {what} to the Recycle Bin?"));
            ui.add_space(4.0);
            ui.label(RichText::new("You can restore it from there.").color(theme::TEXT_2));
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let del =
                        ui.add(egui::Button::new(RichText::new("Delete").font(theme::strong(13.5)).color(Color32::WHITE)).fill(Color32::from_rgb(220, 64, 64)));
                    if del.clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let p = paths.clone();
                        self.side_task = crate::common::Task::spawn(ctx, move || {
                            trash::delete_all(&p).map_err(|e| anyhow::anyhow!("{e}"))?;
                            Ok(format!("Moved {} item(s) to the Recycle Bin", p.len()))
                        });
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        });
        if close || modal.should_close() {
            self.confirm_delete = None;
        }
    }
}

fn paint_row_bg(ui: &egui::Ui, rect: egui::Rect, selected: bool, hovered: bool) {
    if selected {
        ui.painter().rect_filled(rect, theme::RADIUS, theme::accent_tint(if hovered { 56 } else { 44 }));
    } else if hovered {
        ui.painter().rect_filled(rect, theme::RADIUS, Color32::from_white_alpha(7));
    }
}

fn expand_home(s: &str) -> String {
    if let Some(rest) = s.strip_prefix('~')
        && let Some(home) = dirs::home_dir()
    {
        return format!("{}{rest}", home.display());
    }
    s.to_string()
}
