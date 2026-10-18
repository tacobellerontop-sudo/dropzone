//! Archive inspection without extracting everything: list, preview single entries,
//! extract a selection, compare with a folder, drill into nested archives, verify
//! integrity, and create ZIP/7z archives.
//!
//! An archive inside another archive is addressed by [`ArchiveRef`]: the file on disk
//! plus the chain of entry names leading to the inner archive.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::image_ops;
use crate::util::{self, Cancel, Progress};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveKind {
    Zip,
    SevenZ,
    Tar,
    TarGz,
    TarXz,
    TarBz2,
    Gz,
    Xz,
    Bz2,
}

impl ArchiveKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Zip => "ZIP",
            Self::SevenZ => "7z",
            Self::Tar => "TAR",
            Self::TarGz => "TAR.GZ",
            Self::TarXz => "TAR.XZ",
            Self::TarBz2 => "TAR.BZ2",
            Self::Gz => "GZip",
            Self::Xz => "XZ",
            Self::Bz2 => "BZip2",
        }
    }
    /// Whether the format stores a CRC per entry we can compare without decompressing.
    fn has_stored_crc(self) -> bool {
        matches!(self, Self::Zip | Self::SevenZ)
    }
}

/// File extensions treated as archives we can open (ZIP-based formats included).
const ARCHIVE_EXTENSIONS: &[&str] = &[
    "zip", "7z", "tar", "tgz", "gz", "txz", "xz", "tbz", "tbz2", "bz2", "jar", "war", "apk", "epub", "nupkg", "whl", "vsix", "xpi", "cbz", "docx", "xlsx",
    "pptx", "odt", "ods", "odp",
];

pub fn is_archive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.rsplit_once('.').is_some_and(|(_, ext)| ARCHIVE_EXTENSIONS.contains(&ext))
}

pub fn is_archive(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(is_archive_name)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveRef {
    pub path: PathBuf,
    /// Entry names of nested archives, outermost first. Empty for the file itself.
    #[serde(default)]
    pub inner: Vec<String>,
}

impl ArchiveRef {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), inner: Vec::new() }
    }
    fn display_name(&self) -> String {
        match self.inner.last() {
            Some(inner) => inner.rsplit('/').next().unwrap_or(inner).to_string(),
            None => self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Path inside the archive, '/'-separated, without a trailing slash.
    pub path: String,
    pub size: u64,
    pub compressed_size: Option<u64>,
    pub crc32: Option<u32>,
    pub is_dir: bool,
    pub modified: Option<String>,
    pub encrypted: bool,
    /// Looks like an archive itself, so it can be opened in place.
    pub is_archive: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    pub kind: ArchiveKind,
    pub entries: Vec<Entry>,
    pub file_count: u64,
    pub total_size: u64,
    pub total_compressed: Option<u64>,
    pub encrypted: bool,
    pub solid: bool,
}

// ---------------------------------------------------------------------------------------
// Sources: a file on disk, or an inner archive pulled into memory / a temp file.

enum Source {
    File(BufReader<File>),
    Mem(Cursor<Vec<u8>>),
}

impl Read for Source {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Source::File(f) => f.read(buf),
            Source::Mem(m) => m.read(buf),
        }
    }
}

impl Seek for Source {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Source::File(f) => f.seek(pos),
            Source::Mem(m) => m.seek(pos),
        }
    }
}

/// Nested archives at most this big are held in memory; larger ones spill to a temp file.
const MEM_LIMIT: u64 = 256 * 1024 * 1024;

struct Opened {
    kind: ArchiveKind,
    src: Source,
    name: String,
}

fn open_file(path: &Path) -> Result<Source> {
    let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    Ok(Source::File(BufReader::with_capacity(256 * 1024, f)))
}

fn source_from_reader(r: &mut dyn Read, size_hint: u64) -> Result<Source> {
    if size_hint <= MEM_LIMIT {
        let mut buf = Vec::with_capacity(size_hint as usize);
        r.read_to_end(&mut buf)?;
        Ok(Source::Mem(Cursor::new(buf)))
    } else {
        let mut f = tempfile::tempfile()?;
        io::copy(r, &mut f)?;
        f.seek(SeekFrom::Start(0))?;
        Ok(Source::File(BufReader::new(f)))
    }
}

fn is_tar_header(block: &[u8]) -> bool {
    block.len() >= 262 && &block[257..262] == b"ustar"
}

fn read_prefix(r: &mut dyn Read, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    let mut filled = 0;
    while filled < n {
        match r.read(&mut buf[filled..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => filled += k,
        }
    }
    buf.truncate(filled);
    buf
}

fn detect(src: &mut Source, name: &str) -> Result<ArchiveKind> {
    src.seek(SeekFrom::Start(0))?;
    let head = read_prefix(src, 512);
    src.seek(SeekFrom::Start(0))?;
    let lower = name.to_ascii_lowercase();
    let tar_by_name = lower.contains(".tar.") || lower.ends_with(".tgz") || lower.ends_with(".txz") || lower.ends_with(".tbz2") || lower.ends_with(".tbz");

    let kind = if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") || head.starts_with(b"PK\x07\x08") {
        ArchiveKind::Zip
    } else if head.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        ArchiveKind::SevenZ
    } else if head.starts_with(&[0x1F, 0x8B]) {
        let inner = read_prefix(&mut flate2::read::MultiGzDecoder::new(&head[..]), 512);
        if is_tar_header(&inner) || tar_by_name { ArchiveKind::TarGz } else { ArchiveKind::Gz }
    } else if head.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        // The first XZ block may need more than 512 bytes; decode a little from the real stream.
        let inner = read_prefix(&mut lzma_rust2::XzReader::new(&mut *src, true), 512);
        src.seek(SeekFrom::Start(0))?;
        if is_tar_header(&inner) || tar_by_name { ArchiveKind::TarXz } else { ArchiveKind::Xz }
    } else if head.starts_with(b"BZh") {
        let inner = read_prefix(&mut bzip2::read::MultiBzDecoder::new(&mut *src), 512);
        src.seek(SeekFrom::Start(0))?;
        if is_tar_header(&inner) || tar_by_name { ArchiveKind::TarBz2 } else { ArchiveKind::Bz2 }
    } else if is_tar_header(&head) || (lower.ends_with(".tar") && head.len() == 512) {
        ArchiveKind::Tar
    } else {
        bail!("{name} is not a supported archive (ZIP, 7z, TAR, GZ, XZ, BZ2)");
    };
    Ok(kind)
}

