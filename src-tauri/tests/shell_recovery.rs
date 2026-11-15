#![cfg(unix)]

use std::{
    fs,
    process::{Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

#[test]
fn supervisor_saves_exit_result_after_launcher_is_killed() {
    let dir = std::env::temp_dir().join(format!("shell-recovery-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("receipt"), "").unwrap();
    let mut launcher = Command::new("sh")
        .current_dir(&dir)
        .args(["-c", "\"$1\" --internal-shell-receipt receipt 'echo started >> effect; while [ ! -f release ]; do sleep 0.05; done; exit 7' & wait", "launcher"])
        .arg(env!("CARGO_BIN_EXE_free-api-chat"))
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !dir.join("effect").exists() && Instant::now() < deadline {
        sleep(Duration::from_millis(20));
    }
    let started = dir.join("effect").exists();
    launcher.kill().unwrap();
    launcher.wait().unwrap();
    fs::write(dir.join("release"), "").unwrap();
    assert!(started, "command must start before killing the launcher");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let raw = fs::read(dir.join("receipt")).unwrap();
        if let Ok(receipt) = serde_json::from_slice::<serde_json::Value>(&raw) {
            assert_eq!(receipt["exit_code"], 7);
            assert_eq!(receipt["success"], false);
            break;
        }
        assert!(Instant::now() < deadline, "no persisted exit receipt");
        sleep(Duration::from_millis(20));
    }
    assert_eq!(fs::read_to_string(dir.join("effect")).unwrap(), "started\n");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn supervisor_survives_term_until_an_ignoring_command_is_killed() {
    use std::os::unix::process::CommandExt;
    let dir = std::env::temp_dir().join(format!("shell-term-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("receipt"), "").unwrap();
    let mut supervisor = Command::new(env!("CARGO_BIN_EXE_free-api-chat"))
        .current_dir(&dir)
        .args(["--internal-shell-receipt", "receipt", "trap '' TERM; echo ready > ready; while :; do sleep 1; done"])
        .process_group(0)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().unwrap();
    let pid = supervisor.id() as i32;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !dir.join("ready").exists() && Instant::now() < deadline {
        sleep(Duration::from_millis(20));
    }
    let started = dir.join("ready").exists();
    unsafe { libc::kill(-pid, libc::SIGTERM); }
    sleep(Duration::from_millis(200));
    let alive = supervisor.try_wait().unwrap().is_none();
    unsafe { libc::kill(-pid, libc::SIGKILL); }
    let status = supervisor.wait().unwrap();
    let _ = fs::remove_dir_all(dir);
    assert!(started, "command must start");
    assert!(alive, "supervisor must remain alive while the command ignores TERM");
    assert!(!status.success());
}
