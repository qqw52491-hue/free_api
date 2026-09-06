//! AppHandle 暂存与通道服务器启动。

use super::*;

/// setup 时暂存 AppHandle，供无 State/AppHandle 上下文的原生工具（schedule_manager.run_now 等）取用
pub(crate) fn stash_app_handle(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);
}

pub(crate) fn app_handle() -> Option<tauri::AppHandle> {
    APP_HANDLE.get().cloned()
}

static BRIDGE_STATUS_CACHE: std::sync::OnceLock<
    std::sync::Mutex<Option<(std::time::Instant, Value)>>,
> = std::sync::OnceLock::new();

/// bridge_call 成功时缓存状态快照，供 wechat_manager.status 离线查询
pub(crate) fn cache_bridge_status(value: &Value) {
    let cache = BRIDGE_STATUS_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(mut slot) = cache.lock() {
        *slot = Some((std::time::Instant::now(), value.clone()));
    }
}

/// 最近 5 分钟内的桥状态快照（过期返回 None，避免误报在线）
pub(crate) fn cached_bridge_status() -> Option<Value> {
    let cache = BRIDGE_STATUS_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    let slot = cache.lock().ok()?;
    let (at, value) = slot.as_ref()?;
    if at.elapsed() < std::time::Duration::from_secs(300) {
        Some(value.clone())
    } else {
        None
    }
}

pub(crate) fn channel_config_shared() -> &'static Arc<std::sync::RwLock<ChannelConfig>> {
    static CONFIG: OnceLock<Arc<std::sync::RwLock<ChannelConfig>>> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let config = match load_channel_config() {
            Ok(config) => config,
            Err(error) => {
                eprintln!("[channel] 配置加载失败，使用默认配置: {error}");
                ChannelConfig::default()
            }
        };
        Arc::new(std::sync::RwLock::new(config))
    })
}

pub(crate) fn sync_shared_config(config: &ChannelConfig) {
    *channel_config_shared()
        .write()
        .expect("config poisoned") = config.clone();
}

/// 在 Tauri setup 时启动本机通道 API；失败只打日志，不阻断应用启动。
pub fn spawn_channel_server(app: AppHandle) {
    stash_app_handle(app.clone());
    let config = channel_config_shared().read().expect("config poisoned").clone();
    let port = config.port;
    let state = Arc::new(ChannelState {
        app,
        config: channel_config_shared().clone(),
    });
    tauri::async_runtime::spawn(async move {
        let addr = format!("0.0.0.0:{port}");
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => {
                println!("[channel] 微信通道 API 已监听 http://{addr}（局域网设备用 Mac 的 192.168.x.x:{port} 访问）");
                if let Err(error) = axum::serve(listener, channel_router(state)).await {
                    eprintln!("[channel] API 服务退出: {error}");
                }
            }
            Err(error) => eprintln!(
                "[channel] 端口 127.0.0.1:{port} 绑定失败（可能已有应用实例在运行）: {error}"
            ),
        }
    });

    // 自动拉起微信桥（auto_start_bridge=false 可关闭；失败不影响应用）
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let auto_start = channel_config_shared()
            .read()
            .expect("config poisoned")
            .auto_start_bridge;
        if !auto_start {
            println!("[channel] 微信桥自动启动已关闭，可在微信绑定页手动启动");
            return;
        }
        match ensure_bridge_running().await {
            Ok(value) => println!("[channel] 微信桥已自动启动: {value}"),
            Err(error) => eprintln!("[channel] 微信桥自动启动失败（可在微信绑定页手动启动）: {error}"),
        }
    });

    // 看门狗：低频探活，桥意外退出时自动复活（用户手动停止则保持安静）
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            if BRIDGE_WATCHDOG_SUPPRESSED.load(std::sync::atomic::Ordering::SeqCst) {
                continue;
            }
            let auto_start = channel_config_shared()
                .read()
                .map(|config| config.auto_start_bridge)
                .unwrap_or(false);
            if !auto_start {
                continue;
            }
            if let Err(error) = ensure_bridge_running().await {
                eprintln!("[channel] 看门狗拉起微信桥失败: {error}");
            }
        }
    });
}