fn open(r: &ArchiveRef, password: Option<&str>) -> Result<Opened> {
    let mut name = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut src = open_file(&r.path)?;
    let mut kind = detect(&mut src, &name)?;
    for inner in &r.inner {
        let mut opened = Opened { kind, src, name: name.clone() };
        let mut found: Option<Source> = None;
        walk(&mut opened, password, &|p, _| p == inner, &mut |e, rd| {
            found = Some(source_from_reader(rd, e.size)?);
            Ok(false)
        })?;
        src = found.ok_or_else(|| anyhow!("{inner} not found in {name}"))?;
        name = inner.clone();
        kind = detect(&mut src, &name)?;
    }
    Ok(Opened { kind, src, name })
}

// ---------------------------------------------------------------------------------------
// Metadata helpers.

fn fmt_time(t: chrono::DateTime<chrono::Local>) -> String {
    t.format("%Y-%m-%d %H:%M").to_string()
}

fn zip_entry<R: Read>(f: &zip::read::ZipFile<'_, R>) -> Entry {
    let path = f.name().map(|n| n.into_owned()).unwrap_or_else(|_| String::from_utf8_lossy(f.name_raw()).into_owned());
    let path = path.trim_end_matches('/').to_string();
    let modified = f.last_modified().map(|d| format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year(), d.month(), d.day(), d.hour(), d.minute()));
    Entry {
        is_archive: !f.is_dir() && is_archive_name(&path),
        path,
        size: f.size(),
        compressed_size: Some(f.compressed_size()),
        crc32: Some(f.crc32()),
        is_dir: f.is_dir(),
        modified,
        encrypted: f.encrypted(),
    }
}

fn sevenz_entry(e: &sevenz_rust2::ArchiveEntry) -> Entry {
    let path = e.name().replace('\\', "/").trim_end_matches('/').to_string();
    let modified = e.has_last_modified_date.then(|| {
        let st: std::time::SystemTime = e.last_modified_date().into();
        fmt_time(st.into())
    });
    Entry {
        is_archive: !e.is_directory() && is_archive_name(&path),
        path,
        size: e.size(),
        compressed_size: (e.compressed_size > 0).then_some(e.compressed_size),
        crc32: e.has_crc.then_some(e.crc as u32),
        is_dir: e.is_directory(),
        modified,
        encrypted: false,
    }
}

fn tar_entry<R: Read>(e: &tar::Entry<'_, R>) -> Result<Option<Entry>> {
    let h = e.header();
    let et = h.entry_type();
    if !(et.is_file() || et.is_dir() || et == tar::EntryType::Continuous || et.is_gnu_sparse()) {
        return Ok(None); // links, devices, pax headers
    }
    let path = e.path()?.to_string_lossy().replace('\\', "/");
    let path = path.trim_start_matches("./").trim_end_matches('/').to_string();
    if path.is_empty() {
        return Ok(None);
    }
    let modified = h.mtime().ok().and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0)).map(|t| fmt_time(t.into()));
    Ok(Some(Entry {
        is_archive: !et.is_dir() && is_archive_name(&path),
        path,
        size: e.size(),
        compressed_size: None,
        crc32: None,
        is_dir: et.is_dir(),
        modified,
        encrypted: false,
    }))
}

fn single_stream_entry(name: &str, size: u64) -> Entry {
    let lower = name.to_ascii_lowercase();
    let stem = ["gz", "xz", "bz2"]
        .iter()
        .find_map(|e| lower.strip_suffix(&format!(".{e}")).map(|s| name[..s.len()].to_string()))
        .unwrap_or_else(|| format!("{name}.out"));
    let stem = stem.rsplit(['/', '\\']).next().unwrap_or(&stem).to_string();
    Entry { is_archive: is_archive_name(&stem), path: stem, size, compressed_size: None, crc32: None, is_dir: false, modified: None, encrypted: false }
}

/// gzip stores the uncompressed size mod 2^32 in its last four bytes.
fn gzip_isize(src: &mut Source) -> u64 {
    let size = (|| -> io::Result<u64> {
        src.seek(SeekFrom::End(-4))?;
        let mut b = [0u8; 4];
        src.read_exact(&mut b)?;
        Ok(u32::from_le_bytes(b) as u64)
    })()
    .unwrap_or(0);
    let _ = src.seek(SeekFrom::Start(0));
    size
}

fn decompressor<'a>(kind: ArchiveKind, src: &'a mut Source) -> Box<dyn Read + 'a> {
    match kind {
        ArchiveKind::TarGz | ArchiveKind::Gz => Box::new(flate2::read::MultiGzDecoder::new(src)),
        ArchiveKind::TarXz | ArchiveKind::Xz => Box::new(lzma_rust2::XzReader::new(src, true)),
        ArchiveKind::TarBz2 | ArchiveKind::Bz2 => Box::new(bzip2::read::MultiBzDecoder::new(src)),
        _ => Box::new(src),
    }
}

