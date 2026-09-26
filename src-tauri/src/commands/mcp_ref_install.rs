//! 引用式 MCP 安装：一段自由文本 → 可执行插件配置。
//!
//! 支持三种引用形态（粘贴即装，无需知道包管理器）：
//! - 包名（`excel-mcp-server` / `@scope/pkg`）→ 先试 `uvx`，失败退 `npx -y`
//! - GitHub/Gitee 仓库地址 → clone 到 `<app_data>/mcp_repos/<name>` 后按本地目录解析
//! - 本地目录路径（绝对路径或 `~/...`）→ 原地解析
//!
//! 目录解析规则（自上而下首个命中）：
//!   `pyproject.toml`（[project.scripts] → `uv run --directory`）→
//!   `package.json`（bin/main → `node <entry>`；入口缺失先 npm install/build）→
//!   根目录单文件入口（server.py / main.py / server.js / index.js）。
//!
//! 每种形态产出的候选配置都过真实握手（spawn+initialize+tools/list），
//! 握手不通的不落库——与 registry 安装同一验证强度。

use crate::agent::mcp::{McpClient, McpToolDef, PluginConfig};
use crate::db::DbState;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{Manager, State};

/// 安装结果（与 registry 安装对齐前端展示）。
#[derive(Debug, Clone, Serialize)]
pub struct RefInstallResult {
    pub plugin_name: String,
    pub command: String,
    pub args: Vec<String>,
    pub tool_count: usize,
    /// 解析出的引用形态（git_repo / local_dir / package_name / mcpb_bundle）
    pub resolved_kind: String,
    /// 清单声明的必填环境变量键名（manifest user_config / env 占位符提取）。
    /// 前端凭此渲染密钥表单；值为空则无需引导。
    pub required_env: Vec<String>,
}

/// 引用分类。
enum RefKind {
    /// github/gitee 仓库地址——clone 到托管目录
    GitRepo(String),
    /// 本地已存在的目录
    LocalDir(PathBuf),
    /// .mcpb 官方打包文件（zip+manifest）——解包到托管目录
    McpbBundle(PathBuf),
    /// 裸包名（npm 或 pypi，握手定归属）
    PackageName(String),
}

fn classify_reference(reference: &str) -> Result<RefKind, String> {
    let r = reference.trim();
    if r.is_empty() {
        return Err("请输入包名、GitHub 地址或本地目录路径".to_string());
    }
    if r.starts_with("https://github.com/")
        || r.starts_with("http://github.com/")
        || r.starts_with("git@github.com:")
        || r.starts_with("https://gitee.com/")
        || r.starts_with("git@gitee.com:")
    {
        return Ok(RefKind::GitRepo(r.trim_end_matches('/').to_string()));
    }
    // 本地目录/文件：~、/、./、../ 开头且真实存在
    let expanded = if let Some(rest) = r.strip_prefix("~/") {
        dirs_next::home_dir().map(|h| h.join(rest))
    } else {
        Some(PathBuf::from(r))
    };
    if let Some(p) = expanded {
        if p.is_dir() {
            return Ok(RefKind::LocalDir(p));
        }
        // .mcpb 官方打包（zip+manifest）：文件存在且后缀匹配
        if p.is_file()
            && p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("mcpb"))
                .unwrap_or(false)
        {
            return Ok(RefKind::McpbBundle(p));
        }
    }
    if r.chars().all(|c| c.is_alphanumeric() || "._-/@".contains(c)) && !r.contains(' ') {
        return Ok(RefKind::PackageName(r.to_string()));
    }
    Err(format!(
        "无法识别的引用形态: {r}。支持：包名 / github.com|gitee.com 仓库地址 / 本地目录路径 / .mcpb 打包文件。"
    ))
}

/// 与 registry 安装同强度：spawn+initialize+list_tools，120s 封顶。
async fn handshake(config: PluginConfig) -> Result<(PluginConfig, Vec<McpToolDef>), String> {
    let client =
        McpClient::spawn(config.clone()).map_err(|e| format!("启动进程失败: {e}"))?;
    let tools = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        client.initialize().await?;
        client.list_tools().await
    })
    .await
    .map_err(|_| "握手超时（120s）".to_string())?
    .map_err(|e| format!("MCP 初始化失败: {e}"))?;
    Ok((config, tools))
}

