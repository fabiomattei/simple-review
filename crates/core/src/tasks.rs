//! Background work: a small worker pool with progress and cancellation, a lazily paging commit
//! walker, and an LRU cache for computed results.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use gix::ObjectId;

use crate::git::{CommitInfo, Repo};

/// Called whenever a background task has something new for the UI (e.g. `ctx.request_repaint`).
pub type Notify = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct TaskStatus {
    pub id: u64,
    pub label: String,
    /// 0.0..=1.0, or `None` when progress is unknown.
    pub progress: Option<f32>,
}

type Job = Box<dyn FnOnce() + Send>;

/// Context handed to a running task.
pub struct TaskCtx {
    id: u64,
    cancel: CancelToken,
    status: Arc<Mutex<Vec<TaskStatus>>>,
    notify: Notify,
}

impl TaskCtx {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn set_progress(&self, progress: f32) {
        if let Some(s) = self.status.lock().unwrap().iter_mut().find(|s| s.id == self.id) {
            s.progress = Some(progress.clamp(0.0, 1.0));
        }
        (self.notify)();
    }
}

/// A fixed set of worker threads. Results of type `R` are collected through [`TaskPool::try_recv`].
pub struct TaskPool<R> {
    jobs: Sender<Job>,
    results_tx: Sender<R>,
    results_rx: Receiver<R>,
    status: Arc<Mutex<Vec<TaskStatus>>>,
    next_id: AtomicU64,
    notify: Notify,
}

impl<R: Send + 'static> TaskPool<R> {
    pub fn new(workers: usize, notify: Notify) -> Self {
        let (jobs, job_rx) = unbounded::<Job>();
        for i in 0..workers.max(1) {
            let job_rx = job_rx.clone();
            thread::Builder::new()
                .name(format!("worker-{i}"))
                .spawn(move || {
                    for job in job_rx {
                        job();
                    }
                })
                .expect("spawn worker thread");
        }
        let (results_tx, results_rx) = unbounded();
        TaskPool {
            jobs,
            results_tx,
            results_rx,
            status: Arc::default(),
            next_id: AtomicU64::new(1),
            notify,
        }
    }

    /// Queue `f`. It may send any number of results through the provided sender. The returned
    /// token cancels it; tasks are expected to poll [`TaskCtx::is_cancelled`].
    pub fn spawn(
        &self,
        label: impl Into<String>,
        f: impl FnOnce(&TaskCtx, &dyn Fn(R)) + Send + 'static,
    ) -> CancelToken {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = CancelToken::default();
        self.status.lock().unwrap().push(TaskStatus { id, label: label.into(), progress: None });
        let ctx = TaskCtx {
            id,
            cancel: cancel.clone(),
            status: self.status.clone(),
            notify: self.notify.clone(),
        };
        let results = self.results_tx.clone();
        let job = move || {
            if !ctx.is_cancelled() {
                let send = |r: R| {
                    let _ = results.send(r);
                    (ctx.notify)();
                };
                f(&ctx, &send);
            }
            ctx.status.lock().unwrap().retain(|s| s.id != ctx.id);
            (ctx.notify)();
        };
        let _ = self.jobs.send(Box::new(job));
        cancel
    }

    pub fn try_recv(&self) -> Option<R> {
        self.results_rx.try_recv().ok()
    }

    /// Tasks queued or running.
    pub fn running(&self) -> Vec<TaskStatus> {
        self.status.lock().unwrap().clone()
    }
}

#[derive(Debug)]
pub enum PagerEvent {
    Batch(Vec<CommitInfo>),
    /// History is exhausted.
    Done,
    Error(String),
}

/// Walks history on its own thread and hands out commits a page at a time, so opening a repository
/// with a huge history only costs as much as the user scrolls through.
pub struct CommitPager {
    requests: Sender<usize>,
    events: Receiver<PagerEvent>,
}

impl CommitPager {
    pub fn start(repo: Repo, tip: ObjectId, notify: Notify) -> Self {
        let (requests, request_rx) = unbounded::<usize>();
        let (event_tx, events) = unbounded();
        thread::Builder::new()
            .name("commit-pager".into())
            .spawn(move || {
                let Ok(mut wanted) = request_rx.recv() else { return };
                let mut batch = Vec::new();
                let result = repo.walk_commits(tip, |commit| {
                    batch.push(commit);
                    if batch.len() < wanted {
                        return ControlFlow::Continue(());
                    }
                    let _ = event_tx.send(PagerEvent::Batch(std::mem::take(&mut batch)));
                    notify();
                    match request_rx.recv() {
                        Ok(n) => {
                            wanted = n;
                            ControlFlow::Continue(())
                        }
                        // The UI dropped the pager: stop walking.
                        Err(_) => ControlFlow::Break(()),
                    }
                });
                if !batch.is_empty() {
                    let _ = event_tx.send(PagerEvent::Batch(batch));
                }
                let _ = event_tx.send(match result {
                    Ok(()) => PagerEvent::Done,
                    Err(e) => PagerEvent::Error(format!("{e:#}")),
                });
                notify();
            })
            .expect("spawn pager thread");
        CommitPager { requests, events }
    }

    /// Ask for `n` more commits.
    pub fn request(&self, n: usize) {
        let _ = self.requests.send(n.max(1));
    }

    pub fn try_recv(&self) -> Option<PagerEvent> {
        self.events.try_recv().ok()
    }

    /// Blocking receive, for tests.
    pub fn recv(&self) -> Option<PagerEvent> {
        self.events.recv().ok()
    }
}

/// A small least-recently-used cache.
pub struct LruCache<K, V> {
    map: HashMap<K, V>,
    order: VecDeque<K>,
    capacity: usize,
}

impl<K: Eq + Hash + Clone, V: Clone> LruCache<K, V> {
    pub fn new(capacity: usize) -> Self {
        LruCache { map: HashMap::new(), order: VecDeque::new(), capacity: capacity.max(1) }
    }

    pub fn get(&mut self, key: &K) -> Option<V> {
        let value = self.map.get(key)?.clone();
        self.touch(key);
        Some(value)
    }

    pub fn insert(&mut self, key: K, value: V) {
        if self.map.insert(key.clone(), value).is_some() {
            self.touch(&key);
            return;
        }
        self.order.push_back(key);
        while self.order.len() > self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    fn touch(&mut self, key: &K) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(pos).expect("position is valid");
            self.order.push_back(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_evicts_oldest() {
        let mut c = LruCache::new(2);
        c.insert(1, "a");
        c.insert(2, "b");
        assert_eq!(c.get(&1), Some("a"));
        c.insert(3, "c");
        assert_eq!(c.get(&2), None);
        assert_eq!(c.get(&1), Some("a"));
        assert_eq!(c.get(&3), Some("c"));
    }

    #[test]
    fn pool_runs_and_cancels() {
        let pool: TaskPool<u32> = TaskPool::new(1, Arc::new(|| {}));
        let gate = Arc::new(std::sync::Barrier::new(2));
        let g = gate.clone();
        pool.spawn("block", move |_, _| {
            g.wait();
        });
        let cancelled = pool.spawn("never", |_, send| send(1));
        cancelled.cancel();
        pool.spawn("ok", |ctx, send| {
            ctx.set_progress(0.5);
            send(2)
        });
        gate.wait();
        let got = pool.results_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(got, 2);
        assert!(pool.try_recv().is_none());
    }
}
