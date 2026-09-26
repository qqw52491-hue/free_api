//! Node.js 运行时就绪：npm 生态 stdio MCP 的零依赖入口（与 uv_runtime 对偶）。
//!
//! 解析顺序：进程内缓存 → PATH → app 数据目录缓存。
//! 全未命中时 `ensure_node` 从 nodejs.org 官方 dist 拉 latest LTS tarball
//! （先取 SHASUMS256.txt 拿文件名+校验值，sha256 不符拒装）。
//!
//! npm/npx 在 node dist 里是带 `#!/usr/bin/env node` shebang 的 JS 脚本——
//! 调用时必须把 node bin 目录 prepend 进 PATH 才跑得起来，rewrite 统一处理。
//!
//! 供应链纪律：只从 nodejs.org 官方 dist 拉取，强制 sha256 校验；
//! 缓存落 app 数据目录 tools/node/，不污染用户目录。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static RESOLVED_NODE: OnceLock<Option<PathBuf>> = OnceLock::new();

fn node_exe() -> &'static str {
    if cfg!(windows) { "node.exe" } else { "node" }
}

/// 命令是否为 node/npm/npx 调用（含绝对路径形式）。
pub fn is_node_invocation(command: &str) -> bool {
    let trimmed = command.trim();
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    matches!(
        base.trim_end_matches(".exe"),
        "node" | "npm" | "npx" | "npm.cmd" | "npx.cmd"
    )
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

/// node 缓存根目录：与 data_v2.db 同根的 free-api-chat/tools/node。
/// spawn 是同步上下文——按 db/mod.rs 的真实约定推导，不走 bundle id。
pub fn app_node_dir() -> Option<PathBuf> {
    let base = dirs_next::data_dir().or_else(dirs_next::data_local_dir)?;
    Some(base.join("free-api-chat").join("tools").join("node"))
}

fn bin_dir(root: &Path) -> PathBuf {
    if cfg!(windows) {
        root.to_path_buf() // win 发行包二进制平铺在根
    } else {
        root.join("bin")
    }
}

/// 同步解析可用 node 二进制（不联网）：缓存 → PATH → app 数据目录缓存。
/// npm/npx 命中时返回其同目录的 node（脚本需要 node 本体）。
pub fn resolve_node() -> Option<PathBuf> {
    if let Some(cached) = RESOLVED_NODE.get() {
        return cached.clone();
    }
    // 1. PATH 扫描（用户自装优先——brew/volta/nvm 的通常更新）
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let cand = dir.join(node_exe());
            if is_executable_file(&cand) {
                return Some(cand);
            }
        }
    }
    // 2. app 数据目录缓存（ensure_node 曾下载的副本）
    if let Some(root) = app_node_dir() {
        let cand = bin_dir(&root).join(node_exe());
        if is_executable_file(&cand) {
            return Some(cand);
        }
    }
    None
}

/// nodejs.org 平台段名 → dist 资源名后缀。
fn platform_suffix() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("darwin-arm64"),
        ("macos", "x86_64") => Some("darwin-x64"),
        ("linux", "x86_64") => Some("linux-x64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("windows", "x86_64") => Some("win-x64"),
        ("windows", "aarch64") => Some("win-arm64"),
        _ => None,
    }
}

/// spawn 前的命令改写：`node`/`npm`/`npx` → 解析到的绝对路径；
/// npm/npx 脚本经 PATH prepend 找到 node 本体。无法解析时原样返回。
/// 返回 (program, args, extra_env)。
pub fn rewrite_node_invocation(
    command: &str,
    args: &[String],
) -> (String, Vec<String>, Vec<(String, String)>) {
    if !is_node_invocation(command) {
        return (command.to_string(), args.to_vec(), Vec::new());
    }
    let Some(node) = resolve_node() else {
        return (command.to_string(), args.to_vec(), Vec::new());
    };
    let node_bin = node.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let base = command
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim_end_matches(".exe")
        .trim_end_matches(".cmd")
        .to_string();
    let program = match base.as_str() {
        "node" => node.to_string_lossy().to_string(),
        // npm/npx 是同目录脚本，直接以 node 运行对应 cli.js 最稳
        // （免 shebang 依赖 PATH 顺序）：npm → npm-cli.js，npx → npx-cli.js
        "npm" | "npx" => {
            let cli = node_bin
                .parent()
                .unwrap_or(&node_bin)
                .join("lib")
                .join("node_modules")
                .join("npm")
                .join("bin")
                .join(if base == "npm" { "npm-cli.js" } else { "npx-cli.js" });
            if cli.is_file() {
                let script = cli.to_string_lossy().to_string();
                let mut new_args = vec![script];
                new_args.extend(args.iter().cloned());
                return (
                    node.to_string_lossy().to_string(),
                    new_args,
                    vec![("PATH".to_string(), prepend_path(&node_bin))],
                );
            }
            // 布局异常退回脚本本体（依赖 PATH 内的 node）
            node_bin
                .join(if cfg!(windows) { format!("{base}.cmd") } else { base.clone() })
                .to_string_lossy()
                .to_string()
        }
        _ => return (command.to_string(), args.to_vec(), Vec::new()),
    };
    (program, args.to_vec(), vec![("PATH".to_string(), prepend_path(&node_bin))])
}

fn prepend_path(node_bin: &Path) -> String {
    let cur = std::env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ";" } else { ":" };
    format!("{}{sep}{cur}", node_bin.display())
}