/// package.json 目录预处理：入口文件不存在且有依赖要装时，
/// 先 `npm install --omit=dev`，有 build 脚本再 `npm run build`。
/// 全部失败只记日志不致命——后续握手自然败出并带原错。
async fn prepare_node_dir(dir: &Path, entry_exists: bool, has_build: bool) {
    if entry_exists && dir.join("node_modules").is_dir() {
        return;
    }
    // npm 经 rewrite 解析（PATH → app 缓存），自带 PATH prepend 找 node 本体。
    let (npm_prog, npm_args, npm_env) =
        crate::agent::node_runtime::rewrite_node_invocation("npm", &[]);
    let mk_npm = |extra: &[&str]| {
        let mut c = tokio::process::Command::new(&npm_prog);
        c.args(&npm_args).args(extra).current_dir(dir);
        for (k, v) in &npm_env {
            c.env(k, v);
        }
        c
    };
    let install = mk_npm(&["install", "--omit=dev"]).output().await;
    match install {
        Ok(o) if !o.status.success() => {
            eprintln!(
                "[mcp_ref_install] npm install 失败: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return;
        }
        Err(e) => {
            eprintln!("[mcp_ref_install] npm 不可用: {e}");
            return;
        }
        _ => {}
    }
    if has_build && !entry_exists {
        let build = mk_npm(&["run", "build"]).output().await;
        if let Ok(o) = build {
            if !o.status.success() {
                eprintln!(
                    "[mcp_ref_install] npm run build 失败: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                );
            }
        }
    }
}

/// 解析本地目录 → 候选启动配置列表（按优先级排序）。
async fn resolve_local_dir(dir: &Path, name: &str) -> Result<Vec<PluginConfig>, String> {
    let mut candidates: Vec<PluginConfig> = Vec::new();
    let dir_str = dir.to_string_lossy().to_string();
    let mk = |command: &str, args: Vec<String>| PluginConfig {
        name: name.to_string(),
        command: command.to_string(),
        args,
        url: None,
        headers: Default::default(),
        env: Default::default(),
        capability_card: None,
    };

    // 0) manifest.json（mcpb 官方清单）→ server.mcp_config 权威启动姿势。
    //    生态自带的答案文件，优先级高于一切启发式推断。
    let manifest = dir.join("manifest.json");
    if manifest.is_file() {
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            if let Ok(m) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(cfg) = m.pointer("/server/mcp_config") {
                    let command = cfg.get("command").and_then(|v| v.as_str());
                    let args = cfg
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if let Some(command) = command {
                        if !command.is_empty() {
                            candidates.push(mk(command, args));
                        }
                    }
                }
                // entry_point 兜底：清单给了入口但没给 mcp_config 时按 type 映射
                if let Some(entry) = m.pointer("/server/entry_point").and_then(|v| v.as_str()) {
                    if dir.join(entry).is_file() {
                        let cmd = match m.pointer("/server/type").and_then(|v| v.as_str()) {
                            Some("node") => "node",
                            _ => "uv",
                        };
                        let args = if cmd == "uv" {
                            vec![
                                "run".into(),
                                "--directory".into(),
                                dir_str.clone(),
                                entry.into(),
                            ]
                        } else {
                            vec![dir.join(entry).to_string_lossy().to_string()]
                        };
                        candidates.push(mk(cmd, args));
                    }
                }
            }
        }
    }

    // 1) pyproject.toml → uv run --directory <dir>
    let pyproject = dir.join("pyproject.toml");
    let pyproject_text = if pyproject.is_file() {
        std::fs::read_to_string(&pyproject).ok()
    } else {
        None
    };
    if pyproject.is_file() {
        if let Some(text) = pyproject_text.as_deref() {
            if let Some(scripts_pos) = text.find("[project.scripts]") {
                let tail = &text[scripts_pos..];
                if let Some(line) = tail
                    .lines()
                    .skip(1)
                    .map(str::trim)
                    .take_while(|l| !l.starts_with('['))
                    .find(|l| !l.is_empty() && l.contains('='))
                {
                    let script = line.split('=').next().unwrap_or("").trim();
                    if !script.is_empty() {
                        // CLI 类入口（typer/click）裸跑打帮助就退出——除裸调用外
                        // 枚举 MCP 社区约定的 stdio 传输子命令，握手自然筛出真的。
                        for extra in [
                            vec![],
                            vec!["stdio"],
                            vec!["serve", "--transport", "stdio"],
                        ] {
                            let mut args = vec![
                                "run".into(),
                                "--directory".into(),
                                dir_str.clone(),
                                script.to_string(),
                            ];
                            args.extend(extra.iter().map(|s| s.to_string()));
                            candidates.push(mk("uv", args));
                        }
                    }
                }
            }
        }
        for entry in ["server.py", "main.py", "src/main.py"] {
            if dir.join(entry).is_file() {
                candidates.push(mk(
                    "uv",
                    vec![
                        "run".into(),
                        "--directory".into(),
                        dir_str.clone(),
                        entry.into(),
                    ],
                ));
            }
        }
    }

    // 2) package.json → node <bin|main>（入口缺失先 npm 预处理）
    let pkg_json = dir.join("package.json");
    if pkg_json.is_file() {
        if let Ok(text) = std::fs::read_to_string(&pkg_json) {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) {
                let entry = pkg
                    .get("bin")
                    .and_then(|b| {
                        b.as_str()
                            .map(String::from)
                            .or_else(|| b.as_object()?.values().next()?.as_str().map(String::from))
                    })
                    .or_else(|| pkg.get("main").and_then(|m| m.as_str().map(String::from)));
                if let Some(entry) = entry {
                    let entry_path = dir.join(&entry);
                    let has_build = pkg
                        .get("scripts")
                        .and_then(|s| s.get("build"))
                        .is_some();
                    if !entry_path.is_file() {
                        prepare_node_dir(dir, false, has_build).await;
                    } else if !dir.join("node_modules").is_dir() {
                        prepare_node_dir(dir, true, false).await;
                    }
                    if entry_path.is_file() {
                        candidates.push(mk(
                            "node",
                            vec![entry_path.to_string_lossy().to_string()],
                        ));
                    }
                }
            }
        }
    }

    // 3) 根目录单文件入口（node 留裸命令——spawn 期 rewrite_node_invocation 解析）
    for (file, cmd) in [
        ("server.py", "uv"),
        ("main.py", "uv"),
        ("server.js", "node"),
        ("index.js", "node"),
    ] {
        if dir.join(file).is_file() {
            let base_args = if cmd == "uv" {
                vec![
                    "run".into(),
                    "--directory".into(),
                    dir_str.clone(),
                    file.into(),
                ]
            } else {
                vec![dir.join(file).to_string_lossy().to_string()]
            };
            candidates.push(mk(cmd, base_args.clone()));
            // CLI 型入口（argparse/typer）需显式 transport 子命令
            let mut stdio_args = base_args;
            stdio_args.push("stdio".into());
            candidates.push(mk(cmd, stdio_args));
        }
    }

    // Python 版本退避：uv.lock/pyproject 钉死的老依赖在新 Python 下无预编译
    // wheel（pydantic-core 源码编译事故实测）——给每个 `uv run` 候选补
    // `--python` 变体殿后，uv 自动下载受管解释器，不污染主候选顺序。
    // 版本选择遵守 requires-python 声明的下界（3.13 优先：PyO3 系 wheel 上限）。
    let py_fallbacks: Vec<&str> = {
        let mut v = Vec::new();
        let lower_bound = pyproject_text
            .as_deref()
            .and_then(|t| python_lower_bound(t));
        for cand in ["3.13", "3.12"] {
            if lower_bound.map_or(true, |lb| version_satisfies_lower(cand, lb)) {
                v.push(cand);
            }
        }
        v
    };
    let mut py_fallbacks_cfgs: Vec<PluginConfig> = Vec::new();
    for c in &candidates {
        if c.command == "uv" && c.args.first().map(|s| s.as_str()) == Some("run") {
            for ver in &py_fallbacks {
                let mut args = c.args.clone();
                args.splice(1..1, ["--python".to_string(), ver.to_string()]);
                py_fallbacks_cfgs.push(PluginConfig { args, ..c.clone() });
            }
        }
    }
    candidates.extend(py_fallbacks_cfgs);

    if candidates.is_empty() {
        return Err(format!(
            "目录 {} 下没认出可运行形态：需要 pyproject.toml / package.json / server.py / main.py / server.js / index.js 之一",
            dir.display()
        ));
    }
    Ok(candidates)
}

