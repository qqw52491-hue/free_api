//! The command supervisor owns the exit receipt independently of the UI process.
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    process::Command,
};

pub const MODE: &str = "--internal-shell-receipt";

/// The supervisor must survive TERM long enough to observe whether the command
/// exits. The actual shell restores the default disposition before exec.
pub fn prepare_supervisor_signals() -> io::Result<()> {
    #[cfg(unix)]
    unsafe {
        if libc::signal(libc::SIGTERM, libc::SIG_IGN) == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ExitReceipt {
    pub exit_code: Option<i32>,
    pub success: bool,
    pub duration_ms: u64,
}

pub(crate) enum ReceiptState {
    Running,
    Finished(ExitReceipt),
    Unknown,
}

fn receipt_path(db: &Path, id: &str) -> io::Result<PathBuf> {
    let id = uuid::Uuid::parse_str(id).map_err(io::Error::other)?;
    let db = db.canonicalize()?;
    let parent = db
        .parent()
        .ok_or_else(|| io::Error::other("数据库路径没有父目录"))?;
    Ok(parent.join("shell-receipts").join(id.to_string()))
}

pub(crate) fn database_path(db: &crate::db::DbState) -> io::Result<PathBuf> {
    let conn = db.0.lock().map_err(|e| io::Error::other(e.to_string()))?;
    conn.path()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("后台退出回执需要文件数据库"))
}

pub(crate) fn prepare(db: &Path) -> io::Result<(String, PathBuf)> {
    let id = uuid::Uuid::new_v4().to_string();
    let path = receipt_path(db, &id)?;
    let mut directory = std::fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(path.parent().unwrap())?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    Ok((id, path))
}

pub(crate) fn inspect(db: &Path, id: &str) -> io::Result<ReceiptState> {
    inspect_path(&receipt_path(db, id)?)
}

pub(crate) fn inspect_path(path: &Path) -> io::Result<ReceiptState> {
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ReceiptState::Unknown),
        Err(e) => return Err(e),
    };
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(ReceiptState::Running),
        Err(std::fs::TryLockError::Error(e)) => return Err(e),
    }
    let mut data = Vec::new();
    (&mut file).take(4096).read_to_end(&mut data)?;
    if let Ok(receipt) = serde_json::from_slice(&data) {
        return Ok(ReceiptState::Finished(receipt));
    }
    // A supervisor scheduled after recovery must not start the abandoned command.
    if data.is_empty() {
        file.write_all(b"abandoned")?;
        file.sync_all()?;
    }
    Ok(ReceiptState::Unknown)
}

pub(crate) fn discard(db: &Path, id: &str) {
    if let Ok(path) = receipt_path(db, id) {
        let _ = std::fs::remove_file(path);
    }
}

pub fn supervise(path: &Path, script: &std::ffi::OsStr) -> io::Result<i32> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.lock()?;
    if file.metadata()?.len() != 0 {
        return Err(io::Error::other(
            "shell execution was already started or abandoned",
        ));
    }
    file.write_all(b"started")?;
    file.sync_all()?;

    let start = std::time::Instant::now();
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd.exe");
        command.arg("/C");
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("sh");
        command.arg("-c");
        unsafe {
            command.pre_exec(|| {
                if libc::signal(libc::SIGTERM, libc::SIG_DFL) == libc::SIG_ERR {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
    };
    let status = command.arg(script).status()?;
    write_receipt(
        &mut file,
        &ExitReceipt {
            exit_code: status.code(),
            success: status.success(),
            duration_ms: start.elapsed().as_millis() as u64,
        },
    )?;
    Ok(status.code().unwrap_or(1))
}

fn write_receipt(file: &mut File, receipt: &ExitReceipt) -> io::Result<()> {
    file.rewind()?;
    file.set_len(0)?;
    serde_json::to_writer(&mut *file, receipt)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    #[test]
    fn recovery_prevents_a_late_supervisor_from_starting() {
        let path = std::env::temp_dir().join(format!("shell-receipt-{}", uuid::Uuid::new_v4()));
        std::fs::File::create(&path).unwrap();
        assert!(matches!(super::inspect_path(&path).unwrap(), super::ReceiptState::Unknown));
        assert!(super::supervise(&path, std::ffi::OsStr::new("exit 0")).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
