//! 写入型评测的**服务端判分 oracle**。
//!
//! 背景：此前 24 阶段的题目、输入与期望输出全部内嵌在客户端 fixture 里，
//! 一个能用 shell 的模型读一次规格表就能批量解题——"上百轮"变成模型的选择而非物理必然。
//! 本模块把答案表迁到宿主侧：fixture 里的测试只剩「取题 → 执行 transform → 送验 → 解锁」
//! 管道，需求文本与期望输出不再落盘，未来阶段在解锁前不可知，批量预写被物理消灭。
//!
//! 端点（仅绑定 127.0.0.1，nonce 鉴权）：
//! - GET  /current?nonce=  → 当前阶段 {id, requirement, input(含哨兵), total}
//! - POST /verify {nonce, output} → {pass, done, next?, hint?}
//! - GET  /status?nonce=   → {index, total, completed}
//!
//! 哨兵编码：JSON 无法表达 Infinity/NaN，双侧约定 "__Infinity__"/"__-Infinity__"/"__NaN__"
//! 字符串哨兵；比较时归一化为 token，对象键序不敏感（对齐 JS assert.deepEqual 语义）。

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const STAGES: &str = include_str!("eval_oracle_stages.json");

fn stages() -> Vec<Value> {
    serde_json::from_str(STAGES).expect("eval_oracle_stages.json 必须是合法 JSON")
}

/// 首阶段题面（id + requirement）：评测启动时注入目标文本。
/// 修复实测 catch-22——npm test 是取题通道却被写前门禁拦截，模型不看题无法过审、
/// 不过审不能跑测试。答案仍在服务端，此处只下发第一题的题面。
pub fn initial_stage_brief() -> String {
    let stages = stages();
    let first = &stages[0];
    format!(
        "阶段 {}/{}：仅修改 src/stage-{}.mjs 的 transform。{}",
        first["id"].as_str().unwrap_or("01"),
        stages.len(),
        first["id"].as_str().unwrap_or("01"),
        first["requirement"].as_str().unwrap_or("")
    )
}

/// 归一化序列化：哨兵 → token、数值 → f64、对象键排序。
/// 两侧（期望/实际）各自 canon 后字符串相等即语义相等（对齐 JS deepEqual 宽松语义，-0==0）。
fn canon(v: &Value) -> String {
    match v {
        Value::String(s) => match s.as_str() {
            "__Infinity__" => "@inf@".to_string(),
            "__-Infinity__" => "@ninf@".to_string(),
            "__NaN__" => "@nan@".to_string(),
            other => format!("s:{other}"),
        },
        Value::Number(n) => {
            // -0 与 0 宽松相等（对齐 JS deepEqual 语义），Display 对 -0.0 会输出 "-0"
            let f = n.as_f64().unwrap_or(f64::NAN);
            let f = if f == 0.0 { 0.0 } else { f };
            format!("n:{f}")
        }
        Value::Bool(b) => format!("b:{b}"),
        Value::Null => "null".to_string(),
        Value::Array(a) => format!(
            "a:[{}]",
            a.iter().map(canon).collect::<Vec<_>>().join(",")
        ),
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            format!(
                "o:{{{}",
                keys.iter()
                    .map(|k| format!("{k}={}", canon(&o[k.as_str()])))
                    .collect::<Vec<_>>()
                    .join(",")
            ) + "}"
        }
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    canon(a) == canon(b)
}

pub struct EvalOracleHandle {
    pub port: u16,
    pub nonce: String,
}