/// 清单声明的必填 env 键名收集：
/// 1) manifest.user_config 中 required/sensitive 字段名（mcpb 规范）；
/// 2) server.mcp_config.env 值里的 ${user_config.X} / ${X} 占位符引用。
/// 只产键名不产值——密钥收集归前端表单，后端不碰明文。
fn manifest_required_env(dir: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(dir.join("manifest.json")) else {
        return Vec::new();
    };
    let Ok(m) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    if let Some(cfg) = m.get("user_config").and_then(|v| v.as_object()) {
        for (key, spec) in cfg {
            let required = spec
                .get("required")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let sensitive = spec
                .get("sensitive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if required || sensitive {
                out.push(key.clone());
            }
        }
    }
    if let Some(env) = m.pointer("/server/mcp_config/env").and_then(|v| v.as_object()) {
        for (_, v) in env {
            if let Some(s) = v.as_str() {
                for cap in s.match_indices("${") {
                    let rest = &s[cap.0 + 2..];
                    let name = rest
                        .split('}')
                        .next()
                        .unwrap_or("")
                        .trim_start_matches("user_config.");
                    if !name.is_empty() && !out.iter().any(|x| x == name) {
                        out.push(name.to_string());
                    }
                }
            }
        }
    }
    out
}

/// pyproject 里 requires-python 的下界版本号（>=X.Y / ==X.Y 等），
/// 解析不出返回 None（视为无约束）。
fn python_lower_bound(pyproject_text: &str) -> Option<(u32, u32)> {
    let pos = pyproject_text.find("requires-python")?;
    let spec = pyproject_text[pos..].lines().next()?;
    for op in [">=", "==", "~="] {
        if let Some(idx) = spec.find(op) {
            let ver = spec[idx + op.len()..]
                .trim_start_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace())
                .split(|c: char| !c.is_ascii_digit() && c != '.')
                .next()
                .unwrap_or("");
            let mut it = ver.split('.');
            let major = it.next()?.parse().ok()?;
            let minor = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            return Some((major, minor));
        }
    }
    None
}

