//! File hashing and verification of checksum lists (`*.sha256`, `SHA256SUMS`, `*.md5`, ...).

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use md5::Digest;
use serde::{Deserialize, Serialize};

use crate::util::{Cancel, Progress};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Algo {
    Crc32,
    Md5,
    Sha1,
    Sha256,
}

impl Algo {
    pub fn all() -> [Algo; 4] {
        [Algo::Crc32, Algo::Md5, Algo::Sha1, Algo::Sha256]
    }
    /// Guesses the algorithm from a hex digest's length.
    fn from_hex_len(len: usize) -> Option<Algo> {
        match len {
            8 => Some(Algo::Crc32),
            32 => Some(Algo::Md5),
            40 => Some(Algo::Sha1),
            64 => Some(Algo::Sha256),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Hashes {
    pub crc32: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
}

impl Hashes {
    pub fn get(&self, algo: Algo) -> Option<&str> {
        match algo {
            Algo::Crc32 => self.crc32.as_deref(),
            Algo::Md5 => self.md5.as_deref(),
            Algo::Sha1 => self.sha1.as_deref(),
            Algo::Sha256 => self.sha256.as_deref(),
        }
    }
}

/// Hashes a stream with several algorithms in one pass.
pub fn hash_reader(mut r: impl Read, algos: &[Algo], total: Option<u64>, progress: &dyn Progress, cancel: &Cancel) -> Result<Hashes> {
    let mut crc = algos.contains(&Algo::Crc32).then(crc32fast::Hasher::new);
    let mut md5 = algos.contains(&Algo::Md5).then(md5::Md5::new);
    let mut sha1 = algos.contains(&Algo::Sha1).then(sha1::Sha1::new);
    let mut sha256 = algos.contains(&Algo::Sha256).then(sha2::Sha256::new);
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        cancel.check()?;
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        if let Some(h) = crc.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = md5.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = sha1.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = sha256.as_mut() {
            h.update(chunk);
        }
        done += n as u64;
        if let Some(t) = total.filter(|t| *t > 0) {
            progress.report(done as f32 / t as f32, "hashing");
        }
    }
    Ok(Hashes {
        crc32: crc.map(|h| format!("{:08x}", h.finalize())),
        md5: md5.map(|h| hex::encode(h.finalize())),
        sha1: sha1.map(|h| hex::encode(h.finalize())),
        sha256: sha256.map(|h| hex::encode(h.finalize())),
    })
}

pub fn hash_file(path: &Path, algos: &[Algo], progress: &dyn Progress, cancel: &Cancel) -> Result<Hashes> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let len = f.metadata()?.len();
    hash_reader(std::io::BufReader::new(f), algos, Some(len), progress, cancel)
}

pub fn is_checksum_list(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    ["sha256", "sha1", "md5", "sfv", "sha256sum", "md5sum", "sha1sum"].iter().any(|e| name.ends_with(&format!(".{e}"))) || name.contains("sums")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Mismatch,
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub file: PathBuf,
    pub algo: Algo,
    pub expected: String,
    pub actual: Option<String>,
    pub status: CheckStatus,
}

/// Parses one line of `sha256sum`, BSD (`SHA256 (file) = hash`) or SFV (`file CRC`) format.
fn parse_line(line: &str, sfv: bool) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
        return None;
    }
    if let Some((left, hash)) = line.rsplit_once(") = ") {
        let name = left.split_once(" (")?.1;
        return Some((name.to_string(), hash.trim().to_lowercase()));
    }
    if sfv {
        let (name, hash) = line.rsplit_once(char::is_whitespace)?;
        return Some((name.trim().to_string(), hash.trim().to_lowercase()));
    }
    let (hash, name) = line.split_once(char::is_whitespace)?;
    let name = name.trim_start().trim_start_matches('*');
    Some((name.to_string(), hash.to_lowercase()))
}

/// Verifies every entry of a checksum list against files next to it.
pub fn verify_list(list: &Path, progress: &dyn Progress, cancel: &Cancel) -> Result<Vec<CheckResult>> {
    let text = std::fs::read_to_string(list)?;
    let base = list.parent().unwrap_or(Path::new("."));
    let sfv = list.extension().is_some_and(|e| e.eq_ignore_ascii_case("sfv"));
    let entries: Vec<(String, String)> = text.lines().filter_map(|l| parse_line(l, sfv)).collect();
    let mut results = Vec::new();
    for (i, (name, expected)) in entries.iter().enumerate() {
        cancel.check()?;
        let Some(algo) = Algo::from_hex_len(expected.len()) else { continue };
        let file = base.join(name.replace('\\', "/"));
        let (actual, status) = if file.is_file() {
            let h = hash_file(&file, &[algo], &crate::util::NoProgress, cancel)?;
            let actual = h.get(algo).unwrap_or_default().to_string();
            let status = if actual == *expected { CheckStatus::Ok } else { CheckStatus::Mismatch };
            (Some(actual), status)
        } else {
            (None, CheckStatus::Missing)
        };
        results.push(CheckResult { file, algo, expected: expected.clone(), actual, status });
        progress.report((i + 1) as f32 / entries.len() as f32, name);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;

    #[test]
    fn known_digests() {
        let h = hash_reader(&b"hello"[..], &Algo::all(), None, &NoProgress, &Cancel::new()).unwrap();
        assert_eq!(h.crc32.unwrap(), "3610a686");
        assert_eq!(h.md5.unwrap(), "5d41402abc4b2a76b9719d911017c592");
        assert_eq!(h.sha1.unwrap(), "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
        assert_eq!(h.sha256.unwrap(), "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
    }

    #[test]
    fn verifies_lists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("b.txt"), "changed").unwrap();
        let list = dir.path().join("SHA256SUMS");
        std::fs::write(
            &list,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  a.txt\n\
             2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824 *b.txt\n\
             SHA256 (gone.txt) = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824\n",
        )
        .unwrap();
        let r = verify_list(&list, &NoProgress, &Cancel::new()).unwrap();
        let s: Vec<CheckStatus> = r.iter().map(|r| r.status).collect();
        assert_eq!(s, vec![CheckStatus::Ok, CheckStatus::Mismatch, CheckStatus::Missing]);
    }
}