// ---------------------------------------------------------------------------------------
// The one traversal everything else is built on.

type Visit<'a> = dyn FnMut(&Entry, &mut dyn Read) -> Result<bool> + 'a;

/// Calls `f` for every entry `want(path, is_dir)` accepts, with a reader over its data.
/// `f` returns `false` to stop early. Unwanted entries are skipped without decompressing
/// where the format allows (ZIP); solid formats still decode through them.
fn walk(op: &mut Opened, password: Option<&str>, want: &dyn Fn(&str, bool) -> bool, f: &mut Visit<'_>) -> Result<()> {
    op.src.seek(SeekFrom::Start(0))?;
    match op.kind {
        ArchiveKind::Zip => {
            let mut zip = zip::ZipArchive::new(&mut op.src).context("reading ZIP directory")?;
            for i in 0..zip.len() {
                let entry = zip_entry(&zip.by_index_raw(i)?);
                if !want(&entry.path, entry.is_dir) {
                    continue;
                }
                let mut file = match (entry.encrypted, password) {
                    (true, Some(pw)) => zip.by_index_decrypt(i, pw.as_bytes()),
                    (true, None) => bail!("{} is password-protected", entry.path),
                    _ => zip.by_index(i),
                }
                .with_context(|| format!("opening {}", entry.path))?;
                if !f(&entry, &mut file)? {
                    break;
                }
            }
        }
        ArchiveKind::SevenZ => {
            let pw = password.map(sevenz_rust2::Password::from).unwrap_or_else(sevenz_rust2::Password::empty);
            let mut reader = sevenz_rust2::ArchiveReader::new(&mut op.src, pw).map_err(sevenz_err)?;
            let mut user_err: Option<anyhow::Error> = None;
            let mut stopped = false;
            let res = reader.for_each_entries(|e, rd| {
                let entry = sevenz_entry(e);
                if !stopped && want(&entry.path, entry.is_dir) {
                    match f(&entry, rd) {
                        Ok(true) => {}
                        Ok(false) => stopped = true,
                        Err(err) => {
                            user_err = Some(err);
                            return Err(sevenz_rust2::Error::from(io::Error::other("aborted")));
                        }
                    }
                }
                if stopped {
                    return Ok(false);
                }
                // Solid blocks are sequential: whatever wasn't read must be consumed (this
                // is also where the CRC check happens).
                io::copy(rd, &mut io::sink()).map_err(sevenz_rust2::Error::from)?;
                Ok(true)
            });
            if let Some(e) = user_err {
                return Err(e);
            }
            res.map_err(sevenz_err)?;
        }
        ArchiveKind::Tar | ArchiveKind::TarGz | ArchiveKind::TarXz | ArchiveKind::TarBz2 => {
            let kind = op.kind;
            let mut ar = tar::Archive::new(decompressor(kind, &mut op.src));
            for e in ar.entries().context("reading TAR")? {
                let mut e = e.context("reading TAR entry")?;
                let Some(entry) = tar_entry(&e)? else { continue };
                if !want(&entry.path, entry.is_dir) {
                    continue;
                }
                if !f(&entry, &mut e)? {
                    break;
                }
            }
        }
        ArchiveKind::Gz | ArchiveKind::Xz | ArchiveKind::Bz2 => {
            let size = if op.kind == ArchiveKind::Gz { gzip_isize(&mut op.src) } else { 0 };
            let entry = single_stream_entry(&op.name, size);
            if want(&entry.path, false) {
                let kind = op.kind;
                let mut d = decompressor(kind, &mut op.src);
                f(&entry, &mut d)?;
            }
        }
    }
    Ok(())
}

fn sevenz_err(e: sevenz_rust2::Error) -> anyhow::Error {
    let msg = e.to_string();
    if msg.to_lowercase().contains("password") { anyhow!("7z archive needs a (correct) password: {msg}") } else { anyhow!("7z: {msg}") }
}

// ---------------------------------------------------------------------------------------
// Public operations.

pub fn list(r: &ArchiveRef, password: Option<&str>) -> Result<Listing> {
    let mut op = open(r, password)?;
    let mut entries = Vec::new();
    let mut solid = false;
    let mut header_encrypted = false;
    match op.kind {
        ArchiveKind::Zip => {
            let mut zip = zip::ZipArchive::new(&mut op.src).context("reading ZIP directory")?;
            for i in 0..zip.len() {
                entries.push(zip_entry(&zip.by_index_raw(i)?));
            }
        }
        ArchiveKind::SevenZ => {
            let pw = password.map(sevenz_rust2::Password::from).unwrap_or_else(sevenz_rust2::Password::empty);
            let archive = match sevenz_rust2::Archive::read(&mut op.src, &pw) {
                Ok(a) => a,
                Err(e) => {
                    header_encrypted = password.is_none();
                    return Err(sevenz_err(e)).context(if header_encrypted { "the file list itself is encrypted" } else { "reading 7z header" });
                }
            };
            solid = archive.is_solid;
            let encrypted_blocks: Vec<bool> =
                archive.blocks.iter().map(|b| b.coders.iter().any(|c| c.encoder_method_id() == sevenz_rust2::EncoderMethod::ID_AES256_SHA256)).collect();
            for (i, e) in archive.files.iter().enumerate() {
                let mut entry = sevenz_entry(e);
                if let Some(Some(b)) = archive.stream_map.file_block_index.get(i) {
                    entry.encrypted = encrypted_blocks.get(*b).copied().unwrap_or(false);
                }
                entries.push(entry);
            }
        }
        _ => {
            walk(&mut op, password, &|_, _| true, &mut |e, _| {
                entries.push(e.clone());
                Ok(true)
            })?;
        }
    }
    let files = entries.iter().filter(|e| !e.is_dir);
    let file_count = files.clone().count() as u64;
    let total_size = files.clone().map(|e| e.size).sum();
    let total_compressed = if entries.iter().any(|e| e.compressed_size.is_some()) {
        Some(entries.iter().filter_map(|e| e.compressed_size).sum())
    } else {
        Some(if r.inner.is_empty() { std::fs::metadata(&r.path).map(|m| m.len()).unwrap_or(0) } else { 0 }).filter(|v| *v > 0)
    };
    let encrypted = header_encrypted || entries.iter().any(|e| e.encrypted);
    Ok(Listing { kind: op.kind, entries, file_count, total_size, total_compressed, encrypted, solid })
}