/// 版本号是否满足下界（major.minor 粗粒度比较，退避候选够用）。
fn version_satisfies_lower(ver: &str, lower: (u32, u32)) -> bool {
    let mut it = ver.split('.');
    let major = it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    let minor = it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    (major, minor) >= lower
}

/// Git 仓库 → clone 到 <repos_dir>/<name>；已克隆则 fast-forward。
async fn materialize_git_repo(
    repos_dir: &Path,
    url: &str,
    name: &str,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(repos_dir).map_err(|e| e.to_string())?;
    let target = repos_dir.join(name);
    if target.join(".git").is_dir() && git_available().await {
        let out = tokio::process::Command::new("git")
            .args(["pull", "--ff-only"])
            .current_dir(&target)
            .output()
            .await
            .map_err(|e| format!("git pull 失败: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "已克隆仓库更新失败: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        return Ok(target);
    }
    if git_available().await {
        let out = tokio::process::Command::new("git")
            .args(["clone", "--depth", "1", url])
            .arg(&target)
            .output()
            .await
            .map_err(|e| format!("git clone 失败（需要 git 在 PATH）: {e}"))?;
        if out.status.success() {
            return Ok(target);
        }
        eprintln!(
            "[mcp_ref_install] git clone 失败，退回 codeload zip 通道: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // 无 git 兜底：GitHub codeload 官方 zip——纯 HTTP+解压，
    // Windows 裸机（无 git）也能拿到完整仓库快照。
    download_repo_zip(url, &target).await
}

async fn git_available() -> bool {
    tokio::process::Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// GitHub codeload zip 下载（无 git 兜底通道）：
/// https://codeload.github.com/<owner>/<repo>/zip/refs/heads/<main|master>
/// zip 内首层目录为 <repo>-<branch>/，剥掉后平铺到 target。
async fn download_repo_zip(url: &str, target: &Path) -> Result<PathBuf, String> {
    let path = url
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let mut parts = path.split('/');
    if parts.next() != Some("github.com") {
        return Err(format!("非 GitHub 地址无法走 zip 通道: {url}"));
    }
    let (owner, repo) = (
        parts.next().ok_or("GitHub 地址缺 owner")?,
        parts.next().ok_or("GitHub 地址缺 repo")?,
    );
    let repo = repo.trim_end_matches(".git");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| format!("构建下载客户端失败: {e}"))?;
    let mut bytes = None;
    let mut last_err = String::new();
    for branch in ["main", "master", "HEAD"] {
        let zip_url =
            format!("https://codeload.github.com/{owner}/{repo}/zip/refs/heads/{branch}");
        match client.get(&zip_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                bytes = Some(
                    resp.bytes()
                        .await
                        .map_err(|e| format!("读取仓库 zip 失败: {e}"))?,
                );
                break;
            }
            Ok(resp) => last_err = format!("{branch} 分支 HTTP {}", resp.status()),
            Err(e) => last_err = format!("{branch} 分支请求失败: {e}"),
        }
    }
    let bytes = bytes.ok_or_else(|| format!("仓库 zip 下载均失败: {last_err}"))?;

    // 解压到临时目录再整体搬入 target（避免半成品状态）
    let staging = target.with_extension("staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let archive_path = staging.join("repo.zip");
    std::fs::write(&archive_path, &bytes).map_err(|e| format!("写入 zip 失败: {e}"))?;
    {
        let f = std::fs::File::open(&archive_path).map_err(|e| e.to_string())?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| format!("解析 zip 失败: {e}"))?;
        for i in 0..z.len() {
            let mut file = z.by_index(i).map_err(|e| e.to_string())?;
            // 剥掉首层 <repo>-<branch>/ 前缀
            let rel = file
                .name()
                .split('/')
                .skip(1)
                .collect::<Vec<_>>()
                .join("/");
            if rel.is_empty() {
                continue;
            }
            let out_path = staging.join(&rel);
            if file.is_dir() {
                std::fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
                continue;
            }
            if let Some(p) = out_path.parent() {
                std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&out_path).map_err(|e| e.to_string())?;
            std::io::copy(&mut file, &mut out).map_err(|e| e.to_string())?;
        }
    }
    let _ = std::fs::remove_file(&archive_path);
    let _ = std::fs::remove_dir_all(target);
    std::fs::rename(&staging, target).map_err(|e| format!("搬入仓库目录失败: {e}"))?;
    Ok(target.to_path_buf())
}

/// .mcpb 打包文件解包：zip 整体解到 repos_dir/<name>/，manifest.name 为权威插件名。
/// 解出后的目录结构与 git clone 产物等价（manifest.json 在根），
/// 直接走 resolve_local_dir 的 manifest 分支拿权威启动配置。
async fn materialize_mcpb(mcpb: &Path, repos_dir: &Path) -> Result<(PathBuf, String), String> {
    let f = std::fs::File::open(mcpb).map_err(|e| format!("打开 .mcpb 失败: {e}"))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("解析 .mcpb 失败（不是合法 zip）: {e}"))?;

    // 先找 manifest 拿权威名；没有 manifest 就不是 mcpb
    let mut plugin_name = mcpb
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mcpb-plugin")
        .to_string();
    for i in 0..z.len() {
        let mut file = z.by_index(i).map_err(|e| e.to_string())?;
        if file.name().ends_with("manifest.json") && file.name().matches('/').count() <= 1 {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut file, &mut text).map_err(|e| e.to_string())?;
            if let Ok(m) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(n) = m.get("name").and_then(|v| v.as_str()) {
                    if !n.is_empty() {
                        plugin_name = n.to_string();
                    }
                }
            }
            break;
        }
    }

    let target = repos_dir.join(&plugin_name);
    let staging = target.with_extension("staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    // 整体解出（剥可选的首层包装目录——有的 bundle 内容直接平铺，有的套一层）
    let mut top_dirs = std::collections::BTreeSet::new();
    for i in 0..z.len() {
        if let Ok(file) = z.by_index(i) {
            if let Some(first) = file.name().split('/').next() {
                if !first.is_empty() {
                    top_dirs.insert(first.to_string());
                }
            }
        }
    }
    let strip_prefix = if top_dirs.len() == 1
        && !top_dirs.contains("manifest.json")
    {
        top_dirs.into_iter().next()
    } else {
        None
    };

    let mut found_manifest = false;
    for i in 0..z.len() {
        let mut file = z.by_index(i).map_err(|e| e.to_string())?;
        let raw = file.name().to_string();
        let rel = if let Some(p) = &strip_prefix {
            raw.strip_prefix(&format!("{p}/")).unwrap_or(&raw).to_string()
        } else {
            raw.clone()
        };
        if rel.is_empty() || rel.ends_with('/') {
            continue;
        }
        if rel == "manifest.json" {
            found_manifest = true;
        }
        let out_path = staging.join(&rel);
        if let Some(p) = out_path.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let mut out = std::fs::File::create(&out_path).map_err(|e| e.to_string())?;
        std::io::copy(&mut file, &mut out).map_err(|e| e.to_string())?;
        // 保留可执行位（bundle 内二进制需要）
        #[cfg(unix)]
        if let Some(mode) = file.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode));
        }
    }
    if !found_manifest {
        let _ = std::fs::remove_dir_all(&staging);
        return Err("文件解出后未找到 manifest.json——不是合法 .mcpb 打包".to_string());
    }
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&staging, &target).map_err(|e| format!("搬入解包目录失败: {e}"))?;
    Ok((target, plugin_name))
}
/// 克隆落点统一约定：与 data_v2.db 同根（data_dir/free-api-chat/mcp_repos）。
/// UI 与 mcp_manager 两条入口必须共用——Tauri app_data_dir 解析到
/// bundle id 目录（com.free-api-chat.dev），与真实数据根不一致。
pub fn default_repos_dir() -> PathBuf {
    dirs_next::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("free-api-chat")
        .join("mcp_repos")
}

