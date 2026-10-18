//! The operations a queued job can run, as one serialisable enum the UI sends over.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::archive::{self, ArchiveRef, CreateOptions, ExtractOptions};
use crate::checksum::{self, Algo};
use crate::image_ops::{self, ImageOptions};
use crate::media::{self, MediaOptions};
use crate::pdf::{self, PdfImageOptions, PdfPageOptions};
use crate::util::{Cancel, Progress};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Operation {
    ConvertImage {
        #[serde(default)]
        options: ImageOptions,
    },
    ConvertMedia {
        #[serde(default)]
        options: MediaOptions,
    },
    PdfPages {
        #[serde(default)]
        options: PdfPageOptions,
    },
    PdfImages {
        #[serde(default)]
        options: PdfImageOptions,
    },
    ExtractArchive {
        #[serde(default)]
        options: ExtractOptions,
        /// Nested archive chain inside the input, if extracting from an inner archive.
        #[serde(default)]
        inner: Vec<String>,
    },
    VerifyArchive {
        #[serde(default)]
        password: Option<String>,
        #[serde(default = "yes")]
        recursive: bool,
    },
    /// Combines all inputs into one archive.
    CreateArchive {
        #[serde(default)]
        options: CreateOptions,
    },
    /// Copies all inputs into `dest`.
    Copy {
        dest: PathBuf,
    },
    /// Moves all inputs into `dest`.
    Move {
        dest: PathBuf,
    },
    Checksum {
        #[serde(default = "all_algos")]
        algos: Vec<Algo>,
    },
    VerifyChecksums,
}

