//! Node 微信桥进程管理、代理调用与相关 Tauri 命令。
//! 从 channel.rs 原样抽出，行为不变。

use super::*;

// ---------------------------------------------------------------- 页面用 Tauri 命令（桥代理 + 配置）

static BRIDGE_CHILD: OnceLock<Mutex<Option<tokio::process::Child>>> = OnceLock::new();

fn bridge_child_slot() -> &'static Mutex<Option<tokio::process::Child>> {
    BRIDGE_CHILD.get_or_init(|| Mutex::new(None))
}

fn bridge_base(config: &ChannelConfig) -> String {
    format!("http://127.0.0.1:{}", config.bridge_port)
}

fn bridge_http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .expect("reqwest client")
}

fn resolve_bridge_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("FREE_API_WECHAT_BRIDGE_DIR") {
        let dir = PathBuf::from(dir);
        if dir.join("index.js").exists() {
            return Some(dir);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("wechat-bridge"));
        if let Some(parent) = cwd.parent() {
            candidates.push(parent.join("wechat-bridge"));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut ancestor = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..4 {
            if let Some(dir) = ancestor {
                candidates.push(dir.join("wechat-bridge"));
                ancestor = dir.parent().map(|p| p.to_path_buf());
            }
        }
    }
    candidates
        .into_iter()
        .find(|dir| dir.join("index.js").exists())
}

async fn bridge_call(
    config: &ChannelConfig,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let url = format!("{}{}", bridge_base(config), path);
    let client = bridge_http();
    let mut request = match method {
        "POST" => client.post(&url),
        _ => client.get(&url),
    };
    if let Some(value) = &body {
        request = request.json(value);
    }
    let response = request
        .header("x-channel-token", &config.token)
        .send()
        .await
        .map_err(|_| format!("微信桥未响应（{url}）。请先在页面启动桥。"))?;
    let value: Value = response.json().await.map_err(|e| e.to_string())?;
    if path.starts_with("/bridge/status") {
        cache_bridge_status(&value);
    }
    Ok(value)
}

/// 主动推送文本到主人微信（经桥转发；桥未运行时只记日志不报错）
pub(crate) async fn notify_wechat(config: &ChannelConfig, text: &str) {
    let payload = json!({ "text": text });
    if let Err(error) = bridge_call(config, "POST", "/bridge/notify", Some(payload)).await {
        eprintln!("[channel] 微信推送失败: {error}");
    }
}

/// 确保微信桥在运行：已就绪直接返回；否则拉起 node 子进程并等待就绪。
/// 供页面「启动桥」按钮与应用启动时的自动拉起共用；重复调用不会起第二个进程。
pub(crate) async fn ensure_bridge_running() -> Result<Value, String> {
    let config = load_channel_config()?;
    if let Ok(status) = bridge_call(&config, "GET", "/bridge/status", None).await {
        return Ok(json!({ "already_running": true, "status": status }));
    }
    // 已持有存活的子进程（仍在启动中）时不再重复 spawn，避免双实例抢端口。
    // guard 的检查与清理全部收在同步块内，跨 await 前必然释放（否则 future 非 Send）。
    let child_still_starting = {
        let mut slot = bridge_child_slot().lock().expect("child slot poisoned");
        match slot.as_mut().map(|process| process.try_wait()) {
            Some(Ok(None)) => true,  // 子进程存活，仍在启动中
            Some(Ok(Some(_))) => {
                slot.take(); // 子进程已退出，清理句柄后重新拉起
                false
            }
            _ => false,
        }
    };
    if child_still_starting {
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Ok(status) = bridge_call(&config, "GET", "/bridge/status", None).await {
                return Ok(json!({ "already_running": false, "status": status }));
            }
        }
        return Err("桥进程启动中但未在 10 秒内就绪，请查看桥日志".to_string());
    }
    let dir = resolve_bridge_dir().ok_or(
        "未找到 wechat-bridge 目录（可设 FREE_API_WECHAT_BRIDGE_DIR 指向该目录）",
    )?;
    let index_js = dir.join("index.js");
    // 桥日志落盘：之前 piped 后被丢弃，排障困难；现在追加写入数据目录
    let log_path = channel_config_path()
        .parent()
        .map(|p| p.join("wechat-bridge.log"))
        .ok_or_else(|| "无法定位桥日志目录".to_string())?;
    let stdout_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("打开桥日志失败: {e}"))?;
    let stderr_log = stdout_log
        .try_clone()
        .map_err(|e| format!("复用桥日志句柄失败: {e}"))?;
    let child = tokio::process::Command::new("node")
        .arg(&index_js)
        .env("CHANNEL_CONFIG_PATH", channel_config_path())
        .stdout(std::process::Stdio::from(stdout_log))
        .stderr(std::process::Stdio::from(stderr_log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("启动 node 桥失败: {e}"))?;
    *bridge_child_slot().lock().expect("child slot poisoned") = Some(child);

    // 等待桥就绪（最多 10s）
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if let Ok(status) = bridge_call(&config, "GET", "/bridge/status", None).await {
            return Ok(json!({ "already_running": false, "status": status }));
        }
    }
    Err("桥进程已启动但未在 10 秒内就绪，请查看桥日志".to_string())
}

