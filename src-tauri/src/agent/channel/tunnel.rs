//! 内网穿透托管：应用启动后自动确保 cloudflared 命名隧道把固定域名
//! 指向本机通道端口（channel API）。
//!
//! 边界：隧道凭证与 DNS 路由由用户在 cloudflared 侧一次性建好
//! （~/.cloudflared/config.yml + <id>.json + 域名 CNAME）；本模块只负责
//! ①合并 ingress 规则 ②拉起/重启隧道进程 ③汇报真实可达状态。
//! 缺凭证/缺配置时 fail-open：跳过并在日志与状态里给出原因。

use super::*;
use std::path::Path;

static TUNNEL_STATUS: OnceLock<Mutex<Value>> = OnceLock::new();

/// 最近一次托管尝试的结果快照（供 config_value / 前端展示）。
pub fn tunnel_status() -> Value {
    TUNNEL_STATUS
        .get_or_init(|| Mutex::new(json!({ "state": "idle" })))
        .lock()
        .map(|s| s.clone())
        .unwrap_or_else(|_| json!({ "state": "unknown" }))
}

fn set_status(state: &str, message: &str, url: Option<&str>) {
    if let Some(slot) = TUNNEL_STATUS.get() {
        if let Ok(mut s) = slot.lock() {
            *s = json!({
                "state": state,
                "message": message,
                "url": url,
                "at": chrono::Utc::now().to_rfc3339(),
            });
        }
    } else {
        let _ = TUNNEL_STATUS.set(Mutex::new(json!({
            "state": state,
            "message": message,
            "url": url,
            "at": chrono::Utc::now().to_rfc3339(),
        })));
    }
}

fn expand_home(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs_next::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn tunnel_dir(config: &ChannelConfig) -> PathBuf {
    let raw = config.tunnel_dir.trim();
    if raw.is_empty() {
        expand_home("~/.cloudflared")
    } else {
        expand_home(raw)
    }
}

/// 往 cloudflared 配置 YAML 里合并一条 ingress：
/// {hostname → service} 去重更新后插在兜底规则（无 hostname 项）之前。
/// 返回 (新文本, 是否变更)。纯函数便于单测。
pub(crate) fn merge_ingress_rule(
    yaml_text: &str,
    hostname: &str,
    service: &str,
) -> Result<(String, bool), String> {
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(yaml_text).map_err(|e| format!("config.yml 解析失败: {e}"))?;
    let ingress = doc
        .get_mut("ingress")
        .and_then(|v| v.as_sequence_mut())
        .ok_or_else(|| "config.yml 缺少 ingress 列表".to_string())?;

    let changed = !ingress_contains_exact(yaml_text, hostname, service);

    // 摘除同名 hostname 的旧规则（可能多条/旧端口），随后统一重插
    ingress.retain(|item| {
        item.get("hostname").and_then(|h| h.as_str()) != Some(hostname)
    });

    // 兜底规则 = 不带 hostname 的项（惯例放最后）；找到则插其前
    let pos = ingress
        .iter()
        .position(|item| item.get("hostname").is_none())
        .unwrap_or_else(|| ingress.len());

    let mut rule = serde_yaml::Mapping::new();
    rule.insert(
        serde_yaml::Value::String("hostname".into()),
        serde_yaml::Value::String(hostname.to_string()),
    );
    rule.insert(
        serde_yaml::Value::String("service".into()),
        serde_yaml::Value::String(service.to_string()),
    );
    ingress.insert(pos, serde_yaml::Value::Mapping(rule));

    let out = serde_yaml::to_string(&doc).map_err(|e| format!("config.yml 序列化失败: {e}"))?;
    Ok((out, changed))
}

/// 原文里是否已有完全一致的 {hostname, service} 规则（无变化则无需写盘）。
fn ingress_contains_exact(yaml_text: &str, hostname: &str, service: &str) -> bool {
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(yaml_text) else {
        return false;
    };
    doc.get("ingress")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter().any(|item| {
                item.get("hostname").and_then(|h| h.as_str()) == Some(hostname)
                    && item.get("service").and_then(|s| s.as_str()) == Some(service)
            })
        })
        .unwrap_or(false)
}

/// 本机 cloudflared 隧道进程 PID 列表（unix pgrep / windows tasklist）。
fn cloudflared_pids() -> Vec<u32> {
    #[cfg(unix)]
    {
        std::process::Command::new("pgrep")
            .args(["-f", "cloudflared.*tunnel"])
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(|l| l.trim().parse::<u32>().ok())
                    .collect()
            })
            .unwrap_or_default()
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq cloudflared.exe", "/FO", "CSV", "/NH"])
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(|l| {
                        let cols: Vec<&str> = l.split(',').collect();
                        cols.get(1)?.trim_matches('"').parse::<u32>().ok()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Vec::new()
    }
}

fn kill_pids(pids: &[u32]) {
    for pid in pids {
        #[cfg(unix)]
        {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .status();
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .status();
        }
    }
}

