//! Tools panel: conversion, PDF, archive and checksum operations on the selection.

use std::path::PathBuf;

use dropzone_core::archive::{CreateFormat, CreateOptions, ExtractOptions};
use dropzone_core::checksum::Algo;
use dropzone_core::image_ops::{self, ImageFormat, ImageOptions, ImagePreview};
use dropzone_core::media::{self, MediaInfo, MediaOptions, MediaTarget};
use dropzone_core::ops::Operation;
use dropzone_core::pdf::{self, PdfImageOptions, PdfInfo, PdfPageOptions};
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};

use crate::common::{self, Kind, Task};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OpKind {
    #[default]
    Image,
    Media,
    PdfPages,
    PdfImages,
    Extract,
    Verify,
    Create,
    Checksum,
    VerifyChecksums,
}

impl OpKind {
    pub const ALL: [OpKind; 9] = [
        OpKind::Image,
        OpKind::Media,
        OpKind::PdfPages,
        OpKind::PdfImages,
        OpKind::Extract,
        OpKind::Verify,
        OpKind::Create,
        OpKind::Checksum,
        OpKind::VerifyChecksums,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OpKind::Image => "Convert / compress images",
            OpKind::Media => "Convert audio & video",
            OpKind::PdfPages => "PDF: extract or split pages",
            OpKind::PdfImages => "PDF: extract images",
            OpKind::Extract => "Extract archives",
            OpKind::Verify => "Test archives",
            OpKind::Create => "Create archive",
            OpKind::Checksum => "Compute checksums",
            OpKind::VerifyChecksums => "Verify checksum list",
        }
    }

    pub fn applies(self, kind: Kind) -> bool {
        match self {
            OpKind::Image => kind == Kind::Image,
            OpKind::Media => matches!(kind, Kind::Video | Kind::Audio),
            OpKind::PdfPages | OpKind::PdfImages => kind == Kind::Pdf,
            OpKind::Extract | OpKind::Verify => kind == Kind::Archive,
            OpKind::Create | OpKind::Checksum => kind != Kind::Folder,
            OpKind::VerifyChecksums => kind == Kind::ChecksumList,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ResizeMode {
    #[default]
    None,
    Fit,
    Percent,
}

/// Everything on this tab worth remembering between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConvertSettings {
    pub op: OpKind,
    pub image: ImageOptions,
    pub resize_mode: ResizeMode,
    pub fit_w: u32,
    pub fit_h: u32,
    pub percent: u32,
    pub media: MediaOptions,
    pub trim_start: String,
    pub trim_end: String,
    pub pdf_pages: PdfPageOptions,
    pub pdf_images: PdfImageOptions,
    pub extract: ExtractOptions,
    pub verify_recursive: bool,
    pub create: CreateOptions,
    pub algos: Vec<Algo>,
    pub output_dir: Option<PathBuf>,
}

impl Default for ConvertSettings {
    fn default() -> Self {
        Self {
            op: OpKind::Image,
            image: ImageOptions { format: Some(ImageFormat::Webp), ..Default::default() },
            resize_mode: ResizeMode::None,
            fit_w: 1920,
            fit_h: 1920,
            percent: 50,
            media: MediaOptions::default(),
            trim_start: String::new(),
            trim_end: String::new(),
            pdf_pages: PdfPageOptions::default(),
            pdf_images: PdfImageOptions::default(),
            extract: ExtractOptions::default(),
            verify_recursive: true,
            create: CreateOptions::default(),
            algos: vec![Algo::Crc32, Algo::Md5, Algo::Sha256],
            output_dir: None,
        }
    }
}

enum PreviewResult {
    Image { preview: ImagePreview, original: egui::TextureHandle, output: Option<egui::TextureHandle> },
    Media(MediaInfo),
    Pdf { info: PdfInfo, pages: Result<Vec<u32>, String> },
}

enum PreviewData {
    Image(ImagePreview),
    Media(MediaInfo),
    Pdf(PdfInfo),
}

/// One input to a tool: the file, its kind, and the dropped/selected folder it came from.
#[derive(Clone)]
pub struct Input {
    pub path: PathBuf,
    pub kind: Kind,
    pub root: Option<PathBuf>,
}

/// What the tool panel asks the app to do.
pub enum Request {
    Enqueue(Vec<PathBuf>, Operation, Option<PathBuf>),
    OpenArchive(PathBuf),
    Close,
}

pub struct ToolPanel {
    pub settings: ConvertSettings,
    preview_task: Task<PreviewData>,
    preview: Option<(PathBuf, PreviewResult)>,
    preview_open: bool,
    actual_pixels: bool,
    password: String,
}

impl ToolPanel {
    pub fn new(mut settings: ConvertSettings, op: OpKind) -> Self {
        settings.op = op;
        Self { settings, preview_task: Task::default(), preview: None, preview_open: false, actual_pixels: false, password: String::new() }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, inputs: &[Input], scope: &str, has_ffmpeg: bool) -> Option<Request> {
        self.poll_preview(ui.ctx());
        let mut request = None;
        let op = self.settings.op;
        let applicable: Vec<&Input> = inputs.iter().filter(|i| op.applies(i.kind)).collect();

        ui.horizontal(|ui| {
            ui.label(RichText::new("Tools").strong().size(16.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new("🗙").frame(false)).on_hover_text("Close").clicked() {
                    request = Some(Request::Close);
                }
            });
        });
        egui::ComboBox::from_id_salt("tool-op").width(ui.available_width()).selected_text(op.label()).show_ui(ui, |ui| {
            for o in OpKind::ALL {
                let n = inputs.iter().filter(|i| o.applies(i.kind)).count();
                ui.selectable_value(&mut self.settings.op, o, format!("{}  ({n})", o.label()));
            }
        });
        let n = applicable.len();
        let what = if n == 0 {
            RichText::new(format!("Nothing in {scope} fits this tool. Select files in the list.")).small().color(common::WARN)
        } else {
            RichText::new(format!("{n} of {scope}")).small().weak()
        };
        ui.label(what);
        ui.separator();

        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(ui.available_height() - 50.0).show(ui, |ui| {
            match self.settings.op {
                OpKind::Image => self.image_options(ui),
                OpKind::Media => self.media_options(ui, has_ffmpeg),
                OpKind::PdfPages => {
                    labeled(ui, "Pages", |ui| ui.add(egui::TextEdit::singleline(&mut self.settings.pdf_pages.ranges).hint_text("all, or e.g. 1-3, 5, 9-")));
                    ui.checkbox(&mut self.settings.pdf_pages.split, "One PDF per page");
                }
                OpKind::PdfImages => {
                    labeled(ui, "Pages", |ui| ui.add(egui::TextEdit::singleline(&mut self.settings.pdf_images.ranges).hint_text("all, or e.g. 1-3, 5")));
                    ui.label(RichText::new("JPEGs are saved untouched; other images become PNG.").small().weak());
                }
                OpKind::Extract => {
                    password_field(ui, &mut self.password);
                    ui.checkbox(&mut self.settings.extract.into_subfolder, "Into a folder named after the archive");
                    ui.checkbox(&mut self.settings.extract.flatten, "Ignore folders inside the archive");
                    ui.checkbox(&mut self.settings.extract.overwrite, "Overwrite existing files");
                }
                OpKind::Verify => {
                    password_field(ui, &mut self.password);
                    ui.checkbox(&mut self.settings.verify_recursive, "Also test archives inside archives");
                    ui.label(RichText::new("Decompresses everything in memory and checks every CRC. Nothing is written.").small().weak());
                }
                OpKind::Create => {
                    let c = &mut self.settings.create;
                    labeled(ui, "Format", |ui| {
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut c.format, CreateFormat::Zip, "ZIP");
                            ui.radio_value(&mut c.format, CreateFormat::SevenZ, "7z");
                        })
                        .response
                    });
                    if c.format == CreateFormat::Zip {
                        labeled(ui, "Compression level", |ui| ui.add(egui::Slider::new(&mut c.level, 0..=9)));
                    }
                    let mut name = c.name.clone().unwrap_or_default();
                    labeled(ui, "Archive name", |ui| ui.add(egui::TextEdit::singleline(&mut name).hint_text("automatic")));
                    c.name = Some(name).filter(|n| !n.trim().is_empty());
                    password_field(ui, &mut self.password);
                    ui.label(RichText::new("Everything goes into one archive. Folders keep their structure.").small().weak());
                }
                OpKind::Checksum => {
                    ui.horizontal_wrapped(|ui| {
                        for (algo, name) in [(Algo::Crc32, "CRC32"), (Algo::Md5, "MD5"), (Algo::Sha1, "SHA-1"), (Algo::Sha256, "SHA-256")] {
                            let mut on = self.settings.algos.contains(&algo);
                            if ui.checkbox(&mut on, name).changed() {
                                self.settings.algos.retain(|a| *a != algo);
                                if on {
                                    self.settings.algos.push(algo);
                                }
                            }
                        }
                    });
                    ui.label(RichText::new("Results show in the queue; nothing is written.").small().weak());
                }
                OpKind::VerifyChecksums => {
                    ui.label(RichText::new("Select a .sha256, .md5, .sfv or SHA256SUMS file. Every file it lists is hashed and compared.").small().weak());
                }
            }

            ui.add_space(10.0);
            let writes_files = !matches!(self.settings.op, OpKind::Verify | OpKind::Checksum | OpKind::VerifyChecksums);
            if writes_files {
                ui.label(RichText::new("SAVE TO").small().strong().weak());
                let mut custom = self.settings.output_dir.is_some();
                ui.radio_value(&mut custom, false, "Same folder as the originals");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut custom, true, "Another folder");
                    if ui.small_button("Choose…").clicked()
                        && let Some(d) = common::pick_folder("Save results to")
                    {
                        self.settings.output_dir = Some(d);
                        custom = true;
                    }
                });
                if !custom {
                    self.settings.output_dir = None;
                } else if self.settings.output_dir.is_none() {
                    self.settings.output_dir = common::pick_folder("Save results to");
                }
                if let Some(d) = &self.settings.output_dir {
                    ui.label(RichText::new(d.display().to_string()).small().monospace());
                }
            }
        });

        ui.separator();
        let media_blocked = self.settings.op == OpKind::Media && !has_ffmpeg;
        ui.horizontal(|ui| {
            let can_preview =
                n > 0 && matches!(self.settings.op, OpKind::Image | OpKind::Media | OpKind::PdfPages | OpKind::PdfImages | OpKind::Extract | OpKind::Verify);
            if ui.add_enabled(can_preview && !self.preview_task.running(), egui::Button::new("Preview")).clicked()
                && let Some(first) = applicable.first()
                && let Some(r) = self.start_preview(ui.ctx(), first.path.clone())
            {
                request = Some(r);
            }
            if self.preview_task.running() {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = match (self.settings.op, n) {
                    (OpKind::Create, _) => "Create archive".to_string(),
                    (OpKind::Verify, _) => "Test".to_string(),
                    (_, 1) => "Run".to_string(),
                    (_, n) => format!("Run on {n} files"),
                };
                let btn = egui::Button::new(RichText::new(label).strong().color(egui::Color32::WHITE)).fill(ui.visuals().selection.bg_fill);
                if ui.add_enabled(n > 0 && !media_blocked, btn).clicked() {
                    let inputs: Vec<PathBuf> = if self.settings.op == OpKind::Create {
                        let mut seen = std::collections::HashSet::new();
                        applicable.iter().map(|f| f.root.clone().unwrap_or_else(|| f.path.clone())).filter(|p| seen.insert(p.clone())).collect()
                    } else {
                        applicable.iter().map(|f| f.path.clone()).collect()
                    };
                    request = Some(Request::Enqueue(inputs, self.operation(), self.settings.output_dir.clone()));
                }
            });
        });
        if media_blocked {
            ui.label(RichText::new("FFmpeg is needed for audio and video.").small().color(common::WARN));
        }
        self.preview_window(ui.ctx());
        request
    }

    fn image_options(&mut self, ui: &mut egui::Ui) {
        let s = &mut self.settings;
        labeled(ui, "Format", |ui| {
            egui::ComboBox::from_id_salt("img-format")
                .width(ui.available_width())
                .selected_text(match s.image.format {
                    None => "Keep original format".to_string(),
                    Some(f) => f.extension().to_uppercase(),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.image.format, None, "Keep original format");
                    for f in [
                        ImageFormat::Webp,
                        ImageFormat::Avif,
                        ImageFormat::Jpeg,
                        ImageFormat::Png,
                        ImageFormat::Gif,
                        ImageFormat::Bmp,
                        ImageFormat::Tiff,
                        ImageFormat::Ico,
                    ] {
                        ui.selectable_value(&mut s.image.format, Some(f), f.extension().to_uppercase());
                    }
                })
                .response
        });
        let lossy =
            matches!(s.image.format, Some(ImageFormat::Jpeg | ImageFormat::Avif) | None) || (s.image.format == Some(ImageFormat::Webp) && !s.image.lossless);
        if s.image.format == Some(ImageFormat::Webp) {
            ui.checkbox(&mut s.image.lossless, "Lossless");
        }
        if lossy {
            labeled(ui, "Quality", |ui| ui.add(egui::Slider::new(&mut s.image.quality, 1..=100)));
        }
        if matches!(s.image.format, Some(ImageFormat::Png) | None) {
            ui.checkbox(&mut s.image.optimize_png, "Optimize PNG (smaller, slower)");
        }
        labeled(ui, "Resize", |ui| {
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.resize_mode, ResizeMode::None, "No");
                ui.radio_value(&mut s.resize_mode, ResizeMode::Fit, "Fit in box");
                ui.radio_value(&mut s.resize_mode, ResizeMode::Percent, "Percent");
            })
            .response
        });
        match s.resize_mode {
            ResizeMode::Fit => {
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut s.fit_w).range(1..=20000).suffix(" px"));
                    ui.label("×");
                    ui.add(egui::DragValue::new(&mut s.fit_h).range(1..=20000).suffix(" px"));
                    ui.label(RichText::new("never upscales").small().weak());
                });
            }
            ResizeMode::Percent => {
                ui.add(egui::Slider::new(&mut s.percent, 1..=100).suffix("%"));
            }
            ResizeMode::None => {}
        }
        labeled(ui, "Add to file name", |ui| ui.add(egui::TextEdit::singleline(&mut s.image.suffix).hint_text("e.g. -small")));
        ui.checkbox(&mut s.image.overwrite, "Overwrite existing output files");
    }

    fn media_options(&mut self, ui: &mut egui::Ui, has_ffmpeg: bool) {
        let s = &mut self.settings;
        if !has_ffmpeg {
            ui.label(RichText::new("FFmpeg wasn't found. Put ffmpeg.exe next to dropzone.exe or on your PATH.").color(common::WARN));
        }
        labeled(ui, "Convert to", |ui| {
            egui::ComboBox::from_id_salt("media-target")
                .width(ui.available_width())
                .selected_text(target_label(s.media.target))
                .show_ui(ui, |ui| {
                    ui.label(RichText::new("Video").small().weak());
                    for t in [MediaTarget::Mp4, MediaTarget::Webm, MediaTarget::Mkv, MediaTarget::Mov, MediaTarget::Gif] {
                        ui.selectable_value(&mut s.media.target, t, target_label(t));
                    }
                    ui.label(RichText::new("Audio only").small().weak());
                    for t in [MediaTarget::Mp3, MediaTarget::M4a, MediaTarget::Opus, MediaTarget::Ogg, MediaTarget::Flac, MediaTarget::Wav] {
                        ui.selectable_value(&mut s.media.target, t, target_label(t));
                    }
                })
                .response
        });
        let t = s.media.target;
        if !t.is_audio_only() {
            if t != MediaTarget::Gif {
                labeled(ui, "Video quality", |ui| ui.add(egui::Slider::new(&mut s.media.quality, 1..=100)));
            }
            labeled(ui, "Max height", |ui| {
                egui::ComboBox::from_id_salt("media-height")
                    .selected_text(s.media.max_height.map(|h| format!("{h}p")).unwrap_or_else(|| "Keep".into()))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.media.max_height, None, "Keep");
                        for h in [2160, 1440, 1080, 720, 480, 360, 240] {
                            ui.selectable_value(&mut s.media.max_height, Some(h), format!("{h}p"));
                        }
                    })
                    .response
            });
            let mut fps = s.media.fps.unwrap_or(0);
            labeled(ui, "Frame rate (0 = keep)", |ui| ui.add(egui::DragValue::new(&mut fps).range(0..=240)));
            s.media.fps = (fps > 0).then_some(fps);
        }
        if !matches!(t, MediaTarget::Flac | MediaTarget::Wav | MediaTarget::Gif) {
            if !t.is_audio_only() {
                ui.checkbox(&mut s.media.remove_audio, "Remove audio");
            }
            if !s.media.remove_audio || t.is_audio_only() {
                labeled(ui, "Audio bitrate", |ui| {
                    egui::ComboBox::from_id_salt("media-ab")
                        .selected_text(format!("{} kbps", s.media.audio_bitrate_kbps))
                        .show_ui(ui, |ui| {
                            for b in [64, 96, 128, 160, 192, 256, 320] {
                                ui.selectable_value(&mut s.media.audio_bitrate_kbps, b, format!("{b} kbps"));
                            }
                        })
                        .response
                });
            }
        }
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Trim start").small());
                ui.add(egui::TextEdit::singleline(&mut s.trim_start).desired_width(100.0).hint_text("0:00"));
            });
            ui.vertical(|ui| {
                ui.label(RichText::new("Trim end").small());
                ui.add(egui::TextEdit::singleline(&mut s.trim_end).desired_width(100.0).hint_text("end"));
            });
        });
        labeled(ui, "Add to file name", |ui| ui.add(egui::TextEdit::singleline(&mut s.media.suffix).hint_text("e.g. -720p")));
        ui.checkbox(&mut s.media.overwrite, "Overwrite existing output files");
    }

    fn image_opts(&self) -> ImageOptions {
        let s = &self.settings;
        let mut o = s.image.clone();
        o.max_width = None;
        o.max_height = None;
        o.scale_percent = None;
        match s.resize_mode {
            ResizeMode::Fit => {
                o.max_width = Some(s.fit_w);
                o.max_height = Some(s.fit_h);
            }
            ResizeMode::Percent => o.scale_percent = Some(s.percent),
            ResizeMode::None => {}
        }
        o
    }

    fn password(&self) -> Option<String> {
        Some(self.password.clone()).filter(|p| !p.is_empty())
    }

    fn operation(&self) -> Operation {
        let s = &self.settings;
        match s.op {
            OpKind::Image => Operation::ConvertImage { options: self.image_opts() },
            OpKind::Media => {
                let mut m = s.media.clone();
                m.start = common::parse_time(&s.trim_start);
                m.end = common::parse_time(&s.trim_end);
                Operation::ConvertMedia { options: m }
            }
            OpKind::PdfPages => Operation::PdfPages { options: s.pdf_pages.clone() },
            OpKind::PdfImages => Operation::PdfImages { options: s.pdf_images.clone() },
            OpKind::Extract => {
                Operation::ExtractArchive { options: ExtractOptions { password: self.password(), entries: None, ..s.extract.clone() }, inner: vec![] }
            }
            OpKind::Verify => Operation::VerifyArchive { password: self.password(), recursive: s.verify_recursive },
            OpKind::Create => Operation::CreateArchive { options: CreateOptions { password: self.password(), ..s.create.clone() } },
            OpKind::Checksum => Operation::Checksum { algos: if s.algos.is_empty() { vec![Algo::Sha256] } else { s.algos.clone() } },
            OpKind::VerifyChecksums => Operation::VerifyChecksums,
        }
    }

    fn start_preview(&mut self, ctx: &egui::Context, path: PathBuf) -> Option<Request> {
        match self.settings.op {
            OpKind::Extract | OpKind::Verify => return Some(Request::OpenArchive(path)),
            OpKind::Image => {
                let opts = self.image_opts();
                let p = path.clone();
                self.preview_task = Task::spawn(ctx, move || image_ops::preview(&p, &opts).map(PreviewData::Image));
            }
            OpKind::Media => {
                let p = path.clone();
                self.preview_task = Task::spawn(ctx, move || media::probe(&p).map(PreviewData::Media));
            }
            OpKind::PdfPages | OpKind::PdfImages => {
                let p = path.clone();
                self.preview_task = Task::spawn(ctx, move || pdf::info(&p).map(PreviewData::Pdf));
            }
            _ => return None,
        }
        self.preview = Some((path, PreviewResult::Media(MediaInfo::default())));
        self.preview_open = false;
        None
    }

    fn poll_preview(&mut self, ctx: &egui::Context) {
        let Some(result) = self.preview_task.poll() else { return };
        let path = self.preview.as_ref().map(|p| p.0.clone()).unwrap_or_default();
        match result {
            Err(e) => {
                self.preview = None;
                self.preview_open = false;
                // Shown inside the window so the user sees which file failed.
                self.preview = Some((path, PreviewResult::Media(MediaInfo { duration_seconds: None, streams: vec![format!("Error: {e}")] })));
                self.preview_open = true;
            }
            Ok(PreviewData::Image(p)) => {
                let original = common::texture_from_rgba(ctx, "preview-original", &p.original);
                let output = p.output.as_ref().map(|o| common::texture_from_rgba(ctx, "preview-output", o));
                self.preview = Some((path, PreviewResult::Image { preview: p, original, output }));
                self.preview_open = true;
            }
            Ok(PreviewData::Media(info)) => {
                self.preview = Some((path, PreviewResult::Media(info)));
                self.preview_open = true;
            }
            Ok(PreviewData::Pdf(info)) => {
                let ranges = match self.settings.op {
                    OpKind::PdfImages => &self.settings.pdf_images.ranges,
                    _ => &self.settings.pdf_pages.ranges,
                };
                let pages = pdf::parse_ranges(ranges, info.pages).map_err(|e| e.to_string());
                self.preview = Some((path, PreviewResult::Pdf { info, pages }));
                self.preview_open = true;
            }
        }
    }

    fn preview_window(&mut self, ctx: &egui::Context) {
        if !self.preview_open {
            return;
        }
        let Some((path, result)) = &self.preview else { return };
        let mut open = true;
        let op_desc = describe_op(&self.operation());
        egui::Window::new(format!("Preview: {}", common::file_name(path)))
            .open(&mut open)
            .default_size([900.0, 600.0])
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| match result {
                PreviewResult::Image { preview: p, original, output } => {
                    let delta = p.output_bytes as f64 / p.original_bytes.max(1) as f64 - 1.0;
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{} × {} to {} × {}", p.original_width, p.original_height, p.width, p.height));
                        ui.separator();
                        ui.label(format!("{} to {}", common::human_size(p.original_bytes), common::human_size(p.output_bytes)));
                        let color = if delta <= 0.0 { common::OK } else { common::ERR };
                        ui.label(RichText::new(format!("{:+.0}%", delta * 100.0)).strong().color(color));
                        ui.separator();
                        ui.label(p.format.extension().to_uppercase());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.checkbox(&mut self.actual_pixels, "Actual pixels");
                        });
                    });
                    ui.add_space(6.0);
                    let actual = self.actual_pixels;
                    ui.columns(2, |cols| {
                        cols[0].label(RichText::new("Original").weak());
                        show_image(&mut cols[0], original, actual, "orig");
                        cols[1].label(RichText::new("Result").weak());
                        match output {
                            Some(t) => show_image(&mut cols[1], t, actual, "out"),
                            None => {
                                cols[1].label("This format can't be displayed here, but the conversion works.");
                            }
                        }
                    });
                }
                PreviewResult::Media(info) => {
                    ui.label(RichText::new(op_desc.clone()).strong());
                    ui.add_space(4.0);
                    if let Some(d) = info.duration_seconds {
                        ui.label(format!("Duration: {}", fmt_duration(d)));
                    }
                    for s in &info.streams {
                        ui.label(RichText::new(s).monospace());
                    }
                }
                PreviewResult::Pdf { info, pages } => {
                    ui.label(format!("{} pages, {} embedded images, PDF {}", info.pages, info.image_count, info.version));
                    match pages {
                        Ok(p) => ui.label(format!("{op_desc}: {} page(s): {}", p.len(), summarize_pages(p))),
                        Err(e) => ui.label(RichText::new(e).color(common::ERR)),
                    };
                }
            });
        if !open {
            self.preview_open = false;
        }
    }
}

