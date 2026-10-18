//! PDF page and image extraction (pure Rust, via lopdf).

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use lopdf::{Document, Object};
use serde::{Deserialize, Serialize};

use crate::util::{self, Cancel, Progress};

pub fn is_pdf(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

#[derive(Debug, Clone, Serialize)]
pub struct PdfInfo {
    pub pages: u32,
    pub version: String,
    pub encrypted: bool,
    pub image_count: usize,
}

fn load(path: &Path) -> Result<Document> {
    Document::load(path).with_context(|| format!("opening {}", path.display()))
}

pub fn info(path: &Path) -> Result<PdfInfo> {
    let doc = load(path)?;
    let pages = doc.get_pages();
    let mut ids = HashSet::new();
    for id in pages.values() {
        for img in doc.get_page_images(*id).unwrap_or_default() {
            ids.insert(img.id);
        }
    }
    Ok(PdfInfo { pages: pages.len() as u32, version: doc.version.clone(), encrypted: doc.is_encrypted(), image_count: ids.len() })
}

/// Parses "1-3, 5, 9-" into page numbers (1-based) within `1..=count`.
/// An empty spec means every page.
pub fn parse_ranges(spec: &str, count: u32) -> Result<Vec<u32>> {
    let spec = spec.trim();
    if spec.is_empty() || spec.eq_ignore_ascii_case("all") {
        return Ok((1..=count).collect());
    }
    let mut pages = BTreeSet::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => {
                let a = if a.trim().is_empty() { 1 } else { a.trim().parse::<u32>()? };
                let b = if b.trim().is_empty() { count } else { b.trim().parse::<u32>()? };
                (a, b)
            }
            None => {
                let n = part.parse::<u32>().with_context(|| format!("'{part}' is not a page number"))?;
                (n, n)
            }
        };
        if a == 0 || a > b {
            bail!("invalid page range '{part}'");
        }
        if a > count {
            bail!("page {a} is past the end (the document has {count} pages)");
        }
        pages.extend(a..=b.min(count));
    }
    Ok(pages.into_iter().collect())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct PdfPageOptions {
    /// e.g. "1-3,5"; empty for all pages.
    pub ranges: String,
    /// One PDF per page instead of one PDF with the selected pages.
    pub split: bool,
}

