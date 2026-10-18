//! Background job queue: a few worker threads pull jobs, report progress through a
//! listener, and announce when the queue drains (so the app can quit when done).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ops::Operation;
use crate::util::Cancel;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub inputs: Vec<PathBuf>,
    pub operation: Operation,
    /// `None` writes outputs next to each input.
    #[serde(default)]
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JobInfo {
    pub id: u64,
    pub label: String,
    pub operation: String,
    pub inputs: Vec<PathBuf>,
    pub status: JobStatus,
    pub progress: f32,
    pub message: String,
    pub outputs: Vec<PathBuf>,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QueueEvent {
    Job(JobInfo),
    /// Every job has finished (successfully or not).
    Idle {
        done: usize,
        failed: usize,
    },
}

type Listener = Box<dyn Fn(QueueEvent) + Send + Sync>;

struct State {
    jobs: Vec<JobInfo>,
    specs: HashMap<u64, JobSpec>,
    cancels: HashMap<u64, Cancel>,
    pending: VecDeque<u64>,
    running: usize,
    next_id: u64,
    shutdown: bool,
}

struct Inner {
    state: Mutex<State>,
    wake: Condvar,
    listener: Listener,
}

#[derive(Clone)]
pub struct JobQueue {
    inner: Arc<Inner>,
}

impl JobQueue {
    pub fn new(workers: usize, listener: impl Fn(QueueEvent) + Send + Sync + 'static) -> Self {
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                jobs: Vec::new(),
                specs: HashMap::new(),
                cancels: HashMap::new(),
                pending: VecDeque::new(),
                running: 0,
                next_id: 1,
                shutdown: false,
            }),
            wake: Condvar::new(),
            listener: Box::new(listener),
        });
        for i in 0..workers.max(1) {
            let inner = inner.clone();
            std::thread::Builder::new().name(format!("dropzone-worker-{i}")).spawn(move || worker(inner)).expect("spawn worker");
        }
        Self { inner }
    }

    /// Default worker count: half the cores (encoders are multi-threaded themselves), at least 2.
    pub fn default_workers() -> usize {
        std::thread::available_parallelism().map(|n| (n.get() / 2).max(2)).unwrap_or(2)
    }

    /// Queues the spec, split into one job per input unless the operation combines inputs.
    pub fn enqueue(&self, spec: JobSpec) -> Vec<u64> {
        let specs: Vec<JobSpec> = if spec.operation.is_multi_input() || spec.inputs.len() <= 1 {
            vec![spec]
        } else {
            spec.inputs.iter().map(|i| JobSpec { inputs: vec![i.clone()], operation: spec.operation.clone(), output_dir: spec.output_dir.clone() }).collect()
        };
        let mut ids = Vec::new();
        let mut infos = Vec::new();
        {
            let mut st = self.inner.state.lock().unwrap();
            for s in specs {
                let id = st.next_id;
                st.next_id += 1;
                let first = s.inputs.first().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let label = if s.inputs.len() > 1 { format!("{first} + {} more", s.inputs.len() - 1) } else { first };
                let info = JobInfo {
                    id,
                    label,
                    operation: s.operation.label(),
                    inputs: s.inputs.clone(),
                    status: JobStatus::Queued,
                    progress: 0.0,
                    message: String::new(),
                    outputs: vec![],
                    summary: None,
                    error: None,
                    details: None,
                };
                st.jobs.push(info.clone());
                st.specs.insert(id, s);
                st.cancels.insert(id, Cancel::new());
                st.pending.push_back(id);
                infos.push(info);
                ids.push(id);
            }
        }
        for info in infos {
            (self.inner.listener)(QueueEvent::Job(info));
        }
        self.inner.wake.notify_all();
        ids
    }

    pub fn cancel(&self, id: u64) {
        let info = {
            let mut st = self.inner.state.lock().unwrap();
            if let Some(c) = st.cancels.get(&id) {
                c.cancel();
            }
            // Queued jobs are cancelled right away; running ones stop at their next check.
            if let Some(pos) = st.pending.iter().position(|j| *j == id) {
                st.pending.remove(pos);
                let job = st.jobs.iter_mut().find(|j| j.id == id).unwrap();
                job.status = JobStatus::Cancelled;
                Some(job.clone())
            } else {
                None
            }
        };
        if let Some(info) = info {
            (self.inner.listener)(QueueEvent::Job(info));
            self.maybe_idle();
        }
    }

    pub fn cancel_all(&self) {
        let ids: Vec<u64> = self.jobs().iter().filter(|j| !j.status.is_finished()).map(|j| j.id).collect();
        for id in ids {
            self.cancel(id);
        }
    }

    /// Drops finished jobs from the list.
    pub fn clear_finished(&self) {
        let mut st = self.inner.state.lock().unwrap();
        let finished: Vec<u64> = st.jobs.iter().filter(|j| j.status.is_finished()).map(|j| j.id).collect();
        st.jobs.retain(|j| !j.status.is_finished());
        for id in finished {
            st.specs.remove(&id);
            st.cancels.remove(&id);
        }
    }

    pub fn jobs(&self) -> Vec<JobInfo> {
        self.inner.state.lock().unwrap().jobs.clone()
    }

    pub fn is_busy(&self) -> bool {
        let st = self.inner.state.lock().unwrap();
        st.running > 0 || !st.pending.is_empty()
    }

    fn maybe_idle(&self) {
        notify_if_idle(&self.inner);
    }

    /// Blocks until the queue is idle or the timeout passes. Returns whether it's idle.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.is_busy() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        !self.is_busy()
    }

    pub fn shutdown(&self) {
        self.inner.state.lock().unwrap().shutdown = true;
        self.cancel_all();
        self.inner.wake.notify_all();
    }
}