fn selected(selection: &Option<Vec<String>>, path: &str) -> bool {
    match selection {
        None => true,
        Some(sel) => sel.iter().any(|s| {
            let s = s.trim_end_matches('/');
            path == s || (path.len() > s.len() && path.starts_with(s) && path.as_bytes()[s.len()] == b'/')
        }),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtractOptions {
    pub password: Option<String>,
    /// Entry paths (files or folders) to extract; `None` extracts everything.
    pub entries: Option<Vec<String>>,
    /// Extract into a new folder named after the archive.
    pub into_subfolder: bool,
    /// Drop folder structure and put every file directly in the destination.
    pub flatten: bool,
    pub overwrite: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self { password: None, entries: None, into_subfolder: true, flatten: false, overwrite: false }
    }
}

/// Wraps a reader to report byte progress and honour cancellation.
struct Tracked<'a, R: Read + ?Sized> {
    inner: &'a mut R,
    done: &'a mut u64,
    total: u64,
    progress: &'a dyn Progress,
    cancel: &'a Cancel,
    label: &'a str,
}

impl<R: Read + ?Sized> Read for Tracked<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = self.inner.read(buf)?;
        *self.done += n as u64;
        if self.total > 0 {
            self.progress.report((*self.done as f32 / self.total as f32).min(1.0), self.label);
        }
        Ok(n)
    }
}

pub struct ExtractReport {
    pub dest: PathBuf,
    pub files: Vec<PathBuf>,
}

pub fn extract(r: &ArchiveRef, output_dir: Option<&Path>, opts: &ExtractOptions, progress: &dyn Progress, cancel: &Cancel) -> Result<ExtractReport> {
    let base = util::output_dir_for(&r.path, output_dir);
    let dest = if opts.into_subfolder {
        let name = r.display_name();
        let stem = strip_archive_ext(&name);
        util::unique_path(&base.join(stem))
    } else {
        base
    };
    std::fs::create_dir_all(&dest)?;

    // Byte totals only come cheaply from formats with a central directory.
    let mut op = open(r, opts.password.as_deref())?;
    let total = if matches!(op.kind, ArchiveKind::Zip | ArchiveKind::SevenZ) {
        list(r, opts.password.as_deref()).map(|l| l.entries.iter().filter(|e| selected(&opts.entries, &e.path)).map(|e| e.size).sum()).unwrap_or(0)
    } else {
        0
    };

    let mut files = Vec::new();
    let mut done = 0u64;
    walk(&mut op, opts.password.as_deref(), &|p, _| selected(&opts.entries, p), &mut |e, rd| {
        cancel.check()?;
        let rel = if opts.flatten { e.path.rsplit('/').next().unwrap_or(&e.path).to_string() } else { e.path.clone() };
        let Some(target) = util::safe_join(&dest, &rel) else {
            return Ok(true); // unsafe name ("../x", "/etc/x"): skip it
        };
        if e.is_dir {
            if !opts.flatten {
                std::fs::create_dir_all(&target)?;
            }
            return Ok(true);
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let target = if opts.overwrite { target } else { util::unique_path(&target) };
        let mut out = io::BufWriter::new(File::create(&target).with_context(|| format!("creating {}", target.display()))?);
        let mut tracked = Tracked { inner: rd, done: &mut done, total, progress, cancel, label: &e.path };
        let copied = io::copy(&mut tracked, &mut out).and_then(|_| out.flush());
        if let Err(err) = copied {
            drop(out);
            let _ = std::fs::remove_file(&target);
            return Err(anyhow!(err)).with_context(|| format!("extracting {}", e.path));
        }
        files.push(target);
        Ok(true)
    })?;
    progress.report(1.0, "done");
    Ok(ExtractReport { dest, files })
}

fn strip_archive_ext(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for ext in [".tar.gz", ".tar.xz", ".tar.bz2", ".tgz", ".txz", ".tbz2", ".tbz"] {
        if lower.ends_with(ext) {
            return name[..name.len() - ext.len()].to_string();
        }
    }
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => format!("{name} contents"),
    }
}

/// Reads one entry into memory (up to `limit` bytes).
pub fn read_entry(r: &ArchiveRef, entry: &str, password: Option<&str>, limit: u64) -> Result<Vec<u8>> {
    let mut op = open(r, password)?;
    let mut data: Option<Vec<u8>> = None;
    walk(&mut op, password, &|p, d| !d && p == entry, &mut |_, rd| {
        let mut buf = Vec::new();
        rd.take(limit).read_to_end(&mut buf)?;
        data = Some(buf);
        Ok(false)
    })?;
    data.ok_or_else(|| anyhow!("{entry} not found"))
}

#[derive(Debug, Clone)]
pub enum EntryPreview {
    Text { text: String, truncated: bool },
    Image { image: image::RgbaImage, width: u32, height: u32 },
    Archive,
    Binary { hex: String, size: u64 },
}

const TEXT_PREVIEW: u64 = 256 * 1024;
const IMAGE_PREVIEW: u64 = 64 * 1024 * 1024;

