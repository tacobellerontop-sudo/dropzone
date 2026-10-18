//! Where the explorer is (a folder, or a folder inside an archive), what's there, and
//! moving around: history, loading, sorting and filtering.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use dropzone_core::archive::{self, ArchiveRef, Listing};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::common::{Kind, Task};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Folder(PathBuf),
    /// A folder inside an archive (possibly nested in other archives).
    Archive {
        archive: ArchiveRef,
        dir: String,
    },
}

impl Location {
    pub fn folder(&self) -> Option<&Path> {
        match self {
            Location::Folder(p) => Some(p),
            Location::Archive { .. } => None,
        }
    }

    pub fn parent(&self) -> Option<Location> {
        match self {
            Location::Folder(p) => p.parent().map(|p| Location::Folder(p.to_path_buf())),
            Location::Archive { archive, dir } if !dir.is_empty() => {
                Some(Location::Archive { archive: archive.clone(), dir: dir.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default() })
            }
            Location::Archive { archive, .. } => match archive.inner.split_last() {
                Some((last, rest)) => Some(Location::Archive {
                    archive: ArchiveRef { path: archive.path.clone(), inner: rest.to_vec() },
                    dir: last.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default(),
                }),
                None => archive.path.parent().map(|p| Location::Folder(p.to_path_buf())),
            },
        }
    }

    /// Breadcrumb segments, each with the location it leads to.
    pub fn crumbs(&self) -> Vec<(String, Location)> {
        let mut out = Vec::new();
        let folder_crumbs = |p: &Path, out: &mut Vec<(String, Location)>| {
            let mut acc = PathBuf::new();
            for c in p.components() {
                acc.push(c);
                let label = match c {
                    std::path::Component::RootDir if out.is_empty() => "/".to_string(),
                    std::path::Component::RootDir => continue,
                    std::path::Component::Prefix(p) => p.as_os_str().to_string_lossy().into_owned(),
                    other => other.as_os_str().to_string_lossy().into_owned(),
                };
                out.push((label, Location::Folder(acc.clone())));
            }
        };
        match self {
            Location::Folder(p) => folder_crumbs(p, &mut out),
            Location::Archive { archive, dir } => {
                if let Some(parent) = archive.path.parent() {
                    folder_crumbs(parent, &mut out);
                }
                let root = ArchiveRef::file(&archive.path);
                out.push((crate::common::file_name(&archive.path), Location::Archive { archive: root, dir: String::new() }));
                for i in 0..archive.inner.len() {
                    let inner = &archive.inner[i];
                    let ar = ArchiveRef { path: archive.path.clone(), inner: archive.inner[..=i].to_vec() };
                    out.push((inner.rsplit('/').next().unwrap_or(inner).to_string(), Location::Archive { archive: ar, dir: String::new() }));
                }
                let mut acc = String::new();
                for seg in dir.split('/').filter(|s| !s.is_empty()) {
                    acc = if acc.is_empty() { seg.to_string() } else { format!("{acc}/{seg}") };
                    out.push((seg.to_string(), Location::Archive { archive: archive.clone(), dir: acc.clone() }));
                }
            }
        }
        out
    }

    pub fn title(&self) -> String {
        self.crumbs().last().map(|c| c.0.clone()).unwrap_or_else(|| "Dropzone".into())
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    /// Unique within the location: the file name, or the entry path inside an archive.
    pub key: String,
    pub name: String,
    /// Full path on disk (for archive entries: the archive file).
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub packed: Option<u64>,
    pub modified: Option<NaiveDateTime>,
    pub kind: Kind,
    pub hidden: bool,
    pub is_archive: bool,
    pub encrypted: bool,
}

impl Item {
    pub fn type_label(&self) -> String {
        if self.is_dir {
            return "Folder".into();
        }
        match self.name.rsplit_once('.') {
            Some((_, ext)) if !ext.is_empty() => format!("{} file", ext.to_uppercase()),
            _ => "File".into(),
        }
    }
}

pub enum Loaded {
    Folder(PathBuf, Vec<Item>),
    Archive(ArchiveRef, Listing),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortKey {
    #[default]
    Name,
    Modified,
    Type,
    Size,
}

fn list_folder(dir: &Path) -> anyhow::Result<Vec<Item>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        // Follow symlinks so linked folders behave like folders.
        let Ok(md) = std::fs::metadata(&path).or_else(|_| entry.metadata()) else { continue };
        let is_dir = md.is_dir();
        let modified = md.modified().ok().map(|t| chrono::DateTime::<chrono::Local>::from(t).naive_local());
        out.push(Item {
            key: name.clone(),
            kind: Kind::of_name(&name, is_dir),
            is_archive: !is_dir && archive::is_archive_name(&name),
            hidden: is_hidden(&name, &md),
            name,
            path,
            is_dir,
            size: (!is_dir).then_some(md.len()),
            packed: None,
            modified,
            encrypted: false,
        });
    }
    Ok(out)
}