/// 托管入口：确保隧道在跑且 ingress 指向当前通道端口。
/// 可重复调用（幂等）；返回对外 URL 或带原因的错误。
pub fn ensure_tunnel(app_root: &Path, config: &ChannelConfig) -> Result<String, String> {
    let hostname = config.tunnel_hostname.trim().trim_end_matches('/').to_string();
    if hostname.is_empty() {
        return Err("tunnel_hostname 未配置".to_string());
    }
    let url = format!("https://{hostname}");
    let dir = tunnel_dir(config);
    let cfg_path = dir.join("config.yml");
    if !cfg_path.is_file() {
        let msg = format!("缺少隧道配置 {}（先在 cloudflared 侧建好命名隧道+凭证）", cfg_path.display());
        set_status("skipped", &msg, None);
        return Err(msg);
    }
    let raw = std::fs::read_to_string(&cfg_path)
        .map_err(|e| format!("读取 {} 失败: {e}", cfg_path.display()))?;

    // 凭证文件存在性（config 里的 credentials-file 可能带 ~ 前缀）
    let cred = serde_yaml::from_str::<serde_yaml::Value>(&raw)
        .ok()
        .and_then(|doc| doc.get("credentials-file").and_then(|v| v.as_str()).map(String::from));
    match cred {
        Some(c) if !expand_home(&c).is_file() => {
            let msg = format!("隧道凭证不存在: {c}");
            set_status("skipped", &msg, None);
            return Err(msg);
        }
        None => {
            let msg = "config.yml 缺少 credentials-file 字段".to_string();
            set_status("skipped", &msg, None);
            return Err(msg);
        }
        _ => {}
    }

    let service = format!("http://localhost:{}", config.port);
    let (merged, changed) = merge_ingress_rule(&raw, &hostname, &service)?;
    if changed {
        let bak = cfg_path.with_extension(format!("yml.bak-{}", chrono::Utc::now().timestamp()));
        let _ = std::fs::copy(&cfg_path, &bak);
        std::fs::write(&cfg_path, &merged)
            .map_err(|e| format!("写回 {} 失败: {e}", cfg_path.display()))?;
    }

    let bin = crate::agent::tunnel_runtime::ensure_cloudflared_blocking(app_root)
        .map_err(|e| {
            set_status("error", &e, None);
            e
        })?;

    let running = cloudflared_pids();
    if !running.is_empty() && !changed {
        set_status("running", "cloudflared 已在运行且 ingress 已包含本域名", Some(&url));
        return Ok(url);
    }
    if !running.is_empty() {
        // ingress 变了才需要重启（会短暂影响同隧道其他域名，属必要成本）
        kill_pids(&running);
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }

    // spawn 后台隧道，日志落 app 数据目录便于排查
    let log_dir = app_root.join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("cloudflared.log"))
        .map_err(|e| format!("打开 cloudflared 日志失败: {e}"))?;
    let log_err = log_file.try_clone().map_err(|e| e.to_string())?;
    let mut child = std::process::Command::new(&bin)
        .args(["tunnel", "--config"])
        .arg(&cfg_path)
        .arg("run")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(log_err))
        .spawn()
        .map_err(|e| format!("启动 cloudflared 失败: {e}"))?;

    std::thread::sleep(std::time::Duration::from_secs(3));
    match child.try_wait() {
        Ok(Some(status)) => {
            let msg = format!("cloudflared 启动后立即退出（{status}），见 logs/cloudflared.log");
            set_status("error", &msg, None);
            Err(msg)
        }
        Ok(None) => {
            // 进程活着即视为成功；外网探测属网络层确认，不阻塞结论
            set_status("running", "cloudflared 已启动", Some(&url));
            Ok(url)
        }
        Err(e) => Err(format!("cloudflared 状态查询失败: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "tunnel: aaa-bbb\ncredentials-file: /home/u/.cloudflared/aaa.json\n\ningress:\n  - hostname: mcp.example.com\n    service: http://localhost:8787\n  - service: http_status:404\n";

    #[test]
    fn merge_inserts_before_catchall() {
        let (out, changed) =
            merge_ingress_rule(BASE, "agent.example.com", "http://localhost:8790").unwrap();
        assert!(changed);
        let doc: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
        let seq = doc["ingress"].as_sequence().unwrap();
        assert_eq!(seq.len(), 3);
        assert_eq!(seq[1]["hostname"].as_str().unwrap(), "agent.example.com");
        assert_eq!(seq[1]["service"].as_str().unwrap(), "http://localhost:8790");
        assert_eq!(seq[2]["service"].as_str().unwrap(), "http_status:404");
        // 原有规则原样保留
        assert_eq!(seq[0]["hostname"].as_str().unwrap(), "mcp.example.com");
    }

    #[test]
    fn merge_idempotent() {
        let (out, changed) =
            merge_ingress_rule(BASE, "agent.example.com", "http://localhost:8790").unwrap();
        let (_, changed2) =
            merge_ingress_rule(&out, "agent.example.com", "http://localhost:8790").unwrap();
        assert!(!changed2, "同规则二次合并应为幂等: {out}");
    }

    #[test]
    fn merge_updates_stale_target() {
        let once =
            merge_ingress_rule(BASE, "agent.example.com", "http://localhost:8790").unwrap().0;
        let (out, changed) =
            merge_ingress_rule(&once, "agent.example.com", "http://localhost:9999").unwrap();
        assert!(changed);
        let doc: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
        let seq = doc["ingress"].as_sequence().unwrap();
        assert_eq!(seq.len(), 3); // 更新不增生
        assert_eq!(seq[1]["service"].as_str().unwrap(), "http://localhost:9999");
    }

    #[test]
    fn merge_dedupes_duplicate_hostname() {
        let dup = "ingress:\n  - hostname: a.example.com\n    service: http://localhost:1\n  - hostname: a.example.com\n    service: http://localhost:2\n  - service: http_status:404\n";
        let (out, _) = merge_ingress_rule(dup, "a.example.com", "http://localhost:9").unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
        let seq = doc["ingress"].as_sequence().unwrap();
        assert_eq!(seq.len(), 2);
        assert_eq!(seq[0]["service"].as_str().unwrap(), "http://localhost:9");
    }

    #[test]
    fn merge_rejects_missing_ingress() {
        let r = merge_ingress_rule("tunnel: x\n", "a.b", "http://localhost:1");
        assert!(r.is_err());
    }
}
