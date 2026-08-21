use std::process::Command;

pub fn run_shell(cmd: &str) -> (String, String, bool) {
    let result = Command::new("bash").arg("-c").arg(cmd).output();
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            (stdout, stderr, out.status.success())
        }
        Err(e) => (String::new(), e.to_string(), false),
    }
}

pub fn run_osascript(script: &str) -> (String, String, bool) {
    let result = Command::new("osascript").arg("-e").arg(script).output();
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            (stdout, stderr, out.status.success())
        }
        Err(e) => (String::new(), e.to_string(), false),
    }
}