pub fn preview_entry(r: &ArchiveRef, entry: &str, password: Option<&str>) -> Result<EntryPreview> {
    if is_archive_name(entry) {
        return Ok(EntryPreview::Archive);
    }
    let ext = entry.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if image_ops::INPUT_EXTENSIONS.contains(&ext.as_str()) {
        let data = read_entry(r, entry, password, IMAGE_PREVIEW)?;
        let format = image_ops::ImageFormat::from_extension(&ext).unwrap_or(image_ops::ImageFormat::Png);
        let img = image_ops::decode_bytes(&data, format).ok_or_else(|| anyhow!("can't decode this image"))?;
        let (width, height) = (img.width(), img.height());
        let img = if width > 1600 || height > 1600 { img.thumbnail(1600, 1600) } else { img };
        return Ok(EntryPreview::Image { image: img.to_rgba8(), width, height });
    }
    let data = read_entry(r, entry, password, TEXT_PREVIEW + 1)?;
    let truncated = data.len() as u64 > TEXT_PREVIEW;
    let slice = &data[..data.len().min(TEXT_PREVIEW as usize)];
    let text = match std::str::from_utf8(slice) {
        Ok(t) => Some(t.to_string()),
        // A multi-byte character may be cut at the limit.
        Err(e) if truncated && e.valid_up_to() + 4 > slice.len() => Some(String::from_utf8_lossy(&slice[..e.valid_up_to()]).into_owned()),
        Err(_) => None,
    };
    match text {
        Some(t) if !t.contains('\0') => Ok(EntryPreview::Text { text: t, truncated }),
        _ => Ok(EntryPreview::Binary { hex: hex_dump(&data[..data.len().min(4096)]), size: data.len() as u64 }),
    }
}

fn hex_dump(data: &[u8]) -> String {
    let mut out = String::new();
    for (i, chunk) in data.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
        out.push_str(&format!("{:08x}  {:<48} {}\n", i * 16, hex.join(" "), ascii));
    }
    out
}

// ---------------------------------------------------------------------------------------
// Integrity.

#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct VerifyReport {
    pub ok: bool,
    pub kind: Option<ArchiveKind>,
    pub files_checked: u64,
    pub bytes_checked: u64,
    pub nested_checked: u64,
    pub problems: Vec<Problem>,
}

const NESTED_VERIFY_LIMIT: u64 = 1024 * 1024 * 1024;
const MAX_DEPTH: usize = 4;

