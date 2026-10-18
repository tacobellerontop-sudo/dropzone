use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};

/// Cooperative cancellation flag shared between a job and whoever wants to stop it.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            bail!("cancelled");
        }
        Ok(())
    }
}

/// Progress sink. `fraction` is 0.0..=1.0; `message` is a short human-readable step.
pub trait Progress: Send + Sync {
    fn report(&self, fraction: f32, message: &str);
}

impl<F: Fn(f32, &str) + Send + Sync> Progress for F {
    fn report(&self, fraction: f32, message: &str) {
        self(fraction, message)
    }
}

pub struct NoProgress;
impl Progress for NoProgress {
    fn report(&self, _: f32, _: &str) {}
}

/// Picks a path that doesn't exist yet by appending " (1)", " (2)", ... to the stem.
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or(Path::new(""));
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
    for i in 1.. {
        let name = match &ext {
            Some(ext) => format!("{stem} ({i}).{ext}"),
            None => format!("{stem} ({i})"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// Where outputs for `input` go: the chosen folder, or next to the input.
pub fn output_dir_for(input: &Path, output_dir: Option<&Path>) -> PathBuf {
    match output_dir {
        Some(dir) => dir.to_path_buf(),
        None => input.parent().map(Path::to_path_buf).unwrap_or_default(),
    }
}

/// `<dir>/<input stem><suffix>.<ext>`, made unique unless `overwrite` is set.
pub fn output_path(input: &Path, dir: &Path, suffix: &str, ext: &str, overwrite: bool) -> PathBuf {
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "output".into());
    let mut path = dir.join(format!("{stem}{suffix}.{ext}"));
    // Never clobber the input itself, even with overwrite on.
    if path == input {
        path = dir.join(format!("{stem}{suffix} (converted).{ext}"));
    }
    if overwrite { path } else { unique_path(&path) }
}

/// Joins an archive entry name onto `dest`, refusing anything that would escape it
/// (absolute paths, `..`, drive prefixes).
pub fn safe_join(dest: &Path, entry: &str) -> Option<PathBuf> {
    use std::path::Component;
    let mut out = dest.to_path_buf();
    let normalized = entry.replace('\\', "/");
    let mut depth = 0usize;
    for part in Path::new(&normalized).components() {
        match part {
            Component::Normal(p) => {
                out.push(p);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return None;
                }
                out.pop();
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if depth == 0 { None } else { Some(out) }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{size:.1} {}", UNITS[unit]) }
}

/// Expands folders into the files inside them (recursively), keeping plain files as-is.
pub fn expand_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            for entry in walkdir::WalkDir::new(p).sort_by_file_name().into_iter().flatten() {
                if entry.file_type().is_file() {
                    out.push(entry.into_path());
                }
            }
        } else {
            out.push(p.clone());
        }
    }
    out
}

/// Locates a helper executable (ffmpeg): next to our own executable first, where the
/// installer puts the bundled copy, then on PATH.
pub fn find_tool(name: &str) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    if let Ok(env) = std::env::var(format!("DROPZONE_{}", name.to_uppercase())) {
        let p = PathBuf::from(env);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for candidate in [dir.join(&exe_name), dir.join("binaries").join(&exe_name)] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(&exe_name)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_blocks_traversal() {
        let d = Path::new("/out");
        assert_eq!(safe_join(d, "a/b.txt"), Some(PathBuf::from("/out/a/b.txt")));
        assert_eq!(safe_join(d, "a/../b.txt"), Some(PathBuf::from("/out/b.txt")));
        assert_eq!(safe_join(d, "../evil"), None);
        assert_eq!(safe_join(d, "/etc/passwd"), None);
        assert_eq!(safe_join(d, "a\\..\\..\\x"), None);
        assert_eq!(safe_join(d, ""), None);
    }

    #[test]
    fn unique_path_appends_counter() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        assert_eq!(unique_path(&p), p);
        std::fs::write(&p, "x").unwrap();
        assert_eq!(unique_path(&p), dir.path().join("a (1).txt"));
    }
}
