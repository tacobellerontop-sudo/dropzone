//! Audio and video conversion by driving an FFmpeg executable (bundled with the
//! installer, or found on PATH).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::util::{self, Cancel, Progress, find_tool};

pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mkv", "mov", "avi", "webm", "wmv", "flv", "mpg", "mpeg", "ts", "mts", "m2ts", "3gp", "ogv"];
pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "m4a", "aac", "wav", "flac", "ogg", "oga", "opus", "wma", "aiff", "aif", "alac", "amr", "ac3"];

fn has_ext(path: &Path, list: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| list.contains(&e.to_ascii_lowercase().as_str()))
}
pub fn is_video(path: &Path) -> bool {
    has_ext(path, VIDEO_EXTENSIONS)
}
pub fn is_audio(path: &Path) -> bool {
    has_ext(path, AUDIO_EXTENSIONS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaTarget {
    Mp4,
    Webm,
    Mkv,
    Mov,
    Gif,
    Mp3,
    M4a,
    Opus,
    Ogg,
    Flac,
    Wav,
}

impl MediaTarget {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Webm => "webm",
            Self::Mkv => "mkv",
            Self::Mov => "mov",
            Self::Gif => "gif",
            Self::Mp3 => "mp3",
            Self::M4a => "m4a",
            Self::Opus => "opus",
            Self::Ogg => "ogg",
            Self::Flac => "flac",
            Self::Wav => "wav",
        }
    }
    pub fn is_audio_only(self) -> bool {
        matches!(self, Self::Mp3 | Self::M4a | Self::Opus | Self::Ogg | Self::Flac | Self::Wav)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaOptions {
    pub target: MediaTarget,
    /// 1..=100, mapped onto the codec's CRF scale. Higher is better and bigger.
    pub quality: u8,
    /// Downscale video to at most this height (keeps aspect ratio).
    pub max_height: Option<u32>,
    pub fps: Option<u32>,
    pub audio_bitrate_kbps: u32,
    pub remove_audio: bool,
    /// Seconds. Trims the output to this window when set.
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub suffix: String,
    pub overwrite: bool,
}

impl Default for MediaOptions {
    fn default() -> Self {
        Self {
            target: MediaTarget::Mp4,
            quality: 70,
            max_height: None,
            fps: None,
            audio_bitrate_kbps: 192,
            remove_audio: false,
            start: None,
            end: None,
            suffix: String::new(),
            overwrite: false,
        }
    }
}

pub fn ffmpeg_path() -> Result<PathBuf> {
    find_tool("ffmpeg").ok_or_else(|| anyhow!("FFmpeg was not found. Reinstall Dropzone or put ffmpeg on your PATH."))
}

/// Keeps FFmpeg from flashing a console window on Windows.
pub fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// FFmpeg arguments between the input and the output path.
pub fn build_args(opts: &MediaOptions) -> Vec<String> {
    let q = opts.quality.clamp(1, 100) as f32;
    let x264_crf = (51.0 - q * 0.37).round() as u32; // 70 → 25, 100 → 14
    let vp9_crf = (63.0 - q * 0.45).round() as u32; // 70 → 31
    let ab = format!("{}k", opts.audio_bitrate_kbps.clamp(32, 512));
    let mut args: Vec<String> = Vec::new();
    let mut push = |a: &[&str]| args.extend(a.iter().map(|s| s.to_string()));

    let mut filters: Vec<String> = Vec::new();
    if let Some(fps) = opts.fps.filter(|f| *f > 0) {
        filters.push(format!("fps={fps}"));
    }
    if let Some(h) = opts.max_height.filter(|h| *h > 0) {
        // -2 keeps the width even, which most encoders require.
        filters.push(format!("scale=-2:'min({h},ih)'"));
    }

    match opts.target {
        MediaTarget::Gif => {
            let mut chain = if filters.is_empty() { vec!["fps=12".to_string()] } else { filters.clone() };
            chain.push("split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4".into());
            push(&["-filter_complex", &chain.join(","), "-loop", "0"]);
            return args;
        }
        t if t.is_audio_only() => {
            push(&["-vn"]);
            match t {
                MediaTarget::Mp3 => push(&["-c:a", "libmp3lame", "-b:a", &ab]),
                MediaTarget::M4a => push(&["-c:a", "aac", "-b:a", &ab]),
                MediaTarget::Opus => push(&["-c:a", "libopus", "-b:a", &ab]),
                MediaTarget::Ogg => push(&["-c:a", "libvorbis", "-b:a", &ab]),
                MediaTarget::Flac => push(&["-c:a", "flac"]),
                MediaTarget::Wav => push(&["-c:a", "pcm_s16le"]),
                _ => unreachable!(),
            }
            return args;
        }
        _ => {}
    }

    if !filters.is_empty() {
        push(&["-vf", &filters.join(",")]);
    }
    match opts.target {
        MediaTarget::Webm => {
            push(&["-c:v", "libvpx-vp9", "-crf", &vp9_crf.to_string(), "-b:v", "0", "-row-mt", "1"]);
            if !opts.remove_audio {
                push(&["-c:a", "libopus", "-b:a", &ab]);
            }
        }
        _ => {
            push(&["-c:v", "libx264", "-preset", "medium", "-crf", &x264_crf.to_string(), "-pix_fmt", "yuv420p"]);
            if !opts.remove_audio {
                push(&["-c:a", "aac", "-b:a", &ab]);
            }
            if matches!(opts.target, MediaTarget::Mp4 | MediaTarget::Mov) {
                push(&["-movflags", "+faststart"]);
            }
        }
    }
    if opts.remove_audio {
        push(&["-an"]);
    }
    args
}

/// Converts `input`, reporting progress from FFmpeg's `-progress` stream.
pub fn convert(input: &Path, output_dir: Option<&Path>, opts: &MediaOptions, progress: &dyn Progress, cancel: &Cancel) -> Result<PathBuf> {
    let ffmpeg = ffmpeg_path()?;
    let dir = util::output_dir_for(input, output_dir);
    std::fs::create_dir_all(&dir)?;
    let out = util::output_path(input, &dir, &opts.suffix, opts.target.extension(), opts.overwrite);

    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-nostdin", "-y", "-nostats", "-progress", "pipe:1"]);
    if let Some(s) = opts.start.filter(|s| *s > 0.0) {
        cmd.args(["-ss", &format!("{s:.3}")]);
    }
    cmd.arg("-i").arg(input);
    if let Some(e) = opts.end {
        let span = e - opts.start.unwrap_or(0.0);
        if span > 0.0 {
            cmd.args(["-t", &format!("{span:.3}")]);
        }
    }
    cmd.args(build_args(opts)).arg(&out);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    hide_console(&mut cmd);

    let mut child = cmd.spawn()?;
    let duration_us = Arc::new(AtomicU64::new(0));
    let stderr_tail = Arc::new(Mutex::new(Vec::<String>::new()));

    let err_thread = {
        let stderr = child.stderr.take().unwrap();
        let duration_us = duration_us.clone();
        let tail = stderr_tail.clone();
        let window = match (opts.start, opts.end) {
            (s, Some(e)) if e > s.unwrap_or(0.0) => Some(((e - s.unwrap_or(0.0)) * 1e6) as u64),
            _ => None,
        };
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if duration_us.load(Ordering::Relaxed) == 0
                    && let Some(d) = parse_duration_line(&line)
                {
                    let mut d = (d * 1e6) as u64;
                    if let Some(w) = window {
                        d = d.min(w);
                    }
                    duration_us.store(d.max(1), Ordering::Relaxed);
                }
                let mut t = tail.lock().unwrap();
                t.push(line);
                if t.len() > 20 {
                    t.remove(0);
                }
            }
        })
    };

    let (tx, rx) = std::sync::mpsc::channel::<u64>();
    let out_thread = {
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(v) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms="))
                    && let Ok(us) = v.trim().parse::<u64>()
                {
                    let _ = tx.send(us);
                }
            }
        })
    };

    let status = loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&out);
            bail!("cancelled");
        }
        while let Ok(us) = rx.try_recv() {
            let total = duration_us.load(Ordering::Relaxed);
            if total > 0 {
                progress.report((us as f32 / total as f32).min(0.99), "encoding");
            }
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(150));
    };
    let _ = out_thread.join();
    let _ = err_thread.join();

    if !status.success() {
        let _ = std::fs::remove_file(&out);
        let tail = stderr_tail.lock().unwrap();
        let msg = tail.iter().rev().take(4).rev().cloned().collect::<Vec<_>>().join("\n");
        bail!("FFmpeg failed: {msg}");
    }
    progress.report(1.0, "done");
    Ok(out)
}