/// Decompresses every entry, which checks each stored CRC (ZIP, 7z, gzip, xz), and
/// optionally recurses into archives inside the archive.
pub fn verify(r: &ArchiveRef, password: Option<&str>, recursive: bool, progress: &dyn Progress, cancel: &Cancel) -> Result<VerifyReport> {
    let mut report = VerifyReport::default();
    match open(r, password) {
        Ok(mut op) => {
            report.kind = Some(op.kind);
            let total = std::fs::metadata(&r.path).map(|m| m.len()).unwrap_or(0);
            verify_opened(&mut op, password, recursive, "", 0, &mut report, total, progress, cancel)?;
        }
        Err(e) => report.problems.push(Problem { path: String::new(), error: format!("{e:#}") }),
    }
    report.ok = report.problems.is_empty();
    progress.report(1.0, "done");
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn verify_opened(
    op: &mut Opened,
    password: Option<&str>,
    recursive: bool,
    prefix: &str,
    depth: usize,
    report: &mut VerifyReport,
    total: u64,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<()> {
    let mut nested: Vec<(String, Vec<u8>)> = Vec::new();
    let res = walk(op, password, &|_, _| true, &mut |e, rd| {
        cancel.check()?;
        if e.is_dir {
            return Ok(true);
        }
        let keep = recursive && e.is_archive && depth < MAX_DEPTH && e.size <= NESTED_VERIFY_LIMIT;
        let mut buf = Vec::new();
        let result = if keep { rd.read_to_end(&mut buf).map(|n| n as u64) } else { io::copy(rd, &mut io::sink()) };
        match result {
            Ok(n) => {
                report.files_checked += 1;
                report.bytes_checked += n;
                if keep {
                    nested.push((e.path.clone(), buf));
                }
            }
            Err(err) => report.problems.push(Problem { path: format!("{prefix}{}", e.path), error: describe_io(&err) }),
        }
        if depth == 0 && total > 0 {
            progress.report((report.bytes_checked as f32 / (total as f32 * 3.0)).min(0.95), &e.path);
        }
        Ok(true)
    });
    if let Err(e) = res {
        if cancel.is_cancelled() {
            return Err(e);
        }
        report.problems.push(Problem { path: prefix.trim_end_matches(" › ").to_string(), error: format!("{e:#}") });
    }
    for (path, data) in nested {
        let mut src = Source::Mem(Cursor::new(data));
        let Ok(kind) = detect(&mut src, &path) else { continue };
        report.nested_checked += 1;
        let mut inner = Opened { kind, src, name: path.clone() };
        let p = format!("{prefix}{path} › ");
        verify_opened(&mut inner, password, recursive, &p, depth + 1, report, total, progress, cancel)?;
    }
    Ok(())
}

fn describe_io(e: &io::Error) -> String {
    let s = e.to_string();
    if s.to_lowercase().contains("checksum") || s.to_lowercase().contains("crc") { format!("corrupt data (checksum mismatch): {s}") } else { s }
}

// ---------------------------------------------------------------------------------------
// Compare with a folder.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareStatus {
    Same,
    Different,
    OnlyInArchive,
    OnlyInFolder,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompareRow {
    pub path: String,
    pub status: CompareStatus,
    pub archive_size: Option<u64>,
    pub folder_size: Option<u64>,
}

fn compare_key(p: &str) -> String {
    if cfg!(windows) || cfg!(target_os = "macos") { p.to_lowercase() } else { p.to_string() }
}

pub fn compare(r: &ArchiveRef, password: Option<&str>, folder: &Path, progress: &dyn Progress, cancel: &Cancel) -> Result<Vec<CompareRow>> {
    let listing = list(r, password)?;
    let mut archive: HashMap<String, Entry> = HashMap::new();
    for e in listing.entries.into_iter().filter(|e| !e.is_dir) {
        archive.insert(compare_key(&e.path), e);
    }

    let mut on_disk: HashMap<String, (String, PathBuf, u64)> = HashMap::new();
    for entry in walkdir::WalkDir::new(folder).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(folder).unwrap_or(entry.path()).to_string_lossy().replace('\\', "/");
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        on_disk.insert(compare_key(&rel), (rel, entry.into_path(), size));
    }

    // Same-size pairs need content checks. Formats without stored CRCs get them computed
    // in a single pass over the archive.
    let needs_crc: Vec<String> =
        archive.iter().filter(|(k, e)| e.crc32.is_none() && on_disk.get(*k).is_some_and(|d| d.2 == e.size)).map(|(k, _)| k.clone()).collect();
    if !needs_crc.is_empty() && !listing.kind.has_stored_crc() {
        let mut op = open(r, password)?;
        let wanted: std::collections::HashSet<String> = needs_crc.into_iter().collect();
        let mut computed: HashMap<String, u32> = HashMap::new();
        walk(&mut op, password, &|p, d| !d && wanted.contains(&compare_key(p)), &mut |e, rd| {
            cancel.check()?;
            let h = crate::checksum::hash_reader(rd, &[crate::checksum::Algo::Crc32], None, &util::NoProgress, cancel)?;
            computed.insert(compare_key(&e.path), u32::from_str_radix(h.crc32.as_deref().unwrap_or("0"), 16)?);
            Ok(true)
        })?;
        for (k, crc) in computed {
            if let Some(e) = archive.get_mut(&k) {
                e.crc32 = Some(crc);
            }
        }
    }

    let mut rows = Vec::new();
    let total = archive.len().max(1);
    for (i, (k, e)) in archive.iter().enumerate() {
        cancel.check()?;
        let row = match on_disk.get(k) {
            None => CompareRow { path: e.path.clone(), status: CompareStatus::OnlyInArchive, archive_size: Some(e.size), folder_size: None },
            Some((_, disk_path, disk_size)) => {
                let same = *disk_size == e.size
                    && match e.crc32 {
                        Some(crc) => {
                            let h = crate::checksum::hash_file(disk_path, &[crate::checksum::Algo::Crc32], &util::NoProgress, cancel)?;
                            h.crc32.as_deref() == Some(format!("{crc:08x}").as_str())
                        }
                        None => false,
                    };
                CompareRow {
                    path: e.path.clone(),
                    status: if same { CompareStatus::Same } else { CompareStatus::Different },
                    archive_size: Some(e.size),
                    folder_size: Some(*disk_size),
                }
            }
        };
        rows.push(row);
        progress.report((i + 1) as f32 / total as f32, &e.path);
    }
    for (k, (rel, _, size)) in &on_disk {
        if !archive.contains_key(k) {
            rows.push(CompareRow { path: rel.clone(), status: CompareStatus::OnlyInFolder, archive_size: None, folder_size: Some(*size) });
        }
    }
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(rows)
}

// ---------------------------------------------------------------------------------------
// Creating archives.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CreateFormat {
    Zip,
    #[serde(rename = "7z")]
    SevenZ,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CreateOptions {
    pub format: CreateFormat,
    /// 0 (store) ..= 9 (smallest). ZIP only; 7z always uses LZMA2.
    pub level: u8,
    /// File name for the archive, without extension. Defaults to the input's name.
    pub name: Option<String>,
    /// ZIP: AES-256 encryption. 7z: encrypted contents.
    pub password: Option<String>,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self { format: CreateFormat::Zip, level: 6, name: None, password: None }
    }
}

/// Every file/folder to add, with its name inside the archive.
fn collect_inputs(inputs: &[PathBuf]) -> Vec<(PathBuf, String, bool)> {
    let mut items = Vec::new();
    for input in inputs {
        let base = input.parent().unwrap_or(Path::new(""));
        if input.is_dir() {
            for e in walkdir::WalkDir::new(input).sort_by_file_name().into_iter().flatten() {
                let rel = e.path().strip_prefix(base).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
                if e.file_type().is_dir() {
                    items.push((e.into_path(), rel, true));
                } else if e.file_type().is_file() {
                    items.push((e.into_path(), rel, false));
                }
            }
        } else {
            let name = input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            items.push((input.clone(), name, false));
        }
    }
    items
}