fn yes() -> bool {
    true
}
fn all_algos() -> Vec<Algo> {
    vec![Algo::Crc32, Algo::Md5, Algo::Sha256]
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct OperationOutput {
    pub files: Vec<PathBuf>,
    pub summary: String,
    /// Structured result for the UI (verify report, hashes, ...).
    pub details: Option<serde_json::Value>,
}

impl Operation {
    /// Whether all inputs go into a single job (otherwise one job per input).
    pub fn is_multi_input(&self) -> bool {
        matches!(self, Operation::CreateArchive { .. } | Operation::Copy { .. } | Operation::Move { .. })
    }

    pub fn label(&self) -> String {
        match self {
            Operation::ConvertImage { options } => match options.format {
                Some(f) => format!("Image to {}", f.extension().to_uppercase()),
                None => "Compress / resize image".into(),
            },
            Operation::ConvertMedia { options } => format!("Convert to {}", options.target.extension().to_uppercase()),
            Operation::PdfPages { options } if options.split => "Split PDF pages".into(),
            Operation::PdfPages { .. } => "Extract PDF pages".into(),
            Operation::PdfImages { .. } => "Extract PDF images".into(),
            Operation::ExtractArchive { .. } => "Extract".into(),
            Operation::VerifyArchive { .. } => "Test archive".into(),
            Operation::CreateArchive { options } => match options.format {
                archive::CreateFormat::Zip => "Create ZIP".into(),
                archive::CreateFormat::SevenZ => "Create 7z".into(),
            },
            Operation::Copy { dest } => format!("Copy to {}", dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            Operation::Move { dest } => format!("Move to {}", dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            Operation::Checksum { .. } => "Checksums".into(),
            Operation::VerifyChecksums => "Verify checksums".into(),
        }
    }

    pub fn run(&self, inputs: &[PathBuf], output_dir: Option<&Path>, progress: &dyn Progress, cancel: &Cancel) -> Result<OperationOutput> {
        let Some(input) = inputs.first() else { bail!("no input files") };
        let input = input.as_path();
        cancel.check()?;
        let out = match self {
            Operation::ConvertImage { options } => {
                let before = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
                let path = image_ops::convert_file(input, output_dir, options)?;
                let after = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                OperationOutput { summary: size_change(before, after), files: vec![path], details: None }
            }
            Operation::ConvertMedia { options } => {
                let before = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
                let path = media::convert(input, output_dir, options, progress, cancel)?;
                let after = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                OperationOutput { summary: size_change(before, after), files: vec![path], details: None }
            }
            Operation::PdfPages { options } => {
                let files = pdf::extract_pages(input, output_dir, options, progress, cancel)?;
                OperationOutput { summary: format!("{} PDF file(s)", files.len()), files, details: None }
            }
            Operation::PdfImages { options } => {
                let r = pdf::extract_images(input, output_dir, options, progress, cancel)?;
                let mut summary = format!("{} image(s)", r.files.len());
                if r.skipped > 0 {
                    summary.push_str(&format!(", {} in unsupported encodings skipped", r.skipped));
                }
                OperationOutput { summary, files: r.files, details: None }
            }
            Operation::ExtractArchive { options, inner } => {
                let r = ArchiveRef { path: input.to_path_buf(), inner: inner.clone() };
                let rep = archive::extract(&r, output_dir, options, progress, cancel)?;
                OperationOutput { summary: format!("{} file(s) to {}", rep.files.len(), rep.dest.display()), files: vec![rep.dest], details: None }
            }
            Operation::VerifyArchive { password, recursive } => {
                let rep = archive::verify(&ArchiveRef::file(input), password.as_deref(), *recursive, progress, cancel)?;
                let summary = if rep.ok {
                    let nested = if rep.nested_checked > 0 { format!(" incl. {} nested archive(s)", rep.nested_checked) } else { String::new() };
                    format!("OK: {} file(s) tested{nested}", rep.files_checked)
                } else {
                    format!("{} problem(s) found", rep.problems.len())
                };
                let ok = rep.ok;
                let out = OperationOutput { summary, files: vec![], details: Some(serde_json::to_value(&rep)?) };
                if !ok {
                    // Surface as a failed job so it stands out in the queue.
                    let first =
                        rep.problems.first().map(|p| format!("{}: {}", if p.path.is_empty() { "archive" } else { &p.path }, p.error)).unwrap_or_default();
                    bail!("{}. {first}", out.summary);
                }
                out
            }
            Operation::CreateArchive { options } => {
                let path = archive::create(inputs, output_dir, options, progress, cancel)?;
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                OperationOutput { summary: crate::util::human_size(size), files: vec![path], details: None }
            }
            Operation::Copy { dest } => {
                let files = crate::fsops::copy_items(inputs, dest, progress, cancel)?;
                OperationOutput { summary: format!("{} item(s) copied", files.len()), files, details: None }
            }
            Operation::Move { dest } => {
                let files = crate::fsops::move_items(inputs, dest, progress, cancel)?;
                OperationOutput { summary: format!("{} item(s) moved", files.len()), files, details: None }
            }
            Operation::Checksum { algos } => {
                let h = checksum::hash_file(input, algos, progress, cancel)?;
                let summary = h.sha256.clone().or(h.md5.clone()).or(h.crc32.clone()).unwrap_or_default();
                OperationOutput { summary, files: vec![], details: Some(serde_json::to_value(&h)?) }
            }
            Operation::VerifyChecksums => {
                let results = checksum::verify_list(input, progress, cancel)?;
                let bad = results.iter().filter(|r| r.status != checksum::CheckStatus::Ok).count();
                let summary = format!("{} OK, {bad} failed or missing", results.len() - bad);
                let out = OperationOutput { summary, files: vec![], details: Some(serde_json::to_value(&results)?) };
                if bad > 0 {
                    bail!("{}", out.summary);
                }
                out
            }
        };
        Ok(out)
    }
}

fn size_change(before: u64, after: u64) -> String {
    use crate::util::human_size;
    if before == 0 {
        return human_size(after);
    }
    let pct = (after as f64 / before as f64 - 1.0) * 100.0;
    format!("{} to {} ({pct:+.0}%)", human_size(before), human_size(after))
}

/// Suggests operations for a set of dropped files, most likely first. Used by the UI to
/// pre-select something sensible.
pub fn suggest(paths: &[PathBuf]) -> Vec<&'static str> {
    let all = |f: fn(&Path) -> bool| !paths.is_empty() && paths.iter().all(|p| f(p));
    if all(image_ops::is_image) {
        vec!["convert_image"]
    } else if all(|p| media::is_video(p) || media::is_audio(p)) {
        vec!["convert_media"]
    } else if all(pdf::is_pdf) {
        vec!["pdf_pages", "pdf_images"]
    } else if all(archive::is_archive) {
        vec!["extract_archive", "verify_archive"]
    } else if all(checksum::is_checksum_list) {
        vec!["verify_checksums"]
    } else {
        vec!["create_archive", "checksum"]
    }
}