/// 启动判分服务（独立线程，进程存活期间有效；评测二进制随 wdio 会话退出）。
pub fn spawn() -> std::io::Result<EvalOracleHandle> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let nonce = uuid::Uuid::new_v4().to_string();
    let nonce_for_thread = nonce.clone();
    let progress = Arc::new(AtomicUsize::new(0));
    let progress_for_thread = progress.clone();

    std::thread::spawn(move || {
        let stages = stages();
        let total = stages.len();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // 每连接独立线程 + 10s 读写超时：任何半开/慢连接都不得阻塞判分服务——
            // 否则后续所有 npm test 的验题请求集体失败，模型永久卡在当前阶段（实测根因）。
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
            let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(10)));
            let nonce_for_conn = nonce_for_thread.clone();
            let stages_for_conn = stages.clone();
            let progress_for_conn = progress_for_thread.clone();
            std::thread::spawn(move || {
                let Ok(reader_stream) = stream.try_clone() else { return };
                let mut reader = BufReader::new(reader_stream);
                let mut stream = stream;
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() || request_line.trim().is_empty() {
                    return;
                }
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let trimmed = line.trim().to_string();
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some(v) = trimmed
                        .to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().to_string())
                    {
                        content_length = v.parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; content_length.min(1_048_576)];
                if content_length > 0 && reader.read_exact(&mut body).is_err() {
                    return;
                }
                let body_str = String::from_utf8_lossy(&body).to_string();
                let response = route(
                    &request_line,
                    &body_str,
                    &nonce_for_conn,
                    &stages_for_conn,
                    total,
                    &progress_for_conn,
                );
                let _ = write_response(&mut stream, &response);
            });
        }
    });

    Ok(EvalOracleHandle { port, nonce })
}

// 当前评测进程的活动 oracle（单评测串行假设下全局唯一）。
// finish 门禁据此把宿主判分进度作为交付的客观事实源——模型自declared的
// 审查契约/TODO 不能替代它（实测：廉价完成击穿三层门禁，1/24 交付被判 succeeded）。
static ACTIVE_ORACLE: std::sync::OnceLock<(u16, String)> = std::sync::OnceLock::new();

pub fn register_active_oracle(port: u16, nonce: &str) {
    let _ = ACTIVE_ORACLE.set((port, nonce.to_string()));
}

pub fn active_oracle() -> Option<&'static (u16, String)> {
    ACTIVE_ORACLE.get()
}

/// 查询活动 oracle 的判分进度。无 oracle 或查询失败返回 None（fail-open，
/// oracle 基础设施故障不应把 finish 永久堵死）。
pub async fn query_active_progress() -> Option<(usize, usize, bool)> {
    let (port, nonce) = active_oracle()?;
    let url = format!("http://127.0.0.1:{port}/status?nonce={nonce}");
    let body = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        reqwest::get(&url),
    )
    .await
    .ok()?
    .ok()?
    .text()
    .await
    .ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    Some((
        v.get("index")?.as_u64()? as usize,
        v.get("total")?.as_u64()? as usize,
        v.get("completed")?.as_bool()?,
    ))
}

fn route(
    request_line: &str,
    body: &str,
    nonce: &str,
    stages: &[Value],
    total: usize,
    progress: &Arc<AtomicUsize>,
) -> String {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    // 鉴权契约（见文件头部端点约定）：GET 用 URL query nonce；POST /verify 的 nonce
    // 在 body 里（fixture 测试文件如此发送）。判分服务重写时曾误对 POST 也强加
    // URL nonce，导致送验恒 403、hint 为空——冒烟测试抓到的回归。
    let authorized = match method {
        "POST" => body.contains(&format!("\"nonce\":\"{nonce}\"")),
        _ => path.contains(&format!("nonce={nonce}")),
    };
    if !authorized {
        return json_response(403, &serde_json::json!({ "error": "nonce 无效" }));
    }
    let index = progress.load(Ordering::SeqCst);
    let current = &stages[index.min(total - 1)];

    if method == "GET" && path.starts_with("/current") {
        return json_response(
            200,
            &serde_json::json!({
                "id": current["id"],
                "requirement": current["requirement"],
                "input": current["input"],
                "index": index,
                "total": total,
            }),
        );
    }
    if method == "GET" && path.starts_with("/status") {
        return json_response(
            200,
            &serde_json::json!({
                "index": index,
                "total": total,
                "completed": index >= total,
            }),
        );
    }
    if method == "POST" && path.starts_with("/verify") {
        if index >= total {
            return json_response(
                200,
                &serde_json::json!({ "pass": true, "done": true }),
            );
        }
        let payload: Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => return json_response(400, &serde_json::json!({ "error": format!("body 非法: {e}") })),
        };
        let output = payload.get("output").cloned().unwrap_or(Value::Null);
        let expected = &current["expected"];
        if values_equal(expected, &output) {
            let next_index = index + 1;
            progress.store(next_index, Ordering::SeqCst);
            if next_index >= total {
                return json_response(
                    200,
                    &serde_json::json!({ "pass": true, "done": true }),
                );
            }
            let next = &stages[next_index];
            return json_response(
                200,
                &serde_json::json!({
                    "pass": true,
                    "done": false,
                    "next": {
                        "id": next["id"],
                        "requirement": next["requirement"],
                        "input": next["input"],
                    },
                    "progress": { "index": next_index, "total": total },
                }),
            );
        }
        return json_response(
            200,
            &serde_json::json!({
                "pass": false,
                "hint": format!(
                    "阶段 {} 的实现行为与契约不符（宿主判分，细节自查）。请重新阅读 CURRENT_STAGE.md 与契约，修正 src/stage-{}.mjs 后重跑 npm test。",
                    current["id"], current["id"]
                ),
            }),
        );
    }
    json_response(404, &serde_json::json!({ "error": "未知端点" }))
}

