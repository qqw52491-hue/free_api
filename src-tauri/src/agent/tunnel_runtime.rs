//! cloudflared 运行时就绪：内网穿透通道的零依赖入口（与 uv/node_runtime 对偶）。
//!
//! 解析顺序：进程内缓存 → PATH → app 数据目录缓存。
//! 全未命中时 `ensure_cloudflared` 从 cloudflare/cloudflared GitHub releases
//! 拉取钉死版本的官方二进制，sha256 不符拒装。
//!
//! 供应链纪律：releases 不提供 checksums 清单，改为按平台硬编码
//! 钉死版本的 sha256（取包时离线校验过官方产物）；缓存落 app 数据目录
//! tools/cloudflared/，不污染用户目录。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static RESOLVED_CF: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 钉死的 cloudflared 版本（与 checksum 常量一一对应）
const CF_VERSION: &str = "2026.9.1";

fn cf_exe() -> &'static str {
    if cfg!(windows) { "cloudflared.exe" } else { "cloudflared" }
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

/// cloudflared 缓存目录：与 data_v2.db 同根的 free-api-chat/tools/cloudflared。
pub fn app_cloudflared_dir() -> Option<PathBuf> {
    let base = dirs_next::data_dir().or_else(dirs_next::data_local_dir)?;
    Some(base.join("free-api-chat").join("tools").join("cloudflared"))
}

/// 同步解析可用 cloudflared 二进制（不联网）：缓存 → PATH → app 数据目录。
pub fn resolve_cloudflared() -> Option<PathBuf> {
    if let Some(cached) = RESOLVED_CF.get() {
        return cached.clone();
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let cand = dir.join(cf_exe());
            if is_executable_file(&cand) {
                return Some(cand);
            }
        }
    }
    if let Some(root) = app_cloudflared_dir() {
        let cand = root.join(cf_exe());
        if is_executable_file(&cand) {
            return Some(cand);
        }
    }
    None
}

/// (资产名, 期望 sha256)。校验值取自 2026.9.1 官方 release 实包。
fn platform_asset() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some((
            "cloudflared-darwin-arm64.tgz",
            "c27ab8fd0aa489449e3d201eb02f957ef460a13b613662928b1b23394bf1bcfe",
        )),
        ("macos", "x86_64") => Some((
            "cloudflared-darwin-amd64.tgz",
            "ff0d3b51d5ff70eceef89d6b32145fee985018a2174596a5dbe405e2766e2ac4",
        )),
        ("linux", "x86_64") => Some((
            "cloudflared-linux-amd64",
            "03f1f25d1cc93b9ad6c60569d44060bc4f17ed97075760ed8cfca4b12dcd68cc",
        )),
        ("linux", "aarch64") => Some((
            "cloudflared-linux-arm64",
            "3d97437c71848bd8df68041e12436b484a661d95073ea1937f01a845ce88faa3",
        )),
        ("windows", "x86_64") => Some((
            "cloudflared-windows-amd64.exe",
            "2837888cc0f5d58f15b6dc478376de90b4d3ba5241c7947455d1e0a0df429712",
        )),
        _ => None,
    }
}

/// 就绪 cloudflared：先同步解析，未命中则联网下载官方包。
/// app_root 为数据根目录（free-api-chat），供无 AppHandle 的上下文使用。
pub fn ensure_cloudflared_blocking(app_root: &Path) -> Result<PathBuf, String> {
    if let Some(p) = resolve_cloudflared() {
        return Ok(p);
    }
    let path = download_and_install(&app_root.join("tools").join("cloudflared"))?;
    let _ = RESOLVED_CF.set(Some(path.clone()));
    Ok(path)
}

fn download_and_install(target_dir: &Path) -> Result<PathBuf, String> {
    let (asset, expected) = platform_asset().ok_or_else(|| {
        format!(
            "当前平台无可用 cloudflared 预编译包（{}/{}）",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let url = format!(
        "https://github.com/cloudflare/cloudflared/releases/download/{CF_VERSION}/{asset}"
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| format!("构建下载客户端失败: {e}"))?;
    let bytes = client
        .get(&url)
        .send()
        .map_err(|e| format!("下载 cloudflared 失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("下载 cloudflared HTTP 错误: {e}"))?
        .bytes()
        .map_err(|e| format!("读取 cloudflared 包体失败: {e}"))?;

    use sha2::Digest;
    let actual = format!("{:x}", sha2::Sha256::digest(&bytes));
    if actual != expected {
        return Err(format!(
            "cloudflared 包 sha256 不匹配（期望 {expected}，实际 {actual}），拒绝安装"
        ));
    }

    std::fs::create_dir_all(target_dir).map_err(|e| format!("创建 cloudflared 目录失败: {e}"))?;
    let bin_path = target_dir.join(cf_exe());

    if asset.ends_with(".tgz") {
        // macOS 发行是 tgz，内含单个 cloudflared 二进制
        let archive = target_dir.join(asset);
        std::fs::write(&archive, &bytes).map_err(|e| format!("写入 cloudflared 包失败: {e}"))?;
        let status = std::process::Command::new("tar")
            .args(["-xzf"])
            .arg(&archive)
            .arg("-C")
            .arg(target_dir)
            .status()
            .map_err(|e| format!("调用 tar 解压失败: {e}"))?;
        let _ = std::fs::remove_file(&archive);
        if !status.success() {
            return Err("tar 解压 cloudflared 包失败".to_string());
        }
    } else {
        // linux/windows 资产即二进制本体
        std::fs::write(&bin_path, &bytes).map_err(|e| format!("写入 cloudflared 失败: {e}"))?;
    }

    if !bin_path.is_file() {
        return Err("解压/写入后未找到 cloudflared 二进制".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(bin_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_asset_covers_desktop_os() {
        // 本测试仅在受支持平台上有意义；跑在受支持平台时必须非空
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
            all(target_os = "windows", target_arch = "x86_64"),
        ))]
        assert!(platform_asset().is_some());
    }

    #[test]
    fn asset_sha256_are_hex64() {
        if let Some((name, sum)) = platform_asset() {
            assert!(name.starts_with("cloudflared-"));
            assert_eq!(sum.len(), 64);
            assert!(sum.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