pub fn create(inputs: &[PathBuf], output_dir: Option<&Path>, opts: &CreateOptions, progress: &dyn Progress, cancel: &Cancel) -> Result<PathBuf> {
    let first = inputs.first().ok_or_else(|| anyhow!("nothing to archive"))?;
    let dir = util::output_dir_for(first, output_dir);
    std::fs::create_dir_all(&dir)?;
    let name = opts.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
        if inputs.len() == 1 {
            first.file_stem().or(first.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Archive".into())
        } else {
            first.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Archive".into())
        }
    });
    let ext = match opts.format {
        CreateFormat::Zip => "zip",
        CreateFormat::SevenZ => "7z",
    };
    let out = util::unique_path(&dir.join(format!("{name}.{ext}")));
    let items = collect_inputs(inputs);
    let total: u64 = items.iter().filter(|i| !i.2).map(|i| std::fs::metadata(&i.0).map(|m| m.len()).unwrap_or(0)).sum();
    let mut done = 0u64;

    let result = (|| -> Result<()> {
        match opts.format {
            CreateFormat::Zip => {
                use zip::write::SimpleFileOptions;
                let mut w = zip::ZipWriter::new(io::BufWriter::new(File::create(&out)?));
                for (path, name, is_dir) in &items {
                    cancel.check()?;
                    let meta = std::fs::metadata(path)?;
                    let mut fo = SimpleFileOptions::default().large_file(meta.len() >= u32::MAX as u64);
                    fo = if opts.level == 0 {
                        fo.compression_method(zip::CompressionMethod::Stored)
                    } else {
                        fo.compression_method(zip::CompressionMethod::Deflated).compression_level(Some(opts.level.min(9) as i64))
                    };
                    if let Ok(m) = meta.modified() {
                        let dt: chrono::DateTime<chrono::Local> = m.into();
                        use chrono::{Datelike, Timelike};
                        if let Ok(zdt) = zip::DateTime::from_date_and_time(
                            dt.year().clamp(1980, 2107) as u16,
                            dt.month() as u8,
                            dt.day() as u8,
                            dt.hour() as u8,
                            dt.minute() as u8,
                            dt.second().min(59) as u8,
                        ) {
                            fo = fo.last_modified_time(zdt);
                        }
                    }
                    if let Some(pw) = opts.password.as_deref().filter(|p| !p.is_empty()) {
                        fo = fo.with_aes_encryption(zip::AesMode::Aes256, pw);
                    }
                    if *is_dir {
                        w.add_directory(name.as_str(), fo)?;
                        continue;
                    }
                    w.start_file(name.as_str(), fo)?;
                    let mut f = File::open(path)?;
                    let mut tracked = Tracked { inner: &mut f, done: &mut done, total, progress, cancel, label: name };
                    io::copy(&mut tracked, &mut w)?;
                }
                w.finish()?.flush()?;
            }
            CreateFormat::SevenZ => {
                let mut w = sevenz_rust2::ArchiveWriter::create(&out).map_err(sevenz_err)?;
                if let Some(pw) = opts.password.as_deref().filter(|p| !p.is_empty()) {
                    w.set_content_methods(vec![
                        sevenz_rust2::encoder_options::AesEncoderOptions::new(pw.into()).into(),
                        sevenz_rust2::EncoderMethod::LZMA2.into(),
                    ]);
                }
                for (path, name, is_dir) in &items {
                    cancel.check()?;
                    let entry = sevenz_rust2::ArchiveEntry::from_path(path, name.clone());
                    if *is_dir {
                        w.push_archive_entry::<File>(entry, None).map_err(sevenz_err)?;
                    } else {
                        let mut f = File::open(path)?;
                        let tracked = Tracked { inner: &mut f, done: &mut done, total, progress, cancel, label: name };
                        w.push_archive_entry(entry, Some(tracked)).map_err(sevenz_err)?;
                    }
                }
                w.finish()?;
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&out);
        return Err(e);
    }
    progress.report(1.0, "done");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;

    fn tree(dir: &Path) -> PathBuf {
        let root = dir.join("project");
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::write(root.join("README.md"), "# hello\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n".repeat(100)).unwrap();
        std::fs::write(root.join("src/nested/data.bin"), (0..5000u32).flat_map(|i| i.to_le_bytes()).collect::<Vec<_>>()).unwrap();
        root
    }

    fn make(dir: &Path, format: CreateFormat, password: Option<&str>) -> PathBuf {
        let root = tree(dir);
        let opts = CreateOptions { format, password: password.map(String::from), ..Default::default() };
        create(&[root], Some(&dir.join("out")), &opts, &NoProgress, &Cancel::new()).unwrap()
    }

    fn file_paths(l: &Listing) -> Vec<String> {
        let mut v: Vec<String> = l.entries.iter().filter(|e| !e.is_dir).map(|e| e.path.clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn zip_and_7z_roundtrip_list_extract_verify() {
        for format in [CreateFormat::Zip, CreateFormat::SevenZ] {
            let dir = tempfile::tempdir().unwrap();
            let archive = make(dir.path(), format, None);
            let r = ArchiveRef::file(&archive);
            let l = list(&r, None).unwrap();
            assert_eq!(file_paths(&l), vec!["project/README.md", "project/src/main.rs", "project/src/nested/data.bin"], "{format:?}");
            assert_eq!(l.file_count, 3);

            let opts = ExtractOptions { entries: Some(vec!["project/src".into()]), ..Default::default() };
            let rep = extract(&r, Some(&dir.path().join("x")), &opts, &NoProgress, &Cancel::new()).unwrap();
            assert_eq!(rep.files.len(), 2, "{format:?}");
            assert_eq!(std::fs::read_to_string(rep.dest.join("project/src/main.rs")).unwrap(), "fn main() {}\n".repeat(100));
            assert!(!rep.dest.join("project/README.md").exists());

            let v = verify(&r, None, true, &NoProgress, &Cancel::new()).unwrap();
            assert!(v.ok, "{format:?}: {:?}", v.problems);
            assert_eq!(v.files_checked, 3);

            match preview_entry(&r, "project/README.md", None).unwrap() {
                EntryPreview::Text { text, .. } => assert_eq!(text, "# hello\n"),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn detects_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let archive = make(dir.path(), CreateFormat::Zip, None);
        let mut bytes = std::fs::read(&archive).unwrap();
        // Flip bytes in the middle of the compressed data of the largest entry.
        let pos = bytes.len() / 3;
        for b in &mut bytes[pos..pos + 16] {
            *b ^= 0xff;
        }
        std::fs::write(&archive, &bytes).unwrap();
        let v = verify(&ArchiveRef::file(&archive), None, false, &NoProgress, &Cancel::new()).unwrap();
        assert!(!v.ok);

        let garbage = dir.path().join("junk.zip");
        std::fs::write(&garbage, b"PK\x03\x04 definitely not a zip").unwrap();
        let v = verify(&ArchiveRef::file(&garbage), None, false, &NoProgress, &Cancel::new()).unwrap();
        assert!(!v.ok);
    }

    #[test]
    fn nested_archives() {
        let dir = tempfile::tempdir().unwrap();
        let inner = make(dir.path(), CreateFormat::SevenZ, None);
        let outer_opts = CreateOptions { name: Some("outer".into()), ..Default::default() };
        let outer = create(std::slice::from_ref(&inner), Some(dir.path()), &outer_opts, &NoProgress, &Cancel::new()).unwrap();
        let r = ArchiveRef::file(&outer);
        let l = list(&r, None).unwrap();
        assert!(l.entries[0].is_archive);
        let nested = ArchiveRef { path: outer.clone(), inner: vec!["project.7z".into()] };
        assert_eq!(list(&nested, None).unwrap().file_count, 3);
        assert_eq!(read_entry(&nested, "project/README.md", None, 1024).unwrap(), b"# hello\n");
        let v = verify(&r, None, true, &NoProgress, &Cancel::new()).unwrap();
        assert!(v.ok);
        assert_eq!(v.nested_checked, 1);
        assert_eq!(v.files_checked, 4);
    }

    #[test]
    fn tar_flavours() {
        let dir = tempfile::tempdir().unwrap();
        let root = tree(dir.path());
        let tar_bytes = {
            let mut b = tar::Builder::new(Vec::new());
            b.append_dir_all("project", &root).unwrap();
            b.into_inner().unwrap()
        };
        let gz = {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&tar_bytes).unwrap();
            e.finish().unwrap()
        };
        let bz = {
            let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
            e.write_all(&tar_bytes).unwrap();
            e.finish().unwrap()
        };
        for (name, bytes, kind) in
            [("a.tar", tar_bytes.clone(), ArchiveKind::Tar), ("a.tar.gz", gz.clone(), ArchiveKind::TarGz), ("a.tar.bz2", bz, ArchiveKind::TarBz2)]
        {
            let p = dir.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            let r = ArchiveRef::file(&p);
            let l = list(&r, None).unwrap();
            assert_eq!(l.kind, kind);
            assert_eq!(file_paths(&l).len(), 3, "{name}");
            assert!(verify(&r, None, false, &NoProgress, &Cancel::new()).unwrap().ok);
            // Compare against the source tree: everything matches.
            let rows = compare(&r, None, dir.path(), &NoProgress, &Cancel::new()).unwrap();
            let same = rows.iter().filter(|r| r.status == CompareStatus::Same).count();
            assert_eq!(same, 3, "{name}: {rows:?}");
        }
        // A plain .gz of a single file is shown as one entry.
        let p = dir.path().join("notes.txt.gz");
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b"just text").unwrap();
        std::fs::write(&p, e.finish().unwrap()).unwrap();
        let l = list(&ArchiveRef::file(&p), None).unwrap();
        assert_eq!(l.kind, ArchiveKind::Gz);
        assert_eq!(l.entries[0].path, "notes.txt");
        assert_eq!(l.entries[0].size, 9);
    }

    #[test]
    fn compare_finds_differences() {
        let dir = tempfile::tempdir().unwrap();
        let archive = make(dir.path(), CreateFormat::Zip, None);
        let root = dir.path().join("project");
        std::fs::write(root.join("README.md"), "# HELLO\n").unwrap(); // same size, different bytes
        std::fs::remove_file(root.join("src/main.rs")).unwrap();
        std::fs::write(root.join("new.txt"), "new").unwrap();
        let rows = compare(&ArchiveRef::file(&archive), None, dir.path(), &NoProgress, &Cancel::new()).unwrap();
        let status = |p: &str| rows.iter().find(|r| r.path == p).map(|r| r.status);
        assert_eq!(status("project/README.md"), Some(CompareStatus::Different));
        assert_eq!(status("project/src/main.rs"), Some(CompareStatus::OnlyInArchive));
        assert_eq!(status("project/new.txt"), Some(CompareStatus::OnlyInFolder));
        assert_eq!(status("project/src/nested/data.bin"), Some(CompareStatus::Same));
    }

    #[test]
    fn passwords() {
        for format in [CreateFormat::Zip, CreateFormat::SevenZ] {
            let dir = tempfile::tempdir().unwrap();
            let archive = make(dir.path(), format, Some("s3cret"));
            let r = ArchiveRef::file(&archive);
            assert!(read_entry(&r, "project/README.md", None, 1024).is_err(), "{format:?}");
            assert!(read_entry(&r, "project/README.md", Some("wrong"), 1024).is_err(), "{format:?}");
            assert_eq!(read_entry(&r, "project/README.md", Some("s3cret"), 1024).unwrap(), b"# hello\n", "{format:?}");
            assert!(list(&r, Some("s3cret")).unwrap().encrypted, "{format:?}");
        }
    }

    #[test]
    fn refuses_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("evil.zip");
        let mut w = zip::ZipWriter::new(File::create(&p).unwrap());
        w.start_file("../../escaped.txt", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(b"x").unwrap();
        w.start_file("fine.txt", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(b"y").unwrap();
        w.finish().unwrap();
        let rep = extract(&ArchiveRef::file(&p), Some(&dir.path().join("a/b")), &ExtractOptions::default(), &NoProgress, &Cancel::new()).unwrap();
        assert_eq!(rep.files.len(), 1);
        assert!(!dir.path().join("escaped.txt").exists());
        assert!(!dir.path().join("a/escaped.txt").exists());
    }
}
