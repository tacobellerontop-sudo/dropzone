use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use image::codecs::avif::AvifEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageReader};
use serde::{Deserialize, Serialize};

use crate::util::{self, find_tool};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
    Avif,
    Gif,
    Bmp,
    Tiff,
    Ico,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Gif => "gif",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
            Self::Ico => "ico",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
            Self::Gif => "image/gif",
            Self::Bmp => "image/bmp",
            Self::Tiff => "image/tiff",
            Self::Ico => "image/x-icon",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "png" => Self::Png,
            "jpg" | "jpeg" | "jfif" => Self::Jpeg,
            "webp" => Self::Webp,
            "avif" => Self::Avif,
            "gif" => Self::Gif,
            "bmp" => Self::Bmp,
            "tif" | "tiff" => Self::Tiff,
            "ico" => Self::Ico,
            _ => return None,
        })
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        Self::from_extension(path.extension()?.to_str()?)
    }
}

/// Extensions we can read (the last group decodes through FFmpeg when it's available).
pub const INPUT_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "jfif", "webp", "avif", "gif", "bmp", "tif", "tiff", "ico", "heic", "heif", "jxl"];

pub fn is_image(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| INPUT_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageOptions {
    /// Target format; `None` keeps the input's format (compress/resize only).
    pub format: Option<ImageFormat>,
    /// 1..=100, used by JPEG, lossy WebP and AVIF.
    pub quality: u8,
    /// WebP only: encode losslessly instead of using `quality`.
    pub lossless: bool,
    /// Fit inside this box, keeping aspect ratio. Never upscales.
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    /// Alternative to the box: scale to this percentage of the original.
    pub scale_percent: Option<u32>,
    /// Run PNG output through oxipng for smaller files.
    pub optimize_png: bool,
    /// Appended to the output file name, before the extension.
    pub suffix: String,
    pub overwrite: bool,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            format: None,
            quality: 82,
            lossless: false,
            max_width: None,
            max_height: None,
            scale_percent: None,
            optimize_png: true,
            suffix: String::new(),
            overwrite: false,
        }
    }
}

/// Decodes an image, honouring EXIF orientation. Formats the `image` crate can't read
/// (HEIC, JPEG XL, some AVIF) fall back to FFmpeg.
pub fn load_image(path: &Path) -> Result<DynamicImage> {
    match load_native(path) {
        Ok(img) => Ok(img),
        Err(native_err) => load_via_ffmpeg(path).map_err(|ff_err| anyhow!("could not decode {}: {native_err:#}; FFmpeg fallback: {ff_err:#}", path.display())),
    }
}

fn load_native(path: &Path) -> Result<DynamicImage> {
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder)?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

fn load_via_ffmpeg(path: &Path) -> Result<DynamicImage> {
    let ffmpeg = find_tool("ffmpeg").ok_or_else(|| anyhow!("FFmpeg not found"))?;
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-v", "error", "-i"]).arg(path).args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"]);
    crate::media::hide_console(&mut cmd);
    let out = cmd.output()?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Png)?)
}

/// Applies the resize options. Returns the image untouched when no resize applies.
pub fn resize(img: DynamicImage, opts: &ImageOptions) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    let (mut tw, mut th) = (w, h);
    if let Some(pct) = opts.scale_percent.filter(|p| *p > 0 && *p != 100) {
        tw = ((w as u64 * pct as u64) / 100).max(1) as u32;
        th = ((h as u64 * pct as u64) / 100).max(1) as u32;
    }
    let max_w = opts.max_width.filter(|v| *v > 0).unwrap_or(u32::MAX);
    let max_h = opts.max_height.filter(|v| *v > 0).unwrap_or(u32::MAX);
    if tw > max_w || th > max_h {
        let ratio = (max_w as f64 / tw as f64).min(max_h as f64 / th as f64);
        tw = ((tw as f64 * ratio).round() as u32).max(1);
        th = ((th as f64 * ratio).round() as u32).max(1);
    }
    if (tw, th) == (w, h) || tw > w || th > h {
        return img;
    }
    img.resize_exact(tw, th, FilterType::Lanczos3)
}

