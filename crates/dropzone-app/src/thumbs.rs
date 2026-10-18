//! Background thumbnail generation for the grid view, with a small texture cache.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use eframe::egui;

const THUMB: u32 = 176; // 2× the tile size, so it stays crisp on high-DPI screens
const MAX_CACHED: usize = 600;
const MAX_FILE: u64 = 80 * 1024 * 1024;

enum Slot {
    Pending,
    Ready(egui::TextureHandle),
    Failed,
}

pub struct Thumbs {
    ctx: egui::Context,
    slots: HashMap<PathBuf, Slot>,
    order: VecDeque<PathBuf>,
    jobs: Arc<Mutex<Vec<PathBuf>>>,
    wake: Sender<()>,
    results: Receiver<(PathBuf, Option<image::RgbaImage>)>,
}

impl Thumbs {
    pub fn new(ctx: egui::Context) -> Self {
        let jobs: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
        let (wake_tx, wake_rx) = channel::<()>();
        let (res_tx, res_rx) = channel();
        let wake_rx = Arc::new(Mutex::new(wake_rx));
        for _ in 0..2 {
            let jobs = jobs.clone();
            let wake_rx = wake_rx.clone();
            let res_tx = res_tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                loop {
                    // Newest requests first: those are what's on screen now.
                    let next = jobs.lock().unwrap().pop();
                    match next {
                        Some(p) => {
                            let img = make_thumb(&p);
                            if res_tx.send((p, img)).is_err() {
                                return;
                            }
                            ctx.request_repaint();
                        }
                        None => {
                            if wake_rx.lock().unwrap().recv().is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
        Self { ctx, slots: HashMap::new(), order: VecDeque::new(), jobs, wake: wake_tx, results: res_rx }
    }

    /// Returns the thumbnail if it's ready, otherwise schedules it.
    pub fn get(&mut self, path: &Path, size: u64) -> Option<egui::TextureHandle> {
        match self.slots.get(path) {
            Some(Slot::Ready(t)) => return Some(t.clone()),
            Some(_) => return None,
            None => {}
        }
        if size > MAX_FILE {
            self.slots.insert(path.to_path_buf(), Slot::Failed);
            return None;
        }
        self.slots.insert(path.to_path_buf(), Slot::Pending);
        self.jobs.lock().unwrap().push(path.to_path_buf());
        let _ = self.wake.send(());
        let _ = self.wake.send(());
        None
    }

    pub fn poll(&mut self) {
        while let Ok((path, img)) = self.results.try_recv() {
            let slot = match img {
                Some(img) => Slot::Ready(crate::common::texture_from_rgba(&self.ctx, &path.display().to_string(), &img)),
                None => Slot::Failed,
            };
            self.slots.insert(path.clone(), slot);
            self.order.push_back(path);
            while self.order.len() > MAX_CACHED {
                if let Some(old) = self.order.pop_front() {
                    self.slots.remove(&old);
                }
            }
        }
    }
}

fn make_thumb(path: &Path) -> Option<image::RgbaImage> {
    let img = dropzone_core::image_ops::load_image(path).ok()?;
    Some(img.thumbnail(THUMB, THUMB).to_rgba8())
}