/// 主命令：自由引用 → 解析 → 握手验证 → 落库。
#[tauri::command]
pub async fn install_mcp_from_ref(
    _app: tauri::AppHandle,
    state: State<'_, DbState>,
    reference: String,
) -> Result<RefInstallResult, String> {
    install_from_ref_core(state.inner(), &reference, &default_repos_dir()).await
}

/// 核心实现：UI 命令与 mcp_manager(install_ref) 共用。
/// repos_dir 由调用方解析（UI 走 app_data_dir，agent 侧走约定目录）。
pub async fn install_from_ref_core(
    state: &DbState,
    reference: &str,
    repos_dir: &Path,
) -> Result<RefInstallResult, String> {
    let kind = classify_reference(reference)?;

    let mut resolved_dir: Option<PathBuf> = None;
    let (plugin_name, candidates, resolved_kind) = match kind {
        RefKind::PackageName(name) => {
            // 作用域包名取后段会撞名（@playwright/mcp → "mcp"）；
            // 插件名用完整形（@scope/pkg → scope-pkg），有辨识度且唯一。
            let short = name
                .trim_start_matches('@')
                .replace('/', "-")
                .to_string();
            let mk = |nm: &str, command: &str, args: Vec<String>| PluginConfig {
                name: nm.to_string(),
                command: command.to_string(),
                args,
                url: None,
                headers: Default::default(),
                env: Default::default(),
                capability_card: None,
            };
            let candidates = vec![
                mk(&short, "uvx", vec![name.clone()]),
                mk(&short, "npx", vec!["-y".into(), name.clone()]),
            ];
            (short, candidates, "package_name")
        }
        RefKind::LocalDir(dir) => {
            let name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("local-plugin")
                .to_string();
            let cands = resolve_local_dir(&dir, &name).await?;
            resolved_dir = Some(dir);
            (name, cands, "local_dir")
        }
        RefKind::GitRepo(url) => {
            let name = url
                .trim_end_matches(".git")
                .rsplit('/')
                .next()
                .unwrap_or("repo-plugin")
                .to_string();
            let dir = materialize_git_repo(repos_dir, &url, &name).await?;
            let cands = resolve_local_dir(&dir, &name).await?;
            resolved_dir = Some(dir);
            (name, cands, "git_repo")
        }
        RefKind::McpbBundle(path) => {
            let (dir, name) = materialize_mcpb(&path, repos_dir).await?;
            let cands = resolve_local_dir(&dir, &name).await?;
            resolved_dir = Some(dir);
            (name, cands, "mcpb_bundle")
        }
    };

    // 安装期运行时预检：候选里有 node/npm/npx 调用但解析不到 node 时，
    // 联网自动就绪（nodejs.org 官方 dist + sha256）。失败不致命——
    // 握手自然败出并带 command not found 原错。
    let needs_node = candidates
        .iter()
        .any(|c| crate::agent::node_runtime::is_node_invocation(&c.command));
    if needs_node && crate::agent::node_runtime::resolve_node().is_none() {
        let root = repos_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| repos_dir.to_path_buf());
        let res = tokio::task::spawn_blocking(move || {
            crate::agent::node_runtime::ensure_node_blocking(&root)
        })
        .await;
        if let Err(e) = res.map_err(|e| e.to_string()).and_then(|r| r.map(|_| ())) {
            eprintln!("[mcp_ref_install] node 自动就绪失败（握手将以 command not found 败出）: {e}");
        }
    }

    let mut last_err = String::new();
    for config in candidates {
        match handshake(config.clone()).await {
            Ok((cfg, tools)) => {
                crate::agent::mcp_catalog::remember_tools(&cfg, &tools);
                let tool_count = tools.len();
                let conn = state.0.lock().map_err(|e| e.to_string())?;
                let id = uuid::Uuid::new_v4().to_string();
                let now = chrono::Utc::now().to_rfc3339();
                conn.execute(
                    "INSERT INTO mcp_plugins (id, name, command, args, env, capability_card, enabled, created_at, url, headers)
                     VALUES (?1, ?2, ?3, ?4, '{}', '', 1, ?5, '', '{}')
                     ON CONFLICT(name) DO UPDATE SET
                        command = excluded.command,
                        args = excluded.args,
                        enabled = 1",
                    rusqlite::params![
                        id,
                        plugin_name,
                        cfg.command,
                        serde_json::to_string(&cfg.args).map_err(|e| e.to_string())?,
                        now,
                    ],
                )
                .map_err(|e| format!("保存插件失败: {e}"))?;
                crate::commands::record_plugin_health(state, &plugin_name, Ok(tool_count));
                let required_env = resolved_dir
                    .as_deref()
                    .map(manifest_required_env)
                    .unwrap_or_default();
                return Ok(RefInstallResult {
                    plugin_name,
                    command: cfg.command,
                    args: cfg.args,
                    tool_count,
                    resolved_kind: resolved_kind.to_string(),
                    required_env,
                });
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!("所有候选启动方式均握手失败。最后错误: {last_err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_reference_routes_three_shapes() {
        assert!(matches!(
            classify_reference("https://github.com/org/repo").unwrap(),
            RefKind::GitRepo(_)
        ));
        assert!(matches!(
            classify_reference("excel-mcp-server").unwrap(),
            RefKind::PackageName(_)
        ));
        assert!(matches!(
            classify_reference("@scope/mcp-pkg").unwrap(),
            RefKind::PackageName(_)
        ));
        let tmp = std::env::temp_dir();
        assert!(matches!(
            classify_reference(tmp.to_str().unwrap()).unwrap(),
            RefKind::LocalDir(_)
        ));
        assert!(classify_reference("..\\not a path with spaces").is_err());
        assert!(classify_reference("").is_err());
    }

    #[tokio::test]
    async fn resolve_local_dir_pyproject_scripts_first() {
        let dir = std::env::temp_dir().join(format!("mcpref-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pyproject.toml"),
            "[project]\nname=\"x\"\n[project.scripts]\nmy-mcp = \"x:main\"\n",
        )
        .unwrap();
        let cands = resolve_local_dir(&dir, "my-mcp").await.unwrap();
        assert_eq!(cands[0].command, "uv");
        assert_eq!(cands[0].args[0], "run");
        assert!(cands[0].args.contains(&"my-mcp".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn resolve_local_dir_node_bin_entry() {
        let dir = std::env::temp_dir().join(format!("mcpref-node-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(dir.join("dist/index.js"), "// mcp").unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"x","bin":{"x":"dist/index.js"}}"#,
        )
        .unwrap();
        let cands = resolve_local_dir(&dir, "x").await.unwrap();
        assert_eq!(cands[0].command, "node");
        assert!(cands[0].args[0].ends_with("dist/index.js"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn resolve_local_dir_empty_dir_errors() {
        let dir = std::env::temp_dir().join(format!("mcpref-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(resolve_local_dir(&dir, "x").await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn test_db() -> DbState {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mcp_plugins (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                command TEXT NOT NULL,
                args TEXT NOT NULL DEFAULT '[]',
                env TEXT NOT NULL DEFAULT '{}',
                capability_card TEXT NOT NULL DEFAULT '',
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                url TEXT NOT NULL DEFAULT '',
                headers TEXT NOT NULL DEFAULT '{}'
            );",
        )
        .unwrap();
        DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)))
    }

    /// 真实联网安装：跑真实 GitHub clone + 握手 + 落库。
    /// `cargo test --lib real_github_install -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn real_github_install() {
        let db = test_db();
        let repos_dir = std::env::temp_dir().join("mcpref-real-repos");
        // Swift 仓库：无 pyproject/package.json → 应干净报错，不留半成品
        let swift = install_from_ref_core(
            &db,
            "https://github.com/AdelElo13/mac-control-mcp",
            &repos_dir,
        )
        .await;
        println!("swift repo: {:?}", swift.as_ref().map(|r| r.tool_count));
        assert!(swift.is_err(), "Swift 仓库不该装得上");
        let count: i64 = db
            .0
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM mcp_plugins", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "失败安装不得留库记录");

        // Python 仓库：excel-mcp-server（pyproject 系）应解析+握手成功
        let py = install_from_ref_core(
            &db,
            "https://github.com/haris-musa/excel-mcp-server",
            &repos_dir,
        )
        .await;
        println!("python repo: {:?}", py);
        assert!(py.is_ok(), "Python 仓库安装失败: {:?}", py.err());
        let r = py.unwrap();
        assert!(r.tool_count > 0);
    }

    /// 真实联网安装：npm 包名（npx 链路）+ 已克隆本地目录回装。
    /// `cargo test --lib real_package_and_local_install -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn real_package_and_local_install() {
        let db = test_db();
        let repos_dir = std::env::temp_dir().join("mcpref-real-repos");

        // npm 包：uvx 先败（Python 侧没这名）→ npx -y 兜底握手
        let pkg = install_from_ref_core(
            &db,
            "@modelcontextprotocol/server-everything",
            &repos_dir,
        )
        .await;
        println!("npm pkg: {:?}", pkg);
        assert!(pkg.is_ok(), "npm 包安装失败: {:?}", pkg.err());
        assert!(pkg.unwrap().tool_count > 0);

        // 本地目录：直接指已克隆的 excel-mcp-server → manifest/uv 解析
        let local = repos_dir.join("excel-mcp-server");
        if local.is_dir() {
            let r = install_from_ref_core(&db, local.to_str().unwrap(), &repos_dir).await;
            println!("local dir: {:?}", r);
            assert!(r.is_ok(), "本地目录安装失败: {:?}", r.err());
        }
    }

    /// 无 git 兜底通道实测：codeload zip 下载+剥首层+平铺。
    /// `cargo test --lib real_repo_zip -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn real_repo_zip() {
        let target = std::env::temp_dir().join(format!("mcpref-zip-{}", std::process::id()));
        let r = download_repo_zip(
            "https://github.com/haris-musa/excel-mcp-server",
            &target,
        )
        .await;
        println!("{:?}", r);
        assert!(r.is_ok());
        assert!(target.join("pyproject.toml").is_file(), "解压后应有 pyproject.toml");
        assert!(target.join("src").is_dir());
        std::fs::remove_dir_all(&target).ok();
    }

    /// .mcpb 解包实测：真 bundle 文件 → 解包 → manifest 解析 → 候选生成。
    /// `cargo test --lib real_mcpb_unpack -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn real_mcpb_unpack() {
        let mcpb = PathBuf::from("/tmp/exmcptest/excel-mcp-server-0.1.8.mcpb");
        if !mcpb.is_file() {
            eprintln!("跳过：素材不存在 {}", mcpb.display());
            return;
        }
        let repos_dir = std::env::temp_dir().join("mcpref-mcpb-repos");
        let (dir, name) = materialize_mcpb(&mcpb, &repos_dir).await.unwrap();
        println!("name={} dir={}", name, dir.display());
        assert!(dir.join("manifest.json").is_file());
        let cands = resolve_local_dir(&dir, &name).await.unwrap();
        println!("candidates: {:?}", cands.iter().map(|c| (&c.command, &c.args)).collect::<Vec<_>>());
        assert!(!cands.is_empty());
    }

    /// 手动触发：往真实应用库安装一个 MCP（默认 playwright-mcp）。
    /// `cargo test --lib install_real_db -- --ignored --nocapture`
    /// 可用 MCP_REAL_REF 覆盖安装目标。
    #[tokio::test]
    #[ignore]
    async fn install_real_db() {
        let reference = std::env::var("MCP_REAL_REF")
            .unwrap_or_else(|_| "@playwright/mcp".to_string());
        let db_path = dirs_next::data_dir()
            .unwrap_or_default()
            .join("free-api-chat")
            .join("data_v2.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));
        let r = install_from_ref_core(&db, &reference, &default_repos_dir()).await;
        println!("install {:?} => {:?}", reference, r);
        assert!(r.is_ok());
    }
}