#[tauri::command]
pub async fn wechat_bridge_start() -> Result<Value, String> {
    // 手动启动解除看门狗抑制（用户重新启用）
    BRIDGE_WATCHDOG_SUPPRESSED.store(false, std::sync::atomic::Ordering::SeqCst);
    ensure_bridge_running().await
}

#[tauri::command]
pub async fn wechat_bridge_status() -> Result<Value, String> {
    let config = load_channel_config()?;
    let bridge = bridge_call(&config, "GET", "/bridge/status", None)
        .await
        .unwrap_or_else(|e| json!({ "running": false, "error": e }));
    Ok(json!({
        "bridge": bridge,
        "config": config_value(&config),
    }))
}

#[tauri::command]
pub async fn wechat_bridge_login_start() -> Result<Value, String> {
    let config = load_channel_config()?;
    bridge_call(&config, "POST", "/bridge/login/start", None).await
}

#[tauri::command]
pub async fn wechat_bridge_qr() -> Result<Value, String> {
    let config = load_channel_config()?;
    bridge_call(&config, "GET", "/bridge/qr", None).await
}

#[tauri::command]
pub async fn wechat_bridge_relogin() -> Result<Value, String> {
    // 一键重新扫码：桥内部停监听→清凭据→发起新登录，进程不退出
    let config = load_channel_config()?;
    bridge_call(&config, "POST", "/bridge/relogin", None).await
}

#[tauri::command]
pub async fn wechat_bridge_stop() -> Result<Value, String> {
    let config = load_channel_config()?;
    BRIDGE_WATCHDOG_SUPPRESSED.store(true, std::sync::atomic::Ordering::SeqCst);
    // 1. 优先立即强杀托管的 Node 子进程，避免 Node 阻塞在微信长轮询时前端无响应
    let child = bridge_child_slot()
        .lock()
        .expect("child slot poisoned")
        .take();
    if let Some(mut process) = child {
        let _ = process.kill().await;
    }
    // 2. 尝试向可能孤立的端口发送优雅停止请求（忽略失败）
    let _ = bridge_call(&config, "POST", "/bridge/stop", None).await;
    cache_bridge_status(&json!({ "running": false, "logged_in": false }));
    Ok(json!({ "ok": true, "message": "微信桥已停止" }))
}

#[tauri::command]
pub async fn wechat_channel_config_get() -> Result<Value, String> {
    // 以磁盘为准读取（兼容手改文件），并同步进运行中服务的共享配置
    let config = load_channel_config()?;
    sync_shared_config(&config);
    Ok(config_value(&config))
}