fn is_hidden(name: &str, md: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const HIDDEN: u32 = 0x2;
        const SYSTEM: u32 = 0x4;
        if md.file_attributes() & (HIDDEN | SYSTEM) != 0 {
            return true;
        }
    }
    let _ = md;
    name.starts_with('.')
}

/// The contents of `dir` inside an archive, with folders implied by entry paths.
pub fn archive_items(archive: &ArchiveRef, listing: &Listing, dir: &str) -> Vec<Item> {
    let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    let mut folders: BTreeMap<String, Item> = BTreeMap::new();
    let mut files = Vec::new();
    let parse = |m: &Option<String>| m.as_deref().and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").ok());
    for e in &listing.entries {
        let Some(rest) = e.path.strip_prefix(&prefix) else { continue };
        if rest.is_empty() {
            continue;
        }
        let (first, nested) = match rest.split_once('/') {
            Some((f, _)) => (f, true),
            None => (rest, false),
        };
        if nested || e.is_dir {
            let f = folders.entry(first.to_string()).or_insert_with(|| Item {
                key: format!("{prefix}{first}"),
                name: first.to_string(),
                path: archive.path.clone(),
                is_dir: true,
                size: Some(0),
                packed: None,
                modified: None,
                kind: Kind::Folder,
                hidden: false,
                is_archive: false,
                encrypted: false,
            });
            if nested && !e.is_dir {
                *f.size.get_or_insert(0) += e.size;
            } else if !nested {
                f.modified = parse(&e.modified);
            }
        } else {
            files.push(Item {
                key: e.path.clone(),
                name: first.to_string(),
                path: archive.path.clone(),
                is_dir: false,
                size: Some(e.size),
                packed: e.compressed_size,
                modified: parse(&e.modified),
                kind: Kind::of_name(first, false),
                hidden: false,
                is_archive: e.is_archive,
                encrypted: e.encrypted,
            });
        }
    }
    let mut out: Vec<Item> = folders.into_values().collect();
    out.extend(files);
    out
}

/// Natural ordering so "file2" sorts before "file10".
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    bi.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

impl App {
    pub fn navigate(&mut self, ctx: &eframe::egui::Context, loc: Location) {
        if loc != self.loc {
            let prev = std::mem::replace(&mut self.loc, loc);
            self.back.push(prev);
            self.fwd.clear();
        }
        self.after_move(ctx);
    }

    pub fn go_back(&mut self, ctx: &eframe::egui::Context) {
        if let Some(prev) = self.back.pop() {
            let cur = std::mem::replace(&mut self.loc, prev);
            self.fwd.push(cur);
            self.after_move(ctx);
        }
    }

    pub fn go_forward(&mut self, ctx: &eframe::egui::Context) {
        if let Some(next) = self.fwd.pop() {
            let cur = std::mem::replace(&mut self.loc, next);
            self.back.push(cur);
            self.after_move(ctx);
        }
    }

    pub fn go_up(&mut self, ctx: &eframe::egui::Context) {
        let came_from = self.current_key_in_parent();
        if let Some(p) = self.loc.parent() {
            self.navigate(ctx, p);
            if let Some(k) = came_from {
                self.select_only(k);
                self.scroll_to_focus = true;
            }
        }
    }

    /// The key the current location has in its parent's listing, to keep it highlighted after "up".
    fn current_key_in_parent(&self) -> Option<String> {
        match &self.loc {
            Location::Folder(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()),
            Location::Archive { archive, dir } if dir.is_empty() => match archive.inner.last() {
                Some(inner) => Some(inner.clone()),
                None => archive.path.file_name().map(|n| n.to_string_lossy().into_owned()),
            },
            Location::Archive { dir, .. } => Some(dir.clone()),
        }
    }

    fn after_move(&mut self, ctx: &eframe::egui::Context) {
        self.selection.clear();
        self.anchor = None;
        self.focus = None;
        self.filter.clear();
        self.renaming = None;
        self.path_edit = None;
        if let Location::Folder(p) = &self.loc {
            self.settings.last_folder = Some(p.clone());
        }
        self.reload(ctx);
    }

    pub fn reload(&mut self, ctx: &eframe::egui::Context) {
        self.load_error = None;
        match self.loc.clone() {
            Location::Folder(p) => {
                self.load = Task::spawn(ctx, move || list_folder(&p).map(|items| Loaded::Folder(p, items)));
            }
            Location::Archive { archive, dir } => {
                // Moving between folders of the same archive reuses its listing.
                if let Some((a, l)) = &self.archive_listing
                    && *a == archive
                {
                    self.items = archive_items(a, l, &dir);
                    self.visible_dirty = true;
                    return;
                }
                if self.archive_listing.as_ref().is_none_or(|(a, _)| a.path != archive.path) {
                    self.archive_password.clear();
                }
                self.needs_password = false;
                let pw = Some(self.archive_password.clone()).filter(|p| !p.is_empty());
                self.load = Task::spawn(ctx, move || archive::list(&archive, pw.as_deref()).map(|l| Loaded::Archive(archive, l)));
            }
        }
    }

