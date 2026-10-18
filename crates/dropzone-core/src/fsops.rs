//! Copying and moving files and folders, with progress, for the explorer's paste and drop.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::util::{self, Cancel, Progress};

fn tree_size(p: &Path) -> u64 {
    if p.is_dir() {
        walkdir::WalkDir::new(p).into_iter().flatten().filter(|e| e.file_type().is_file()).filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()
    } else {
        std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
    }
}

/// Where `src` lands inside `dest_dir`, renamed "name (1)" if that name is taken.
fn target_for(src: &Path, dest_dir: &Path) -> Result<PathBuf> {
    let name = src.file_name().with_context(|| format!("{} has no file name", src.display()))?;
    Ok(util::unique_path(&dest_dir.join(name)))
}

fn check_not_into_itself(src: &Path, dest_dir: &Path) -> Result<()> {
    if let (Ok(s), Ok(d)) = (src.canonicalize(), dest_dir.canonicalize())
        && s.is_dir()
        && d.starts_with(&s)
    {
        bail!("can't put {} inside itself", src.display());
    }
    Ok(())
}

struct Counter<'a> {
    done: u64,
    total: u64,
    progress: &'a dyn Progress,
    cancel: &'a Cancel,
}

impl Counter<'_> {
    fn copy_file(&mut self, from: &Path, to: &Path) -> Result<()> {
        self.cancel.check()?;
        let mut src = std::fs::File::open(from).with_context(|| format!("reading {}", from.display()))?;
        let mut dst = std::fs::File::create(to).with_context(|| format!("writing {}", to.display()))?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            if self.cancel.is_cancelled() {
                drop(dst);
                let _ = std::fs::remove_file(to);
                bail!("cancelled");
            }
            let n = std::io::Read::read(&mut src, &mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut dst, &buf[..n])?;
            self.done += n as u64;
            if self.total > 0 {
                self.progress.report(self.done as f32 / self.total as f32, &from.file_name().unwrap_or_default().to_string_lossy());
            }
        }
        if let Ok(m) = std::fs::metadata(from)
            && let Ok(t) = m.modified()
        {
            let _ = dst.set_modified(t);
        }
        Ok(())
    }

    fn copy_tree(&mut self, from: &Path, to: &Path) -> Result<()> {
        if from.is_dir() {
            std::fs::create_dir_all(to)?;
            for entry in std::fs::read_dir(from)? {
                let entry = entry?;
                self.copy_tree(&entry.path(), &to.join(entry.file_name()))?;
            }
            Ok(())
        } else {
            self.copy_file(from, to)
        }
    }
}

pub fn copy_items(sources: &[PathBuf], dest_dir: &Path, progress: &dyn Progress, cancel: &Cancel) -> Result<Vec<PathBuf>> {
    let total = sources.iter().map(|s| tree_size(s)).sum();
    let mut c = Counter { done: 0, total, progress, cancel };
    let mut out = Vec::new();
    for src in sources {
        check_not_into_itself(src, dest_dir)?;
        let target = target_for(src, dest_dir)?;
        if let Err(e) = c.copy_tree(src, &target) {
            // Don't leave a half-copied item behind.
            let _ = if target.is_dir() { std::fs::remove_dir_all(&target) } else { std::fs::remove_file(&target) };
            return Err(e);
        }
        out.push(target);
    }
    progress.report(1.0, "done");
    Ok(out)
}

/// Moves items; a rename when on the same drive, copy-then-delete otherwise.
pub fn move_items(sources: &[PathBuf], dest_dir: &Path, progress: &dyn Progress, cancel: &Cancel) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for (i, src) in sources.iter().enumerate() {
        cancel.check()?;
        if src.parent() == Some(dest_dir) {
            out.push(src.clone()); // already there
            continue;
        }
        check_not_into_itself(src, dest_dir)?;
        let target = target_for(src, dest_dir)?;
        if std::fs::rename(src, &target).is_err() {
            let copied = copy_items(std::slice::from_ref(src), dest_dir, &crate::util::NoProgress, cancel)?;
            if src.is_dir() {
                std::fs::remove_dir_all(src)?;
            } else {
                std::fs::remove_file(src)?;
            }
            out.extend(copied);
        } else {
            out.push(target);
        }
        progress.report((i + 1) as f32 / sources.len() as f32, &src.file_name().unwrap_or_default().to_string_lossy());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;

    #[test]
    fn copy_and_move() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/a.txt"), "a").unwrap();
        std::fs::write(d.path().join("f.txt"), "f").unwrap();
        let dest = d.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let out = copy_items(&[src.clone(), d.path().join("f.txt")], &dest, &NoProgress, &Cancel::new()).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("src/sub/a.txt")).unwrap(), "a");
        assert_eq!(out.len(), 2);
        // Copying again keeps both.
        let out = copy_items(&[d.path().join("f.txt")], &dest, &NoProgress, &Cancel::new()).unwrap();
        assert_eq!(out[0], dest.join("f (1).txt"));
        // Into itself is refused.
        assert!(copy_items(std::slice::from_ref(&src), &src.join("sub"), &NoProgress, &Cancel::new()).is_err());

        let moved = move_items(&[d.path().join("f.txt")], &dest.join("src"), &NoProgress, &Cancel::new()).unwrap();
        assert!(moved[0].exists() && !d.path().join("f.txt").exists());
    }
}