#[tauri::command]
pub async fn wechat_channel_config_set(config: ConfigPutReq) -> Result<Value, String> {
    let mut current = load_channel_config()?;
    if let Some(v) = config.port {
        current.port = v;
    }
    if let Some(v) = config.bridge_port {
        current.bridge_port = v;
    }
    if let Some(v) = config.scratch_dir {
        current.scratch_dir = v.trim().to_string();
    }
    if let Some(v) = config.model_id {
        current.model_id = v.trim().to_string();
    }
    if let Some(v) = config.pro {
        current.pro = v.trim().to_string();
    }
    if let Some(v) = config.flash {
        current.flash = v.trim().to_string();
    }
    if let Some(v) = config.vision {
        current.vision = v.trim().to_string();
    }
    if let Some(v) = config.heartbeat_minutes {
        current.heartbeat_minutes = v.clamp(1, 24 * 60);
    }
    if let Some(v) = config.help_text {
        current.help_text = v;
    }
    if let Some(v) = config.auto_start_bridge {
        current.auto_start_bridge = v;
    }
    if let Some(v) = config.allowlist {
        current.allowlist = v.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    if let Some(v) = config.llm_retries {
        current.llm_retries = Some(v.clamp(1, 100));
    }
    if let Some(v) = config.action_milestone_steps {
        current.action_milestone_steps = v.clamp(0, 1000);
    }
    if let Some(v) = config.console_base_url.clone() {
        let trimmed = v.trim().to_string();
        current.console_base_url = if trimmed.is_empty() { None } else { Some(trimmed) };
    }
    if let Some(v) = config.phase_pushes {
        current.phase_pushes = v;
    }
    save_channel_config(&current)?;
    sync_shared_config(&current);
    Ok(config_value(&current))
}

/// 探测本机局域网 IPv4（UDP connect 不发包，仅取路由选定的本地地址）
fn lan_ipv4_addresses() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") else {
        return out;
    };
    // 对多个公网锚点做路由探测，多网卡/多网段都能覆盖
    for anchor in ["8.8.8.8:80", "1.1.1.1:80", "114.114.114.114:80"] {
        if socket.connect(anchor).is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = addr.ip().to_string();
                if !ip.starts_with("127.") && !out.contains(&ip) {
                    out.push(ip);
                }
            }
            // connect 会改 socket 的对端，重置后继续
            let _ = socket.connect("0.0.0.0:0");
        }
    }
    out
}

/// 网页控制台二维码：默认二维码指向局域网地址（扫码免输 token）；
/// 配置了 console_base_url（内网穿透域名）则优先用它。
#[tauri::command]
pub async fn wechat_channel_console_qr() -> Result<Value, String> {
    let config = channel_config_shared().read().expect("config poisoned").clone();
    let port = config.port;
    let token = config.token.clone();

    let mut urls: Vec<String> = Vec::new();
    let custom = config
        .console_base_url
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.trim_end_matches('/').to_string());
    if let Some(base) = &custom {
        urls.push(format!("{base}/?token={token}"));
    }
    for ip in lan_ipv4_addresses() {
        let url = format!("http://{ip}:{port}/?token={token}");
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    if urls.is_empty() {
        urls.push(format!("http://127.0.0.1:{port}/?token={token}"));
    }

    // 主二维码 = 第一个候选（自定义穿透地址优先，否则局域网）
    use qrcode::render::svg::Color;
    let code = qrcode::QrCode::with_error_correction_level(urls[0].as_bytes(), qrcode::EcLevel::M)
        .map_err(|e| format!("二维码生成失败: {e}"))?;
    let svg = code
        .render::<Color>()
        .min_dimensions(220, 220)
        .quiet_zone(true)
        .build();

    Ok(json!({
        "svg": svg,
        "primary": urls[0],
        "urls": urls,
        "source": if custom.is_some() { "custom" } else { "lan" },
    }))
}

#[tauri::command]
pub async fn wechat_channel_runs() -> Result<Value, String> {
    let mut runs: Vec<RunRecord> = runs_store()
        .lock()
        .expect("runs store poisoned")
        .values()
        .filter(|r| r.source == "channel")
        .cloned()
        .collect();
    runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    runs.truncate(20);
    serde_json::to_value(runs).map_err(|e| e.to_string())
}
