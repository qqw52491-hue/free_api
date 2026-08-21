use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use std::time::Instant;
use tokio::sync::watch;
use tokio::task::{AbortHandle, JoinHandle};

pub type TaskId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskKind {
    McpActor,
    McpInit,
    McpSessionInit,
    AgentLoop,
    PlanLeaf,
    LessonWrite,
    Probe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Finishing,
    Done,
    Failed,
}

impl TaskStatus {
    fn as_u8(self) -> u8 {
        self as u8
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => TaskStatus::Running,
            1 => TaskStatus::Finishing,
            2 => TaskStatus::Done,
            _ => TaskStatus::Failed,
        }
    }
}

pub struct TaskEntry {
    pub kind: TaskKind,
    pub name: String,
    pub abort: AbortHandle,
    pub finished: Arc<AtomicBool>,
    pub started_at: Instant,
    pub status: AtomicU8,
    pub cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

pub struct TaskRegistry {
    tasks: Mutex<HashMap<TaskId, TaskEntry>>,
    shutdown_tx: watch::Sender<bool>,
    next_id: AtomicU64,
}

static GLOBAL_REGISTRY: OnceLock<Arc<TaskRegistry>> = OnceLock::new();

impl TaskRegistry {
    pub fn global() -> Arc<TaskRegistry> {
        GLOBAL_REGISTRY
            .get_or_init(|| {
                Arc::new(TaskRegistry {
                    tasks: Mutex::new(HashMap::new()),
                    shutdown_tx: watch::channel(false).0,
                    next_id: AtomicU64::new(1),
                })
            })
            .clone()
    }

    pub fn spawn<F>(&self, kind: TaskKind, name: impl Into<String>, fut: F) -> TaskId
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_c = finished.clone();
        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = fut => {}
                _ = cancel_rx => {}
            }
            finished_c.store(true, Ordering::SeqCst);
        });
        let entry = TaskEntry {
            kind,
            name: name.into(),
            abort: handle.abort_handle(),
            finished,
            started_at: Instant::now(),
            status: AtomicU8::new(TaskStatus::Running.as_u8()),
            cancel_tx: Some(cancel_tx),
        };
        self.tasks.lock().unwrap().insert(id, entry);
        id
    }

    /// 注册任务并返回原始 JoinHandle，供调用方 await 获取返回值（如 PlanTree 并行叶子）。
    /// 取消时走 abort 强杀（无协作取消信号）。
    pub fn spawn_with_handle<F, T>(
        &self,
        kind: TaskKind,
        name: impl Into<String>,
        fut: F,
    ) -> (TaskId, JoinHandle<T>)
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let finished = Arc::new(AtomicBool::new(false));
        let finished_c = finished.clone();
        let handle: JoinHandle<T> = tokio::spawn(async move {
            let res = fut.await;
            finished_c.store(true, Ordering::SeqCst);
            res
        });
        let entry = TaskEntry {
            kind,
            name: name.into(),
            abort: handle.abort_handle(),
            finished,
            started_at: Instant::now(),
            status: AtomicU8::new(TaskStatus::Running.as_u8()),
            cancel_tx: None,
        };
        self.tasks.lock().unwrap().insert(id, entry);
        (id, handle)
    }

    pub async fn cancel(&self, id: TaskId) {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(entry) = tasks.get_mut(&id) {
            entry
                .status
                .store(TaskStatus::Finishing.as_u8(), Ordering::SeqCst);
            if let Some(tx) = entry.cancel_tx.take() {
                let _ = tx.send(());
            } else {
                entry.abort.abort();
            }
        }
    }

    pub async fn cancel_all(&self) {
        let mut tasks = self.tasks.lock().unwrap();
        for entry in tasks.values_mut() {
            entry
                .status
                .store(TaskStatus::Finishing.as_u8(), Ordering::SeqCst);
            if let Some(tx) = entry.cancel_tx.take() {
                let _ = tx.send(());
            } else {
                entry.abort.abort();
            }
        }
        tasks.clear();
    }

    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }

    pub fn trigger_shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// 优雅停机：广播停机信号，等待所有任务在超时内自然结束，超时后强制 abort
    pub async fn graceful_shutdown(&self, timeout: Duration) {
        self.trigger_shutdown();

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let all_done = {
                let snapshot = self.tasks.lock().unwrap();
                snapshot.values().all(|e| e.finished.load(Ordering::SeqCst))
            };
            if all_done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // 超时后强制 abort 所有未完成任务
        let mut tasks = self.tasks.lock().unwrap();
        for entry in tasks.values_mut() {
            if !entry.finished.load(Ordering::SeqCst) {
                entry
                    .status
                    .store(TaskStatus::Finishing.as_u8(), Ordering::SeqCst);
                entry.abort.abort();
            }
        }
        tasks.clear();
    }

    pub async fn mark_done(&self, id: TaskId) {
        if let Some(entry) = self.tasks.lock().unwrap().get_mut(&id) {
            entry
                .status
                .store(TaskStatus::Done.as_u8(), Ordering::SeqCst);
        }
    }

    pub async fn metrics(&self) -> HashMap<TaskKind, usize> {
        let mut counts = HashMap::new();
        let tasks = self.tasks.lock().unwrap();
        for entry in tasks.values() {
            *counts.entry(entry.kind).or_insert(0) += 1;
        }
        counts
    }

    pub async fn task_count(&self) -> usize {
        self.tasks.lock().unwrap().len()
    }
}