fn save_with_pages(src: &Document, keep: &[u32], out: &Path) -> Result<()> {
    let mut doc = src.clone();
    let all: Vec<u32> = doc.get_pages().keys().copied().collect();
    let keep: HashSet<u32> = keep.iter().copied().collect();
    let drop: Vec<u32> = all.into_iter().filter(|p| !keep.contains(p)).collect();
    doc.delete_pages(&drop);
    doc.prune_objects();
    doc.renumber_objects();
    doc.compress();
    doc.save(out).with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

pub fn extract_pages(input: &Path, output_dir: Option<&Path>, opts: &PdfPageOptions, progress: &dyn Progress, cancel: &Cancel) -> Result<Vec<PathBuf>> {
    let doc = load(input)?;
    let count = doc.get_pages().len() as u32;
    let pages = parse_ranges(&opts.ranges, count)?;
    let dir = util::output_dir_for(input, output_dir);
    std::fs::create_dir_all(&dir)?;

    if !opts.split {
        let suffix = if opts.ranges.trim().is_empty() { " (copy)".to_string() } else { format!(" (pages {})", opts.ranges.trim()) };
        let out = util::output_path(input, &dir, &suffix.replace([':', '/', '\\'], "_"), "pdf", false);
        save_with_pages(&doc, &pages, &out)?;
        progress.report(1.0, "done");
        return Ok(vec![out]);
    }

    let width = count.to_string().len();
    let mut outputs = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        cancel.check()?;
        let out = util::output_path(input, &dir, &format!(" - page {page:0width$}"), "pdf", false);
        save_with_pages(&doc, &[*page], &out)?;
        outputs.push(out);
        progress.report((i + 1) as f32 / pages.len() as f32, &format!("page {page}"));
    }
    Ok(outputs)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PdfImageOptions {
    pub ranges: String,
}

pub struct ExtractedImages {
    pub files: Vec<PathBuf>,
    /// Images in encodings we can't export yet (e.g. indexed colour, CCITT fax).
    pub skipped: usize,
}

const MAX_IMAGE_BYTES: usize = 512 * 1024 * 1024;

/// Exports embedded images: JPEGs are copied byte-for-byte, raw bitmaps become PNG.
pub fn extract_images(input: &Path, output_dir: Option<&Path>, opts: &PdfImageOptions, progress: &dyn Progress, cancel: &Cancel) -> Result<ExtractedImages> {
    let doc = load(input)?;
    let page_ids = doc.get_pages();
    let pages = parse_ranges(&opts.ranges, page_ids.len() as u32)?;
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = util::output_dir_for(input, output_dir).join(format!("{stem} images"));
    let dir = util::unique_path(&dir);
    std::fs::create_dir_all(&dir)?;

    let mut seen = HashSet::new();
    let mut files = Vec::new();
    let mut skipped = 0;
    for (pi, page) in pages.iter().enumerate() {
        cancel.check()?;
        let id = page_ids[page];
        for (n, img) in doc.get_page_images(id).unwrap_or_default().into_iter().enumerate() {
            if !seen.insert(img.id) {
                continue;
            }
            let base = dir.join(format!("page{page:03}-{:02}", n + 1));
            let filters = img.filters.clone().unwrap_or_default();
            let last = filters.last().map(String::as_str);
            let written = match last {
                Some("DCTDecode") if filters.len() == 1 => {
                    let p = base.with_extension("jpg");
                    std::fs::write(&p, img.content)?;
                    Some(p)
                }
                Some("JPXDecode") if filters.len() == 1 => {
                    let p = base.with_extension("jp2");
                    std::fs::write(&p, img.content)?;
                    Some(p)
                }
                None | Some("FlateDecode") | Some("LZWDecode") => {
                    raw_to_png(&doc, img.id, img.width, img.height, img.bits_per_component, img.origin_dict, &base)?
                }
                _ => None,
            };
            match written {
                Some(p) => files.push(p),
                None => skipped += 1,
            }
        }
        progress.report((pi + 1) as f32 / pages.len() as f32, &format!("page {page}"));
    }
    Ok(ExtractedImages { files, skipped })
}

fn components(doc: &Document, dict: &lopdf::Dictionary) -> Option<u32> {
    let cs = dict.get(b"ColorSpace").ok()?;
    let cs = match cs {
        Object::Reference(r) => doc.get_object(*r).ok()?,
        other => other,
    };
    match cs {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"CalGray" => Some(1),
            b"DeviceRGB" | b"CalRGB" => Some(3),
            b"DeviceCMYK" => Some(4),
            _ => None,
        },
        Object::Array(arr) => {
            let kind = arr.first()?.as_name().ok()?;
            match kind {
                b"ICCBased" => {
                    let stream = doc.get_object(arr.get(1)?.as_reference().ok()?).ok()?.as_stream().ok()?;
                    stream.dict.get(b"N").ok()?.as_i64().ok().map(|n| n as u32)
                }
                b"CalRGB" => Some(3),
                b"CalGray" => Some(1),
                _ => None,
            }
        }
        _ => None,
    }
}