/// Encodes `img` as `format`. Formats that lack alpha get it flattened onto white.
pub fn encode(img: &DynamicImage, format: ImageFormat, opts: &ImageOptions) -> Result<Vec<u8>> {
    let quality = opts.quality.clamp(1, 100);
    let mut buf = Vec::new();
    match format {
        ImageFormat::Jpeg => {
            let rgb = flatten_alpha(img);
            JpegEncoder::new_with_quality(&mut buf, quality).write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)?;
        }
        ImageFormat::Png => {
            let enc = PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilter::Adaptive);
            img.write_with_encoder(enc)?;
            if opts.optimize_png {
                let mut o = oxipng::Options::from_preset(2);
                o.strip = oxipng::StripChunks::Safe;
                if let Ok(smaller) = oxipng::optimize_from_memory(&buf, &o)
                    && smaller.len() < buf.len()
                {
                    buf = smaller;
                }
            }
        }
        ImageFormat::Webp => {
            let rgba = img.to_rgba8();
            let enc = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height());
            let mem = if opts.lossless { enc.encode_lossless() } else { enc.encode(quality as f32) };
            buf = mem.to_vec();
        }
        ImageFormat::Avif => {
            // Speed 6 is a sensible quality/time trade-off for batch work.
            let enc = AvifEncoder::new_with_speed_quality(&mut buf, 6, quality);
            if img.color().has_alpha() {
                img.to_rgba8().write_with_encoder(enc)?;
            } else {
                img.to_rgb8().write_with_encoder(enc)?;
            }
        }
        ImageFormat::Ico => {
            let img = if img.width() > 256 || img.height() > 256 { img.resize(256, 256, FilterType::Lanczos3) } else { img.clone() };
            img.to_rgba8().write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Ico)?;
        }
        ImageFormat::Gif => img.to_rgba8().write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Gif)?,
        ImageFormat::Bmp => img.to_rgba8().write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Bmp)?,
        ImageFormat::Tiff => img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Tiff)?,
    }
    Ok(buf)
}

fn flatten_alpha(img: &DynamicImage) -> image::RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }
    let rgba = img.to_rgba8();
    let mut out = image::RgbImage::new(rgba.width(), rgba.height());
    for (dst, src) in out.pixels_mut().zip(rgba.pixels()) {
        let a = src[3] as u32;
        for c in 0..3 {
            dst[c] = ((src[c] as u32 * a + 255 * (255 - a)) / 255) as u8;
        }
    }
    out
}

fn target_format(input: &Path, opts: &ImageOptions) -> ImageFormat {
    opts.format.or_else(|| ImageFormat::from_path(input)).unwrap_or(ImageFormat::Png)
}

pub struct Converted {
    pub bytes: Vec<u8>,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub original_width: u32,
    pub original_height: u32,
}

pub fn convert_in_memory(input: &Path, opts: &ImageOptions) -> Result<Converted> {
    let img = load_image(input)?;
    let (original_width, original_height) = (img.width(), img.height());
    let img = resize(img, opts);
    let format = target_format(input, opts);
    let bytes = encode(&img, format, opts).with_context(|| format!("encoding {}", format.extension()))?;
    Ok(Converted { bytes, format, width: img.width(), height: img.height(), original_width, original_height })
}

/// Converts one file and writes the result. Returns the output path.
pub fn convert_file(input: &Path, output_dir: Option<&Path>, opts: &ImageOptions) -> Result<PathBuf> {
    let converted = convert_in_memory(input, opts)?;
    let dir = util::output_dir_for(input, output_dir);
    std::fs::create_dir_all(&dir)?;
    let out = util::output_path(input, &dir, &opts.suffix, converted.format.extension(), opts.overwrite);
    std::fs::write(&out, &converted.bytes)?;
    Ok(out)
}

/// Before/after images for the preview, plus the numbers that matter.
pub struct ImagePreview {
    pub original_bytes: u64,
    pub output_bytes: u64,
    pub original_width: u32,
    pub original_height: u32,
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
    /// The original, scaled down to at most [`PREVIEW_MAX_SIDE`].
    pub original: image::RgbaImage,
    /// The encoded result decoded back, so compression artefacts are visible.
    /// `None` if this build can't decode the output format.
    pub output: Option<image::RgbaImage>,
}