/// 就绪 node：先同步解析，未命中则联网下载官方 dist 包。
/// app_root 为数据根目录（free-api-chat），供无 AppHandle 的上下文使用。
pub fn ensure_node_blocking(app_root: &Path) -> Result<PathBuf, String> {
    if let Some(p) = resolve_node() {
        return Ok(p);
    }
    let path = download_and_install(&app_root.join("tools").join("node"))?;
    let _ = RESOLVED_NODE.set(Some(path.clone()));
    Ok(path)
}

fn download_and_install(target_dir: &Path) -> Result<PathBuf, String> {
    let suffix = platform_suffix().ok_or_else(|| {
        format!("当前平台无可用 node 预编译包（{}）", std::env::consts::OS)
    })?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| format!("构建下载客户端失败: {e}"))?;

    // SHASUMS256.txt 同时给出文件名（自版本化）与校验值
    let sums_url = "https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt";
    let sums = client
        .get(sums_url)
        .send()
        .map_err(|e| format!("拉取 node 校验清单失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("node 校验清单 HTTP 错误: {e}"))?
        .text()
        .map_err(|e| format!("读取校验清单失败: {e}"))?;
    let archive_ext = if cfg!(windows) { ".zip" } else { ".tar.gz" };
    let needle = format!("-{suffix}{archive_ext}");
    let (expected, archive_name) = sums
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next()?.to_string()))
        })
        .find(|(_, f)| f.ends_with(&needle))
        .ok_or_else(|| format!("校验清单中找不到 {suffix} 的 node 包"))?;
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("node sha256 校验值格式异常，拒绝安装".to_string());
    }
    let archive_url = format!("https://nodejs.org/dist/latest-v22.x/{archive_name}");

    let bytes = client
        .get(&archive_url)
        .send()
        .map_err(|e| format!("下载 node 失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("下载 node HTTP 错误: {e}"))?
        .bytes()
        .map_err(|e| format!("读取 node 包体失败: {e}"))?;

    use sha2::Digest;
    let actual = format!("{:x}", sha2::Sha256::digest(&bytes));
    if actual != expected.to_ascii_lowercase() {
        return Err(format!(
            "node 包 sha256 不匹配（期望 {expected}，实际 {actual}），拒绝安装"
        ));
    }

    std::fs::create_dir_all(target_dir).map_err(|e| format!("创建 node 目录失败: {e}"))?;
    let archive_path = target_dir.join(&archive_name);
    std::fs::write(&archive_path, &bytes).map_err(|e| format!("写入 node 包失败: {e}"))?;

    if archive_name.ends_with(".zip") {
        extract_zip(&archive_path, target_dir)?;
    } else {
        let status = std::process::Command::new("tar")
            .args(["-xzf"])
            .arg(&archive_path)
            .arg("-C")
            .arg(target_dir)
            .status()
            .map_err(|e| format!("调用 tar 解压失败: {e}"))?;
        if !status.success() {
            return Err("tar 解压 node 包失败".to_string());
        }
        // node-vX.Y.Z-<suffix>/ 子目录扁平化到 target_dir
        if let Ok(entries) = std::fs::read_dir(target_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && entry.file_name().to_string_lossy().starts_with("node-") {
                    for sub in ["bin", "lib", "include", "share"] {
                        let src = p.join(sub);
                        if src.exists() {
                            let _ = copy_dir_recursive(&src, &target_dir.join(sub));
                        }
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_file(&archive_path);

    let node_path = bin_dir(target_dir).join(node_exe());
    if !node_path.is_file() {
        return Err("解压后未找到 node 二进制".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&node_path, std::fs::Permissions::from_mode(0o755));
        for name in ["npm", "npx"] {
            let p = bin_dir(target_dir).join(name);
            if p.is_file() {
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
    Ok(node_path)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let (s, d) = (entry.path(), dst.join(entry.file_name()));
        if s.is_dir() {
            copy_dir_recursive(&s, &d)?;
        } else {
            std::fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn extract_zip(archive: &Path, target: &Path) -> Result<(), String> {
    let f = std::fs::File::open(archive).map_err(|e| format!("打开 zip 失败: {e}"))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("解析 zip 失败: {e}"))?;
    for i in 0..z.len() {
        let mut file = z.by_index(i).map_err(|e| format!("读取 zip 项失败: {e}"))?;
        if file.is_dir() {
            continue;
        }
        // node zip 内为 node-vX/… 单根目录，剥掉首层
        let rel = file.name().split('/').skip(1).collect::<Vec<_>>().join("/");
        if rel.is_empty() {
            continue;
        }
        let out_path = target.join(&rel);
        if let Some(parent) = out_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut out = std::fs::File::create(&out_path)
            .map_err(|e| format!("写出 {rel} 失败: {e}"))?;
        std::io::copy(&mut file, &mut out).map_err(|e| format!("解压 {rel} 失败: {e}"))?;
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
    fn node_invocation_detection() {
        assert!(is_node_invocation("node"));
        assert!(is_node_invocation("npm"));
        assert!(is_node_invocation("npx"));
        assert!(is_node_invocation("/usr/local/bin/node"));
        assert!(!is_node_invocation("python"));
        assert!(!is_node_invocation("nodejs-but-other")); // 相似前缀不算
        assert!(!is_node_invocation("uvx"));
    }
}
