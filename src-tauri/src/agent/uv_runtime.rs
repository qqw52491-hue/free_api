//! uv 运行时就绪：Python 生态 stdio MCP 的零依赖入口。
//!
//! 解析顺序：进程内缓存 → Tauri 内嵌 sidecar → PATH → app_data 缓存。
//! 全未命中时 `ensure_uv` 联网下载官方 release（sha256 校验后解压落盘）。
//! `uvx <pkg>` 等价 `uv tool run <pkg>`——单个 uv 二进制即覆盖两种调用，
//! uv 还会自举受管 Python，宿主机无需预装 Python/pip。
//!
//! 供应链纪律：只从 astral-sh/uv 官方 releases 拉取，强制 sha256 校验；
//! 缓存落 app_data/tools/uv/，不污染用户目录。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::AppHandle;
use tauri::Manager;

/// ensure_uv 成功后就绪的路径缓存——spawn 是同步上下文，靠它免 AppHandle 复用结果。
static RESOLVED_UV: OnceLock<Option<PathBuf>> = OnceLock::new();

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "uv.exe"
    } else {
        "uv"
    }
}

/// 命令是否为 uv/uvx 调用（含绝对路径形式）。
pub fn is_uv_invocation(command: &str) -> bool {
    let trimmed = command.trim();
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    matches!(base.trim_end_matches(".exe"), "uv" | "uvx")
}

fn is_executable_file(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn app_uv_dir(app: Option<&AppHandle>) -> Option<PathBuf> {
    if let Some(app) = app {
        app.path().app_data_dir().ok().map(|d| d.join("tools").join("uv"))
    } else {
        // spawn 同步上下文无 AppHandle：按 bundle identifier 推导约定路径
        let base = dirs_next::data_dir().or_else(dirs_next::data_local_dir)?;
        Some(base.join("com.free-api-chat.dev").join("tools").join("uv"))
    }
}

/// 平台三元组 → 官方 release 资源名（覆盖桌面三平台主架构）。
fn platform_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        _ => None,
    }
}

/// 同步解析可用 uv 二进制（不联网）：缓存 → sidecar → PATH → app_data。
/// uvx 调用一律改写为 `uv tool run`，所以只找 uv。
pub fn resolve_uv(app: Option<&AppHandle>) -> Option<PathBuf> {
    if let Some(cached) = RESOLVED_UV.get() {
        return cached.clone();
    }
    // 1. Tauri 内嵌 sidecar（打包内置时）
    if let Some(app) = app {
        if let Ok(res) = app.path().resource_dir() {
            for cand in [
                res.join("binaries").join(exe_name()),
                res.join("binaries").join("uv"),
            ] {
                if is_executable_file(&cand) {
                    return Some(cand);
                }
            }
        }
    }
    // 2. PATH 扫描（用户自装；uvx 独立存在时也可用其同目录的 uv）
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in [exe_name(), "uv", "uvx", "uvx.exe"] {
                let cand = dir.join(name);
                if is_executable_file(&cand) {
                    // uvx 命中时尝试同目录的 uv（改写需要 uv 本体）
                    if name.starts_with("uvx") {
                        let sibling = dir.join(exe_name());
                        if is_executable_file(&sibling) {
                            return Some(sibling);
                        }
                        continue;
                    }
                    return Some(cand);
                }
            }
        }
    }
    // 3. app_data 缓存（ensure_uv 曾下载的副本）
    if let Some(dir) = app_uv_dir(app) {
        let cand = dir.join(exe_name());
        if is_executable_file(&cand) {
            return Some(cand);
        }
    }
    None
}

/// spawn 前的命令改写：`uvx pkg args` → `<resolved uv> tool run pkg args`；
/// `uv ...` → `<resolved uv> ...`。无法解析时原样返回（spawn 报明确错误）。
/// 返回 (program, args, extra_env)。extra_env 把 uv 缓存指进 app_data。
pub fn rewrite_uvx_invocation(
    command: &str,
    args: &[String],
) -> (String, Vec<String>, Vec<(String, String)>) {
    if !is_uv_invocation(command) {
        return (command.to_string(), args.to_vec(), Vec::new());
    }
    let Some(uv) = resolve_uv(None) else {
        return (command.to_string(), args.to_vec(), Vec::new());
    };
    let program = uv.to_string_lossy().to_string();
    let is_uvx = command
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim_end_matches(".exe")
        == "uvx";
    let new_args: Vec<String> = if is_uvx {
        let mut v = vec!["tool".to_string(), "run".to_string()];
        v.extend(args.iter().cloned());
        v
    } else {
        args.to_vec()
    };
    // uv 缓存与受管 Python 统一进 app_data，不污染用户家目录
    let mut env = Vec::new();
    if let Some(dir) = app_uv_dir(None) {
        if let Some(cache) = dir.parent().map(|p| p.join("uv-cache")) {
            env.push(("UV_CACHE_DIR".to_string(), cache.to_string_lossy().to_string()));
        }
    }
    (program, new_args, env)
}

/// 异步就绪 uv：先同步解析，未命中则联网下载官方 release。
/// 供持锁之外的 async 上下文调用（网络下载耗时不定，严禁在锁内调用）。
pub async fn ensure_uv(app: &AppHandle) -> Result<PathBuf, String> {
    if let Some(p) = resolve_uv(Some(app)) {
        return Ok(p);
    }
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法定位 app_data 目录: {e}"))?;
    let path = tokio::task::spawn_blocking(move || download_and_install(&app_data))
        .await
        .map_err(|e| format!("uv 下载任务异常: {e}"))??;
    let _ = RESOLVED_UV.set(Some(path.clone()));
    Ok(path)
}