    pub fn poll_load(&mut self) {
        let Some(result) = self.load.poll() else { return };
        match result {
            Ok(Loaded::Folder(p, items)) => {
                if self.loc == Location::Folder(p) {
                    self.items = items;
                    // Keep selection for items that still exist (refresh after paste etc.).
                    let keys: std::collections::HashSet<&str> = self.items.iter().map(|i| i.key.as_str()).collect();
                    self.selection.retain(|k| keys.contains(k.as_str()));
                }
            }
            Ok(Loaded::Archive(a, l)) => {
                self.needs_password = l.encrypted && self.archive_password.is_empty();
                if let Location::Archive { archive, dir } = &self.loc
                    && *archive == a
                {
                    self.items = archive_items(&a, &l, dir);
                }
                self.archive_listing = Some((a, l));
            }
            Err(e) => {
                self.needs_password = matches!(self.loc, Location::Archive { .. }) && (e.to_lowercase().contains("password") || e.contains("encrypted"));
                self.items.clear();
                self.load_error = Some(e);
            }
        }
        self.visible_dirty = true;
    }

    /// Recomputes the filtered, sorted view when something it depends on changed.
    pub fn refresh_visible(&mut self) {
        if !self.visible_dirty {
            return;
        }
        self.visible_dirty = false;
        let filter = self.filter.trim().to_lowercase();
        let show_hidden = self.settings.show_hidden;
        let mut idx: Vec<usize> = (0..self.items.len())
            .filter(|&i| {
                let it = &self.items[i];
                (show_hidden || !it.hidden) && (filter.is_empty() || it.name.to_lowercase().contains(&filter))
            })
            .collect();
        let items = &self.items;
        let (key, asc) = (self.settings.sort_key, self.settings.sort_asc);
        idx.sort_by(|&a, &b| {
            let (x, y) = (&items[a], &items[b]);
            // Folders always first, like every file manager.
            let dirs = y.is_dir.cmp(&x.is_dir);
            let ord = match key {
                SortKey::Name => natural_cmp(&x.name, &y.name),
                SortKey::Modified => x.modified.cmp(&y.modified),
                SortKey::Type => x.type_label().cmp(&y.type_label()).then_with(|| natural_cmp(&x.name, &y.name)),
                SortKey::Size => x.size.cmp(&y.size),
            };
            dirs.then(if asc { ord } else { ord.reverse() })
        });
        self.visible = idx;
    }

    pub fn item(&self, key: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.key == key)
    }

    pub fn selected_items(&self) -> Vec<&Item> {
        self.visible.iter().map(|&i| &self.items[i]).filter(|it| self.selection.contains(&it.key)).collect()
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        if self.loc.folder().is_none() {
            return Vec::new();
        }
        self.selected_items().iter().map(|i| i.path.clone()).collect()
    }

    pub fn select_only(&mut self, key: String) {
        self.selection.clear();
        self.selection.insert(key.clone());
        self.anchor = Some(key.clone());
        self.focus = Some(key);
    }

    /// Opens an item: folders and archives are entered, files open in their default app.
    pub fn open_item(&mut self, ctx: &eframe::egui::Context, key: &str) {
        let Some(item) = self.item(key).cloned() else { return };
        match self.loc.clone() {
            Location::Folder(_) => {
                if item.is_dir {
                    self.navigate(ctx, Location::Folder(item.path));
                } else if item.is_archive {
                    self.navigate(ctx, Location::Archive { archive: ArchiveRef::file(&item.path), dir: String::new() });
                } else {
                    crate::common::open_path(&item.path);
                }
            }
            Location::Archive { archive, .. } => {
                if item.is_dir {
                    self.navigate(ctx, Location::Archive { archive, dir: item.key });
                } else if item.is_archive {
                    let mut inner = archive.inner.clone();
                    inner.push(item.key);
                    self.navigate(ctx, Location::Archive { archive: ArchiveRef { path: archive.path, inner }, dir: String::new() });
                } else {
                    self.open_archive_entry(ctx, archive, item.key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("file2", "file10"), Less);
        assert_eq!(natural_cmp("File10", "file9"), Greater);
        assert_eq!(natural_cmp("a", "B"), Less);
        assert_eq!(natural_cmp("x01", "x1"), Equal);
    }

    #[test]
    fn parents_walk_out_of_nested_archives() {
        let a = ArchiveRef { path: PathBuf::from("/d/outer.zip"), inner: vec!["x/inner.7z".into()] };
        let loc = Location::Archive { archive: a, dir: "sub".into() };
        let p1 = loc.parent().unwrap();
        assert!(matches!(&p1, Location::Archive { dir, archive } if dir.is_empty() && archive.inner.len() == 1));
        let p2 = p1.parent().unwrap();
        assert!(matches!(&p2, Location::Archive { dir, archive } if dir == "x" && archive.inner.is_empty()));
        let p3 = p2.parent().unwrap().parent().unwrap();
        assert_eq!(p3, Location::Folder(PathBuf::from("/d")));
        let crumbs = loc.crumbs();
        assert_eq!(crumbs.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), vec!["/", "d", "outer.zip", "inner.7z", "sub"]);
    }
}