fn raw_to_png(
    doc: &Document,
    id: lopdf::ObjectId,
    width: i64,
    height: i64,
    bpc: Option<i64>,
    dict: &lopdf::Dictionary,
    base: &Path,
) -> Result<Option<PathBuf>> {
    let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) else { return Ok(None) };
    if bpc.unwrap_or(8) != 8 {
        return Ok(None);
    }
    let Some(n) = components(doc, dict) else { return Ok(None) };
    let stream = doc.get_object(id)?.as_stream()?;
    let data = if dict.get(b"Filter").is_ok() {
        match stream.decompressed_content_with_limit(MAX_IMAGE_BYTES) {
            Ok(d) => d,
            Err(_) => return Ok(None),
        }
    } else {
        stream.content.clone()
    };
    let expected = w as usize * h as usize * n as usize;
    if data.len() < expected {
        return Ok(None);
    }
    let data = &data[..expected];
    let img: image::DynamicImage = match n {
        1 => image::GrayImage::from_raw(w, h, data.to_vec()).map(Into::into),
        3 => image::RgbImage::from_raw(w, h, data.to_vec()).map(Into::into),
        4 => {
            // Naive CMYK → RGB; good enough for a preview-quality export.
            let rgb: Vec<u8> = data
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| {
                    let k = 255 - p[3] as u32;
                    [0, 1, 2].map(|i| ((255 - p[i] as u32) * k / 255) as u8)
                })
                .collect();
            image::RgbImage::from_raw(w, h, rgb).map(Into::into)
        }
        _ => None,
    }
    .ok_or_else(|| anyhow::anyhow!("bad image buffer"))?;
    let p = base.with_extension("png");
    img.save(&p)?;
    Ok(Some(p))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::{Stream, dictionary};

    /// Builds a small PDF with `pages` pages; each page draws one raw RGB image.
    pub fn sample_pdf(path: &Path, pages: u32) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut kids = vec![];
        for i in 0..pages {
            let pixels: Vec<u8> = (0..16 * 16).flat_map(|p| [(p % 256) as u8, (i * 40) as u8, 200]).collect();
            let img = Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image", "Width" => 16, "Height" => 16,
                    "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
                },
                pixels,
            );
            let img_id = doc.add_object(img);
            let content = Content {
                operations: vec![
                    Operation::new("q", vec![]),
                    Operation::new("cm", vec![100.into(), 0.into(), 0.into(), 100.into(), 50.into(), 50.into()]),
                    Operation::new("Do", vec![Object::Name(b"Im0".to_vec())]),
                    Operation::new("Q", vec![]),
                ],
            };
            let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
                "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => img_id } },
                "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
            });
            kids.push(page_id.into());
        }
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => pages as i64 }));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        doc.compress();
        doc.save(path).unwrap();
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_ranges("", 3).unwrap(), vec![1, 2, 3]);
        assert_eq!(parse_ranges("1-2, 5, 7-", 8).unwrap(), vec![1, 2, 5, 7, 8]);
        assert_eq!(parse_ranges("-2", 8).unwrap(), vec![1, 2]);
        assert!(parse_ranges("9", 8).is_err());
        assert!(parse_ranges("3-1", 8).is_err());
        assert!(parse_ranges("x", 8).is_err());
    }

    #[test]
    fn extracts_and_splits_pages() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("doc.pdf");
        sample_pdf(&input, 4);
        assert_eq!(info(&input).unwrap().pages, 4);
        let opts = PdfPageOptions { ranges: "2-3".into(), split: false };
        let out = extract_pages(&input, None, &opts, &crate::util::NoProgress, &Cancel::new()).unwrap();
        assert_eq!(info(&out[0]).unwrap().pages, 2);
        let opts = PdfPageOptions { ranges: "".into(), split: true };
        let out = extract_pages(&input, None, &opts, &crate::util::NoProgress, &Cancel::new()).unwrap();
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|p| info(p).unwrap().pages == 1));
    }

    #[test]
    fn extracts_images() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("doc.pdf");
        sample_pdf(&input, 3);
        let r = extract_images(&input, None, &PdfImageOptions::default(), &crate::util::NoProgress, &Cancel::new()).unwrap();
        assert_eq!(r.files.len(), 3);
        assert_eq!(r.skipped, 0);
        let img = image::open(&r.files[0]).unwrap();
        assert_eq!(img.width(), 16);
    }
}
