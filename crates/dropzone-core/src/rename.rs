//! Batch renaming: build a plan, show it, then apply it atomically-ish (two-phase, with
//! rollback) so swaps like a→b, b→a work and a failure halfway leaves nothing half-done.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaseChange {
    #[default]
    None,
    Lower,
    Upper,
    Title,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RenameRule {
    /// Template for the new name (without extension). Tokens:
    /// `{name}` original name, `{n}` / `{n:3}` counter (optionally zero-padded),
    /// `{parent}` containing folder, `{date}` modified date (YYYY-MM-DD),
    /// `{time}` modified time (HHMMSS), `{ext}` original extension.
    pub template: String,
    pub find: String,
    pub replace: String,
    pub use_regex: bool,
    pub case_sensitive: bool,
    pub case: CaseChange,
    pub extension_case: CaseChange,
    /// Replaces the extension when set (without the dot).
    pub new_extension: Option<String>,
    pub start: u64,
    pub step: u64,
}

impl Default for RenameRule {
    fn default() -> Self {
        Self {
            template: "{name}".into(),
            find: String::new(),
            replace: String::new(),
            use_regex: false,
            case_sensitive: false,
            case: CaseChange::None,
            extension_case: CaseChange::None,
            new_extension: None,
            start: 1,
            step: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanStatus {
    Ok,
    Unchanged,
    Conflict,
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameItem {
    pub from: PathBuf,
    pub to: PathBuf,
    pub status: PlanStatus,
    pub message: Option<String>,
}

pub fn plan(paths: &[PathBuf], rule: &RenameRule) -> Result<Vec<RenameItem>> {
    let finder = build_finder(rule)?;
    let token = Regex::new(r"\{(name|n|parent|date|time|ext)(?::(\d+))?\}").unwrap();
    let step = rule.step.max(1);

    let mut items: Vec<RenameItem> = paths
        .iter()
        .enumerate()
        .map(|(i, from)| {
            let counter = rule.start + i as u64 * step;
            let new_name = new_file_name(from, rule, &token, finder.as_ref(), counter);
            let to = from.with_file_name(&new_name);
            let (status, message) = match validate_name(&new_name) {
                Err(msg) => (PlanStatus::Invalid, Some(msg)),
                Ok(()) if to == *from => (PlanStatus::Unchanged, None),
                Ok(()) => (PlanStatus::Ok, None),
            };
            RenameItem { from: from.clone(), to, status, message }
        })
        .collect();

    // Targets are compared case-insensitively: Windows and macOS filesystems are.
    let key = |p: &Path| p.to_string_lossy().to_lowercase();
    let sources: HashSet<String> = items.iter().map(|i| key(&i.from)).collect();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for idx in 0..items.len() {
        if items[idx].status == PlanStatus::Invalid {
            continue;
        }
        let k = key(&items[idx].to);
        if let Some(&first) = seen.get(&k) {
            items[idx].status = PlanStatus::Conflict;
            items[idx].message = Some(format!("same new name as {}", file_name(&items[first].from)));
            continue;
        }
        seen.insert(k.clone(), idx);
        // A case-only rename hits the file itself; any other existing file is a real clash.
        let only_case_changed = key(&items[idx].from) == k;
        if items[idx].status == PlanStatus::Ok && items[idx].to.exists() && !sources.contains(&k) && !only_case_changed {
            items[idx].status = PlanStatus::Conflict;
            items[idx].message = Some("a file with this name already exists".into());
        }
    }
    Ok(items)
}

fn build_finder(rule: &RenameRule) -> Result<Option<Regex>> {
    if rule.find.is_empty() {
        return Ok(None);
    }
    let pattern = if rule.use_regex { rule.find.clone() } else { regex::escape(&rule.find) };
    let re = RegexBuilder::new(&pattern).case_insensitive(!rule.case_sensitive).build().context("invalid find pattern")?;
    Ok(Some(re))
}

fn new_file_name(from: &Path, rule: &RenameRule, token: &Regex, finder: Option<&Regex>, counter: u64) -> String {
    let stem = from.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = from.extension().map(|s| s.to_string_lossy().into_owned());
    let parent = from.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let modified: Option<chrono::DateTime<chrono::Local>> = std::fs::metadata(from).and_then(|m| m.modified()).ok().map(Into::into);

    let template = if rule.template.trim().is_empty() { "{name}" } else { &rule.template };
    let mut name = token
        .replace_all(template, |c: &regex::Captures| {
            let width: usize = c.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
            match &c[1] {
                "name" => stem.clone(),
                "n" => format!("{counter:0width$}"),
                "parent" => parent.clone(),
                "ext" => ext.clone().unwrap_or_default(),
                "date" => modified.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(),
                "time" => modified.map(|d| d.format("%H%M%S").to_string()).unwrap_or_default(),
                _ => String::new(),
            }
        })
        .into_owned();

    if let Some(re) = finder {
        name = re.replace_all(&name, rule.replace.as_str()).into_owned();
    }
    name = apply_case(&name, rule.case);

    let ext = rule
        .new_extension
        .as_deref()
        .map(|e| e.trim_start_matches('.').to_string())
        .filter(|e| !e.is_empty())
        .or(ext)
        .map(|e| apply_case(&e, rule.extension_case));
    match ext {
        Some(ext) => format!("{name}.{ext}"),
        None => name,
    }
}

fn apply_case(s: &str, case: CaseChange) -> String {
    match case {
        CaseChange::None => s.to_string(),
        CaseChange::Lower => s.to_lowercase(),
        CaseChange::Upper => s.to_uppercase(),
        CaseChange::Title => {
            let mut out = String::with_capacity(s.len());
            let mut start = true;
            for ch in s.chars() {
                if ch.is_alphanumeric() {
                    if start {
                        out.extend(ch.to_uppercase());
                    } else {
                        out.extend(ch.to_lowercase());
                    }
                    start = false;
                } else {
                    out.push(ch);
                    start = true;
                }
            }
            out
        }
    }
}

/// Rejects names Windows can't store, so a plan made on any OS is safe there too.
fn validate_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err("empty name".into());
    }
    if let Some(c) = name.chars().find(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control()) {
        return Err(format!("contains '{c}'"));
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Err("ends with a space or dot".into());
    }
    let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
        "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&base.as_str()) {
        return Err(format!("{base} is a reserved name on Windows"));
    }
    if name.len() > 255 {
        return Err("name longer than 255 characters".into());
    }
    Ok(())
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A rename that happened; feed the list back to [`undo`] to revert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Applied {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Applies every `Ok` item. Refuses to run if the plan has conflicts or invalid names.
pub fn apply(items: &[RenameItem]) -> Result<Vec<Applied>> {
    if let Some(bad) = items.iter().find(|i| matches!(i.status, PlanStatus::Conflict | PlanStatus::Invalid)) {
        bail!("plan has problems (e.g. {}); fix them before renaming", file_name(&bad.from));
    }
    let pairs: Vec<(PathBuf, PathBuf)> = items.iter().filter(|i| i.status == PlanStatus::Ok).map(|i| (i.from.clone(), i.to.clone())).collect();
    rename_two_phase(&pairs)
}

pub fn undo(applied: &[Applied]) -> Result<Vec<Applied>> {
    let pairs: Vec<(PathBuf, PathBuf)> = applied.iter().map(|a| (a.to.clone(), a.from.clone())).collect();
    rename_two_phase(&pairs)
}

fn rename_two_phase(pairs: &[(PathBuf, PathBuf)]) -> Result<Vec<Applied>> {
    let tag = format!(".dzrn-{}", std::process::id());
    // Phase 1: move every source to a temporary name in its own folder.
    let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    for (i, (from, to)) in pairs.iter().enumerate() {
        let tmp = from.with_file_name(format!("{}{tag}-{i}", file_name(from)));
        if let Err(e) = std::fs::rename(from, &tmp) {
            for (orig, _, tmp) in staged.iter().rev() {
                let _ = std::fs::rename(tmp, orig);
            }
            return Err(e).with_context(|| format!("renaming {}", from.display()));
        }
        staged.push((from.clone(), tmp, to.clone()));
    }
    // Phase 2: move temporaries to their final names.
    let mut done: Vec<Applied> = Vec::new();
    for (idx, (from, tmp, to)) in staged.iter().enumerate() {
        let clash = to.exists();
        let result =
            if clash { Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "target appeared while renaming")) } else { std::fs::rename(tmp, to) };
        if let Err(e) = result {
            for a in done.iter().rev() {
                let _ = std::fs::rename(&a.to, &a.from);
            }
            for (orig, tmp, _) in &staged[idx..] {
                let _ = std::fs::rename(tmp, orig);
            }
            return Err(e).with_context(|| format!("renaming {} to {}", from.display(), to.display()));
        }
        done.push(Applied { from: from.clone(), to: to.clone() });
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(dir: &Path, names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|n| {
                let p = dir.join(n);
                std::fs::write(&p, n).unwrap();
                p
            })
            .collect()
    }

    #[test]
    fn template_counter_and_case() {
        let dir = tempfile::tempdir().unwrap();
        let paths = files(dir.path(), &["IMG_001.JPG", "IMG_002.JPG"]);
        let rule = RenameRule { template: "holiday-{n:3}".into(), extension_case: CaseChange::Lower, ..Default::default() };
        let p = plan(&paths, &rule).unwrap();
        assert_eq!(file_name(&p[0].to), "holiday-001.jpg");
        assert_eq!(file_name(&p[1].to), "holiday-002.jpg");
        assert!(p.iter().all(|i| i.status == PlanStatus::Ok));
    }

    #[test]
    fn find_replace_regex() {
        let dir = tempfile::tempdir().unwrap();
        let paths = files(dir.path(), &["report 2023 final.txt"]);
        let rule = RenameRule { find: r"\s+".into(), replace: "_".into(), use_regex: true, case: CaseChange::Title, ..Default::default() };
        assert_eq!(file_name(&plan(&paths, &rule).unwrap()[0].to), "Report_2023_Final.txt");
    }

    #[test]
    fn detects_duplicates_and_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let paths = files(dir.path(), &["a.txt", "b.txt"]);
        files(dir.path(), &["taken.txt"]);
        let p = plan(&paths, &RenameRule { template: "same".into(), ..Default::default() }).unwrap();
        assert_eq!(p[1].status, PlanStatus::Conflict);
        let p = plan(&paths[..1], &RenameRule { template: "taken".into(), ..Default::default() }).unwrap();
        assert_eq!(p[0].status, PlanStatus::Conflict);
        let p = plan(&paths[..1], &RenameRule { template: "bad:name".into(), ..Default::default() }).unwrap();
        assert_eq!(p[0].status, PlanStatus::Invalid);
        assert!(apply(&p).is_err());
    }

    #[test]
    fn swap_and_undo() {
        let dir = tempfile::tempdir().unwrap();
        let paths = files(dir.path(), &["a.txt", "b.txt"]);
        let items = vec![
            RenameItem { from: paths[0].clone(), to: paths[1].clone(), status: PlanStatus::Ok, message: None },
            RenameItem { from: paths[1].clone(), to: paths[0].clone(), status: PlanStatus::Ok, message: None },
        ];
        let applied = apply(&items).unwrap();
        assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), "b.txt");
        assert_eq!(std::fs::read_to_string(&paths[1]).unwrap(), "a.txt");
        undo(&applied).unwrap();
        assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), "a.txt");
    }

    #[test]
    fn hundreds_of_files() {
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..500).map(|i| format!("file{i}.dat")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let paths = files(dir.path(), &refs);
        let p = plan(&paths, &RenameRule { template: "item_{n:4}".into(), ..Default::default() }).unwrap();
        let applied = apply(&p).unwrap();
        assert_eq!(applied.len(), 500);
        assert!(dir.path().join("item_0500.dat").exists());
    }
}