fn parse_duration_line(line: &str) -> Option<f64> {
    let rest = line.trim_start().strip_prefix("Duration:")?;
    parse_timestamp(rest.trim().split(',').next()?)
}

fn parse_timestamp(s: &str) -> Option<f64> {
    let mut parts = s.trim().split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MediaInfo {
    pub duration_seconds: Option<f64>,
    pub streams: Vec<String>,
}

/// Lightweight probe using `ffmpeg -i`, so we don't need to ship ffprobe too.
pub fn probe(input: &Path) -> Result<MediaInfo> {
    let mut cmd = Command::new(ffmpeg_path()?);
    cmd.args(["-hide_banner", "-nostdin", "-i"]).arg(input);
    hide_console(&mut cmd);
    let out = cmd.output()?;
    let text = String::from_utf8_lossy(&out.stderr);
    let mut info = MediaInfo::default();
    for line in text.lines() {
        if info.duration_seconds.is_none() {
            info.duration_seconds = parse_duration_line(line);
        }
        let t = line.trim();
        if t.starts_with("Stream #") {
            info.streams.push(t.split_once(": ").map(|x| x.1).unwrap_or(t).to_string());
        }
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duration() {
        assert_eq!(parse_duration_line("  Duration: 00:01:02.50, start: 0.0, bitrate: 1 kb/s"), Some(62.5));
        assert_eq!(parse_duration_line("Duration: N/A, bitrate: N/A"), None);
    }

    #[test]
    fn audio_targets_drop_video() {
        let args = build_args(&MediaOptions { target: MediaTarget::Mp3, ..Default::default() });
        assert_eq!(args[0], "-vn");
        assert!(args.contains(&"libmp3lame".to_string()));
    }

    #[test]
    fn converts_with_real_ffmpeg_when_available() {
        let Some(ffmpeg) = find_tool("ffmpeg") else { return };
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.mp4");
        let ok = Command::new(ffmpeg)
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc=duration=2:size=320x240:rate=10", "-f", "lavfi", "-i"])
            .args(["sine=duration=2", "-shortest", "-c:v", "libx264", "-c:a", "aac"])
            .arg(&input)
            .status()
            .unwrap()
            .success();
        if !ok {
            return; // this FFmpeg build lacks x264; nothing to test against
        }
        let last = Mutex::new(0.0f32);
        let progress = |f: f32, _: &str| *last.lock().unwrap() = f;
        for target in [MediaTarget::Webm, MediaTarget::Mp3, MediaTarget::Gif, MediaTarget::Wav] {
            let opts = MediaOptions { target, max_height: Some(120), ..Default::default() };
            let out = convert(&input, None, &opts, &progress, &Cancel::new()).unwrap();
            assert!(std::fs::metadata(&out).unwrap().len() > 0, "{target:?}");
        }
        assert_eq!(*last.lock().unwrap(), 1.0);
        let info = probe(&input).unwrap();
        assert!(info.duration_seconds.unwrap() > 1.5);
        assert_eq!(info.streams.len(), 2);
    }
}