fn show_image(ui: &mut egui::Ui, tex: &egui::TextureHandle, actual: bool, id: &str) {
    if actual {
        egui::ScrollArea::both().id_salt(id).max_height(ui.available_height()).show(ui, |ui| {
            ui.image((tex.id(), tex.size_vec2()));
        });
    } else {
        let avail = egui::vec2(ui.available_width(), ui.available_height().max(200.0));
        let size = tex.size_vec2();
        let scale = (avail.x / size.x).min(avail.y / size.y).min(1.0);
        ui.image((tex.id(), size * scale));
    }
}

fn describe_op(op: &Operation) -> String {
    op.label()
}

fn summarize_pages(p: &[u32]) -> String {
    if p.len() <= 12 {
        p.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
    } else {
        format!("{} … {}", p[..6].iter().map(u32::to_string).collect::<Vec<_>>().join(", "), p[p.len() - 1])
    }
}

fn fmt_duration(s: f64) -> String {
    let t = s.round() as u64;
    if t >= 3600 { format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60) } else { format!("{}:{:02}", t / 60, t % 60) }
}

fn target_label(t: MediaTarget) -> &'static str {
    match t {
        MediaTarget::Mp4 => "MP4 (H.264 + AAC)",
        MediaTarget::Webm => "WebM (VP9 + Opus)",
        MediaTarget::Mkv => "MKV (H.264 + AAC)",
        MediaTarget::Mov => "MOV (H.264 + AAC)",
        MediaTarget::Gif => "Animated GIF",
        MediaTarget::Mp3 => "MP3",
        MediaTarget::M4a => "M4A (AAC)",
        MediaTarget::Opus => "Opus",
        MediaTarget::Ogg => "OGG (Vorbis)",
        MediaTarget::Flac => "FLAC (lossless)",
        MediaTarget::Wav => "WAV (uncompressed)",
    }
}

pub fn labeled(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> egui::Response) {
    ui.label(RichText::new(label).small());
    add(ui);
    ui.add_space(4.0);
}

fn password_field(ui: &mut egui::Ui, pw: &mut String) {
    labeled(ui, "Password (if needed)", |ui| ui.add(egui::TextEdit::singleline(pw).password(true).hint_text("none")));
}
