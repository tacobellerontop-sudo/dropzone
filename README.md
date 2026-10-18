# Dropzone

A small, fast file explorer with the tools you usually go to a website or 7-Zip for built in:
convert and compress images, convert audio and video, batch rename, split PDFs, and open,
test and extract archives without unpacking them first.

It's a single native executable (Rust + egui), around 15 MB with no runtime to install.
FFmpeg ships next to it for audio and video.

## What it does

**Explorer**
- List and thumbnail views, sortable columns, search-as-you-type filter, breadcrumb path bar
  (click it to type a path), back/forward/up, pinned folders, drives.
- Copy, cut, paste, new folder, inline rename, delete to the recycle bin.
- Drag files from anywhere onto the window to copy them into the current folder.
- A details pane with a preview of the selected file: images, text, PDF and media info,
  archive contents.

**Tools** (right-click a selection, or the Tools menu; with nothing selected they apply to the whole folder)
- Images: PNG, JPEG, WebP, AVIF, GIF, BMP, TIFF, ICO in; resize (fit in box or percent),
  quality, lossless, PNG optimisation. HEIC and JPEG XL are read through FFmpeg.
  Preview the result side by side, with the size saving, before running it.
- Audio and video: MP4, WebM, MKV, MOV, GIF, MP3, M4A, Opus, OGG, FLAC, WAV, with quality,
  resolution and trim.
- PDF: extract page ranges (as one file or one per page), extract embedded images.
- Batch rename: a pattern with `{name}`, `{n:3}`, `{date}`, `{parent}` and friends,
  find/replace (plain or regex), case changes, a live old/new table that flags conflicts,
  and undo.
- Checksums (CRC32, MD5, SHA-1, SHA-256), and verifying `.sha256`/`.md5`/`.sfv` lists.
- Compress to ZIP or 7z, with optional AES password.

**Archives**
- Double-click a ZIP, 7z, TAR (gz/xz/bz2) to browse it like a folder, including archives
  inside archives.
- Extract only the selected files, extract all, or extract here.
- Test integrity: decompresses everything, checks every CRC, recurses into nested archives.
- Compare an archive with a folder on disk: identical, different, or missing on either side.
- Password-protected ZIP and 7z.

**Queue**
- Every operation runs in a background queue with progress and cancel.
- Tick *Quit when finished* to queue a pile of work and walk away.

## Keyboard

| Key | Action |
| --- | --- |
| Enter / double-click | Open |
| Backspace, Alt+↑ | Up a folder |
| Alt+← / Alt+→ | Back / forward |
| F2 | Rename (batch rename when several are selected) |
| Del | Delete to recycle bin |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy / cut / paste |
| Ctrl+A | Select all |
| Ctrl+Shift+N | New folder |
| Ctrl+L | Type a path |
| Ctrl+F | Search this folder |
| F5 | Refresh |

## Building

```sh
cargo run --release -p dropzone
```

Rust 1.88 or newer. On Linux, audio/video conversion uses `ffmpeg` from your PATH.
On Windows, put `ffmpeg.exe` next to `dropzone.exe` (the release zip already does).
`DROPZONE_FFMPEG=/path/to/ffmpeg` overrides both.

The repo is two crates:
- `crates/dropzone-core`: the engine (conversions, archives, PDF, rename, job queue), with tests.
- `crates/dropzone-app`: the egui explorer.

The *Build* workflow tests on Linux and produces `Dropzone-windows-x64.zip` on every push;
pushing a `v*` tag attaches it to a GitHub release.

## Licenses

Dropzone is MIT licensed. The Windows release bundles an unmodified `ffmpeg.exe`
(the [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) essentials build), which is GPLv3;
its license is included as `FFMPEG-LICENSE.txt` and its source is available from
[ffmpeg.org](https://ffmpeg.org/download.html). Dropzone runs it as a separate program.
