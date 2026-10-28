//! Local diagnostics for Rust panics and sessions that did not exit normally.
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

static PANIC_NOTIFIER: OnceLock<tokio::sync::mpsc::UnboundedSender<()>> = OnceLock::new();

#[derive(Serialize, Deserialize)]
struct Marker {
    pid: u32,
    started: u64,
}

#[derive(Clone)]
pub struct CrashSession {
    marker: Option<PathBuf>,
    dir: PathBuf,
}

fn process_start(pid: u32) -> Option<u64> {
    let mut system = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(pid);
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).map(|p| p.start_time())
}

fn is_stale(marker: &Marker, current: Option<u64>) -> bool {
    match current {
        None => true,
        Some(0) => false,
        Some(start) => start != marker.started,
    }
}

impl CrashSession {
    pub fn new(db_path: &Path) -> Self {
        let dir = db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("diagnostics");
        let marker = (|| {
            fs::create_dir_all(&dir).ok()?;
            let pid = std::process::id();
            let started = process_start(pid).filter(|start| *start != 0)?;
            let path = dir.join(format!("session-{}.json", uuid::Uuid::new_v4()));
            let data = serde_json::to_vec(&Marker { pid, started }).ok()?;
            fs::write(&path, data).ok()?;
            Some(path)
        })();
        Self { marker, dir }
    }

    pub fn install_panic_hook(&self) {
        let log = self.dir.join("panic.log");
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Panic messages may contain user data; write a bounded diagnostic, never upload it.
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
                .unwrap_or("非文本 panic");
            let message: String = message.chars().take(2000).collect();
            let location = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "未知位置".into());
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log) {
                let _ = writeln!(
                    file,
                    "{} pid={} location={}\n{}\n",
                    chrono::Utc::now().to_rfc3339(),
                    std::process::id(),
                    location,
                    message
                );
            }
            if let Some(tx) = PANIC_NOTIFIER.get() {
                let _ = tx.send(());
            }
            previous(info);
        }));
    }

    pub fn record_startup_error(&self, message: &str) {
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("startup.log"))
        {
            let _ = writeln!(file, "{} {}", chrono::Utc::now().to_rfc3339(), message);
        }
    }

    fn stale_markers(&self) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return vec![];
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if self.marker.as_ref() == Some(&path)
                    || !entry.file_name().to_string_lossy().starts_with("session-")
                {
                    return None;
                }
                let marker: Marker = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
                is_stale(&marker, process_start(marker.pid)).then_some(path)
            })
            .collect()
    }

    pub fn start_notifications(&self, app: &AppHandle) {
        let stale = self.stale_markers();
        if !stale.is_empty() {
            let message = format!("检测到上次有应用进程未正常结束，可能是崩溃、被强制关闭或系统关机。\n\n诊断目录：{}\n如需排查，可查看 panic.log（不一定存在）；日志可能含个人信息，请检查后再分享。", self.dir.display());
            app.dialog()
                .message(message)
                .title("上次运行未正常结束")
                .kind(MessageDialogKind::Warning)
                .show(move |_| {
                    for path in stale {
                        let _ = fs::remove_file(path);
                    }
                });
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _ = PANIC_NOTIFIER.set(tx);
        let app = app.clone();
        let dir = self.dir.clone();
        tauri::async_runtime::spawn(async move {
            while rx.recv().await.is_some() {
                app.dialog().message(format!("程序内部发生异常，当前任务可能未完成。应用可能仍在运行。\n\n诊断日志：{}\n日志可能含个人信息，请检查后再分享。", dir.join("panic.log").display()))
                    .title("程序发生异常").kind(MessageDialogKind::Error).show(|_| {});
            }
        });
    }

    pub fn mark_clean(&self) {
        if let Some(path) = &self.marker {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marker_checks_pid_reuse_and_live_processes() {
        let marker = Marker {
            pid: 42,
            started: 100,
        };
        assert!(!is_stale(&marker, Some(100)));
        assert!(!is_stale(&marker, Some(0)));
        assert!(is_stale(&marker, Some(101)));
        assert!(is_stale(&marker, None));
    }
    #[test]
    fn normal_exit_removes_only_own_marker() {
        let dir = std::env::temp_dir().join(format!("crash-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("db.sqlite");
        let first = CrashSession::new(&db);
        let second = CrashSession::new(&db);
        assert!(first.marker.as_ref().unwrap().exists());
        assert!(first.stale_markers().is_empty());
        first.mark_clean();
        assert!(!first.marker.as_ref().unwrap().exists());
        assert!(second.marker.as_ref().unwrap().exists());
        second.mark_clean();
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn terminated_process_leaves_a_detectable_marker() {
        const ENV: &str = "FREE_API_CRASH_TEST_DIR";
        if let Some(dir) = std::env::var_os(ENV) {
            let session = CrashSession::new(&PathBuf::from(dir).join("db.sqlite"));
            assert!(session.marker.is_some());
            std::process::exit(0);
        }
        let dir = std::env::temp_dir().join(format!("crash-child-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "crash_report::tests::terminated_process_leaves_a_detectable_marker",
            ])
            .env(ENV, &dir)
            .status()
            .unwrap();
        assert!(status.success());
        let session = CrashSession::new(&dir.join("db.sqlite"));
        let stale = session.stale_markers();
        assert_eq!(stale.len(), 1);
        session.mark_clean();
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn panic_hook_records_caught_panic_without_claiming_process_exited() {
        const ENV: &str = "FREE_API_PANIC_TEST_DIR";
        if let Some(dir) = std::env::var_os(ENV) {
            let session = CrashSession::new(&PathBuf::from(dir).join("db.sqlite"));
            session.install_panic_hook();
            let _ = std::panic::catch_unwind(|| panic!("diagnostic test"));
            session.mark_clean();
            std::process::exit(0);
        }
        let dir = std::env::temp_dir().join(format!("panic-child-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash_report::tests::panic_hook_records_caught_panic_without_claiming_process_exited"])
            .env(ENV, &dir).status().unwrap();
        assert!(status.success());
        let session = CrashSession::new(&dir.join("db.sqlite"));
        assert!(session.stale_markers().is_empty());
        let log = fs::read_to_string(dir.join("diagnostics/panic.log")).unwrap();
        assert!(log.contains("diagnostic test"));
        session.mark_clean();
        fs::remove_dir_all(dir).unwrap();
    }
}