pub const PREVIEW_MAX_SIDE: u32 = 2048;

/// Runs the conversion in memory without writing anything, for the before/after view.
pub fn preview(input: &Path, opts: &ImageOptions) -> Result<ImagePreview> {
    let original_bytes = std::fs::metadata(input)?.len();
    let original_img = load_image(input)?;
    let (original_width, original_height) = (original_img.width(), original_img.height());
    let resized = resize(original_img.clone(), opts);
    let format = target_format(input, opts);
    let bytes = encode(&resized, format, opts)?;
    let output = decode_bytes(&bytes, format).map(|img| fit(&img).to_rgba8());
    Ok(ImagePreview {
        original_bytes,
        output_bytes: bytes.len() as u64,
        original_width,
        original_height,
        width: resized.width(),
        height: resized.height(),
        format,
        original: fit(&original_img).to_rgba8(),
        output,
    })
}

fn fit(img: &DynamicImage) -> DynamicImage {
    if img.width() > PREVIEW_MAX_SIDE || img.height() > PREVIEW_MAX_SIDE {
        img.resize(PREVIEW_MAX_SIDE, PREVIEW_MAX_SIDE, FilterType::Triangle)
    } else {
        img.clone()
    }
}

/// Decodes encoded bytes; formats the image crate can't read go through FFmpeg via a temp file.
pub fn decode_bytes(bytes: &[u8], format: ImageFormat) -> Option<DynamicImage> {
    if let Ok(img) = image::load_from_memory(bytes) {
        return Some(img);
    }
    let dir = tempfile::tempdir().ok()?;
    let p = dir.path().join(format!("preview.{}", format.extension()));
    std::fs::write(&p, bytes).ok()?;
    load_via_ffmpeg(&p).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(dir: &Path) -> PathBuf {
        let mut img = image::RgbaImage::new(400, 300);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, if x < 20 { 0 } else { 255 }]);
        }
        let path = dir.join("sample.png");
        img.save(&path).unwrap();
        path
    }

    #[test]
    fn converts_to_every_output_format() {
        let dir = tempfile::tempdir().unwrap();
        let input = sample(dir.path());
        for fmt in
            [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Webp, ImageFormat::Avif, ImageFormat::Gif, ImageFormat::Bmp, ImageFormat::Tiff, ImageFormat::Ico]
        {
            let opts = ImageOptions { format: Some(fmt), quality: 60, ..Default::default() };
            let out = convert_file(&input, None, &opts).unwrap();
            assert_eq!(out.extension().unwrap(), fmt.extension());
            assert!(std::fs::metadata(&out).unwrap().len() > 0, "{fmt:?} wrote an empty file");
            if fmt != ImageFormat::Avif {
                // AVIF decoding needs dav1d; the rest must round-trip.
                let back = image::open(&out).unwrap();
                assert!(back.width() > 0);
            }
        }
    }

    #[test]
    fn resize_fits_box_and_never_upscales() {
        let img = DynamicImage::new_rgb8(400, 300);
        let opts = ImageOptions { max_width: Some(200), ..Default::default() };
        let r = resize(img.clone(), &opts);
        assert_eq!((r.width(), r.height()), (200, 150));
        let opts = ImageOptions { max_width: Some(1000), max_height: Some(1000), ..Default::default() };
        assert_eq!(resize(img.clone(), &opts).width(), 400);
        let opts = ImageOptions { scale_percent: Some(50), ..Default::default() };
        assert_eq!(resize(img, &opts).height(), 150);
    }

    #[test]
    fn same_format_output_does_not_overwrite_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = sample(dir.path());
        let before = std::fs::read(&input).unwrap();
        let out = convert_file(&input, None, &ImageOptions { overwrite: true, ..Default::default() }).unwrap();
        assert_ne!(out, input);
        assert_eq!(std::fs::read(&input).unwrap(), before);
    }

    #[test]
    fn preview_reports_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let input = sample(dir.path());
        let p = preview(&input, &ImageOptions { format: Some(ImageFormat::Jpeg), ..Default::default() }).unwrap();
        assert!(p.output.is_some());
        assert_eq!((p.width, p.height), (400, 300));
        assert!(p.output_bytes > 0);
    }
}