fn notify_if_idle(inner: &Inner) {
    let event = {
        let st = inner.state.lock().unwrap();
        if st.running > 0 || !st.pending.is_empty() || st.jobs.is_empty() {
            return;
        }
        let done = st.jobs.iter().filter(|j| j.status == JobStatus::Done).count();
        let failed = st.jobs.iter().filter(|j| j.status == JobStatus::Failed).count();
        QueueEvent::Idle { done, failed }
    };
    (inner.listener)(event);
}

fn update(inner: &Inner, id: u64, f: impl FnOnce(&mut JobInfo)) {
    let info = {
        let mut st = inner.state.lock().unwrap();
        let Some(job) = st.jobs.iter_mut().find(|j| j.id == id) else { return };
        f(job);
        job.clone()
    };
    (inner.listener)(QueueEvent::Job(info));
}

fn worker(inner: Arc<Inner>) {
    loop {
        let (id, spec, cancel) = {
            let mut st = inner.state.lock().unwrap();
            loop {
                if st.shutdown {
                    return;
                }
                if let Some(id) = st.pending.pop_front() {
                    st.running += 1;
                    let spec = st.specs.get(&id).cloned();
                    let cancel = st.cancels.get(&id).cloned().unwrap_or_default();
                    match spec {
                        Some(spec) => break (id, spec, cancel),
                        None => {
                            st.running -= 1;
                            continue;
                        }
                    }
                }
                st = inner.wake.wait(st).unwrap();
            }
        };

        update(&inner, id, |j| {
            j.status = JobStatus::Running;
            j.message = "starting".into();
        });

        // Throttle progress events: the UI doesn't need more than ~10 per second per job.
        let last = Mutex::new((Instant::now() - Duration::from_secs(1), -1.0f32));
        let progress = |fraction: f32, message: &str| {
            let mut l = last.lock().unwrap();
            if l.0.elapsed() < Duration::from_millis(100) && (fraction - l.1).abs() < 0.05 {
                return;
            }
            *l = (Instant::now(), fraction);
            drop(l);
            let message = message.to_string();
            update(&inner, id, |j| {
                j.progress = fraction.clamp(0.0, 1.0);
                j.message = message;
            });
        };

        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| spec.operation.run(&spec.inputs, spec.output_dir.as_deref(), &progress, &cancel)));

        update(&inner, id, |j| match result {
            Ok(Ok(out)) => {
                j.status = JobStatus::Done;
                j.progress = 1.0;
                j.outputs = out.files;
                j.summary = Some(out.summary);
                j.details = out.details;
                j.message = String::new();
            }
            Ok(Err(_)) if cancel.is_cancelled() => {
                j.status = JobStatus::Cancelled;
                j.message = String::new();
            }
            Ok(Err(e)) => {
                j.status = JobStatus::Failed;
                j.error = Some(format!("{e:#}"));
                j.message = String::new();
            }
            Err(_) => {
                j.status = JobStatus::Failed;
                j.error = Some("internal error while processing this file".into());
            }
        });
        inner.state.lock().unwrap().running -= 1;
        notify_if_idle(&inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_ops::{ImageFormat, ImageOptions};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn runs_jobs_and_reports_idle() {
        let dir = tempfile::tempdir().unwrap();
        let inputs: Vec<PathBuf> = (0..6)
            .map(|i| {
                let p = dir.path().join(format!("img{i}.png"));
                image::RgbImage::from_pixel(64, 64, image::Rgb([i * 30, 10, 10])).save(&p).unwrap();
                p
            })
            .collect();
        let idle = Arc::new(AtomicUsize::new(0));
        let idle2 = idle.clone();
        let q = JobQueue::new(3, move |e| {
            if let QueueEvent::Idle { done, failed } = e {
                assert_eq!((done, failed), (6, 1));
                idle2.fetch_add(1, Ordering::SeqCst);
            }
        });
        let mut all = inputs.clone();
        all.push(dir.path().join("missing.png"));
        let op = Operation::ConvertImage { options: ImageOptions { format: Some(ImageFormat::Webp), ..Default::default() } };
        let ids = q.enqueue(JobSpec { inputs: all, operation: op, output_dir: Some(dir.path().join("out")) });
        assert_eq!(ids.len(), 7);
        assert!(q.wait_idle(Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(50));
        let jobs = q.jobs();
        assert_eq!(jobs.iter().filter(|j| j.status == JobStatus::Done).count(), 6);
        assert!(jobs.iter().any(|j| j.status == JobStatus::Failed && j.error.is_some()));
        assert!(dir.path().join("out/img0.webp").exists());
        assert!(idle.load(Ordering::SeqCst) >= 1);
        q.clear_finished();
        assert!(q.jobs().is_empty());
    }

    #[test]
    fn json_shape_matches_the_ui() {
        let op: Operation = serde_json::from_str(r#"{"kind":"convert_image","options":{"format":"avif","quality":50}}"#).unwrap();
        assert!(matches!(op, Operation::ConvertImage { options } if options.format == Some(ImageFormat::Avif) && options.quality == 50));
        let op: Operation = serde_json::from_str(r#"{"kind":"create_archive","options":{"format":"7z"}}"#).unwrap();
        assert!(op.is_multi_input());
        let op: Operation = serde_json::from_str(r#"{"kind":"verify_archive"}"#).unwrap();
        assert!(matches!(op, Operation::VerifyArchive { recursive: true, .. }));
    }
}