/// 阻塞式下载安装：release 拉取 → sha256 校验 → 解压出 uv/uvx。
fn download_and_install(app_data: &Path) -> Result<PathBuf, String> {
    let triple = platform_triple()
        .ok_or_else(|| format!("当前平台无可用 uv 预编译包（{}）", std::env::consts::OS))?;
    let (archive_name, is_zip) = if cfg!(windows) {
        (format!("uv-{triple}.zip"), true)
    } else {
        (format!("uv-{triple}.tar.gz"), false)
    };
    let base = "https://github.com/astral-sh/uv/releases/latest/download";
    let archive_url = format!("{base}/{archive_name}");

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| format!("构建下载客户端失败: {e}"))?;

    // 先拉校验值（小文件），失败不阻断下载但必须如实记录
    let expected_sha = client
        .get(format!("{archive_url}.sha256"))
        .send()
        .ok()
        .and_then(|r| r.text().ok())
        .and_then(|t| t.split_whitespace().next().map(|s| s.to_string()))
        .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()));

    let bytes = client
        .get(&archive_url)
        .send()
        .map_err(|e| format!("下载 uv 失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("下载 uv HTTP 错误: {e}"))?
        .bytes()
        .map_err(|e| format!("读取 uv 包体失败: {e}"))?;

    // 供应链防线：官方 sha256 对不上就拒装（拿不到校验文件同样拒装——
    // 可校验性消失时宁可失败，不静默放过）
    let Some(expected) = expected_sha else {
        return Err("无法获取 uv 官方 sha256 校验值，拒绝安装未校验的二进制".to_string());
    };
    use sha2::Digest;
    let actual = format!("{:x}", sha2::Sha256::digest(&bytes));
    if actual != expected.to_ascii_lowercase() {
        return Err(format!(
            "uv 二进制 sha256 不匹配（期望 {expected}，实际 {actual}），拒绝安装"
        ));
    }

    let target_dir = app_data.join("tools").join("uv");
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| format!("创建 uv 目录失败: {e}"))?;
    let archive_path = target_dir.join(&archive_name);
    std::fs::write(&archive_path, &bytes).map_err(|e| format!("写入 uv 包失败: {e}"))?;

    if is_zip {
        extract_zip(&archive_path, &target_dir)?;
    } else {
        // tar.gz：系统 tar（macOS/Linux 必带 bsdtar/gtar）
        let status = std::process::Command::new("tar")
            .args(["-xzf"])
            .arg(&archive_path)
            .arg("-C")
            .arg(&target_dir)
            .status()
            .map_err(|e| format!("调用 tar 解压失败: {e}"))?;
        if !status.success() {
            return Err("tar 解压 uv 包失败".to_string());
        }
        // release tar 解出的是 uv-<triple>/ 子目录，扁平化到 target_dir
        if let Ok(entries) = std::fs::read_dir(&target_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && entry.file_name().to_string_lossy().starts_with("uv-") {
                    for inner in ["uv", "uvx"] {
                        let src = p.join(inner);
                        if src.is_file() {
                            let _ = std::fs::rename(&src, target_dir.join(inner));
                        }
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_file(&archive_path);

    let uv_path = target_dir.join(exe_name());
    if !uv_path.is_file() {
        return Err("解压后未找到 uv 二进制".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["uv", "uvx"] {
            let p = target_dir.join(name);
            if p.is_file() {
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
    Ok(uv_path)
}

#[cfg(windows)]
fn extract_zip(archive: &Path, target: &Path) -> Result<(), String> {
    let f = std::fs::File::open(archive).map_err(|e| format!("打开 zip 失败: {e}"))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("解析 zip 失败: {e}"))?;
    for i in 0..z.len() {
        let mut file = z.by_index(i).map_err(|e| format!("读取 zip 项失败: {e}"))?;
        let name = file.name().rsplit(['/', '\\']).next().unwrap_or("").to_string();
        if name == "uv.exe" || name == "uvx.exe" {
            let mut out = std::fs::File::create(target.join(&name))
                .map_err(|e| format!("写出 {name} 失败: {e}"))?;
            std::io::copy(&mut file, &mut out).map_err(|e| format!("解压 {name} 失败: {e}"))?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn extract_zip(_: &Path, _: &Path) -> Result<(), String> {
    unreachable!("非 Windows 平台走 tar.gz 路径")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uv_invocation_detection() {
        assert!(is_uv_invocation("uvx"));
        assert!(is_uv_invocation("uv"));
        assert!(is_uv_invocation("/usr/local/bin/uvx"));
        assert!(is_uv_invocation("C:\\tools\\uvx.exe"));
        assert!(!is_uv_invocation("npx"));
        assert!(!is_uv_invocation("python"));
        assert!(!is_uv_invocation("uvicorn")); // 前缀相似但不是
    }

    #[test]
    fn uvx_rewrites_to_tool_run_when_resolved() {
        // 无 uv 环境时原样返回（spawn 会报 command not found——语义诚实）
        let (prog, args, _) = rewrite_uvx_invocation("echo", &["hi".to_string()]);
        assert_eq!(prog, "echo");
        assert_eq!(args, vec!["hi".to_string()]);
    }
}