fn json_response(code: u16, body: &Value) -> String {
    format!(
        "HTTP/1.1 {code} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.to_string().len(),
        body
    )
}

fn write_response(stream: &mut TcpStream, response: &str) -> std::io::Result<()> {
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canon_is_order_insensitive_for_objects_and_sentinel_aware() {
        let a: Value = serde_json::json!({ "b": 1, "a": [2, "__Infinity__"] });
        let b: Value = serde_json::json!({ "a": [2, "__Infinity__"], "b": 1.0 });
        assert_eq!(canon(&a), canon(&b));

        // 数组有序
        let c: Value = serde_json::json!([1, 2]);
        let d: Value = serde_json::json!([2, 1]);
        assert_ne!(canon(&c), canon(&d));

        // -0 与 0 宽松相等（对齐 JS deepEqual）
        let e: Value = serde_json::from_str("-0.0").unwrap();
        let f: Value = serde_json::from_str("0").unwrap();
        assert!(values_equal(&e, &f));
    }

    #[test]
    fn stages_table_is_well_formed() {
        let stages = stages();
        assert_eq!(stages.len(), 24);
        for s in &stages {
            assert!(s.get("id").is_some());
            assert!(s.get("requirement").is_some());
            assert!(s.get("input").is_some());
            assert!(s.get("expected").is_some());
        }
    }

    #[test]
    fn verify_post_auths_via_body_nonce_not_url() {
        // 回归锁：判分服务重写曾对 POST 强加 URL nonce，送验恒 403（hint 为空）。
        // 契约：POST /verify 的 nonce 在 body（fixture 测试文件如此发送）。
        let progress = Arc::new(AtomicUsize::new(0));
        let req = "POST /verify HTTP/1.1";
        let body = serde_json::json!({ "nonce": "n-123", "output": "ALPHA" }).to_string();
        let res = route(req, &body, "n-123", &stages(), 24, &progress);
        assert!(res.contains("\"pass\":true"), "body nonce 应放行并判分通过: {res}");
        assert_eq!(progress.load(Ordering::SeqCst), 1, "判分通过应推进进度");

        // URL nonce 的 POST（无 body nonce）不再放行
        let progress2 = Arc::new(AtomicUsize::new(0));
        let res2 = route("POST /verify?nonce=n-123 HTTP/1.1", "{}", "n-123", &stages(), 24, &progress2);
        assert!(res2.contains("403"), "缺 body nonce 的 POST 必须 403: {res2}");

        // GET 仍走 URL nonce
        let res3 = route("GET /current?nonce=n-123&v=1 HTTP/1.1", "", "bad", &stages(), 24, &progress);
        assert!(res3.contains("403"), "GET 携带错误 nonce 必须 403: {res3}");
    }
}
