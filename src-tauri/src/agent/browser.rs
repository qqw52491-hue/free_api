use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicU8, Ordering};
use std::path::PathBuf;
use tokio::time::Duration;
use headless_chrome::{Browser, Tab, LaunchOptions};
use serde::{Deserialize, Serialize};

static GLOBAL_BROWSER: OnceLock<Browser> = OnceLock::new();
static GLOBAL_TAB: OnceLock<Arc<Tab>> = OnceLock::new();

/// 浏览器启动模式
/// 0 = 默认（每次干净的临时 profile）
/// 1 = 持久化（user_data_dir，保留 Cookie/登录态）
/// 2 = 连接已有浏览器（CDP 端口 9222）
static BROWSER_MODE: AtomicU8 = AtomicU8::new(0);

/// 外部调用：设置浏览器模式（在首次使用浏览器之前调用）
pub fn set_browser_mode(mode: u8) {
    BROWSER_MODE.store(mode, Ordering::Relaxed);
}

/// 获取当前浏览器模式
pub fn get_browser_mode() -> u8 {
    BROWSER_MODE.load(Ordering::Relaxed)
}

pub fn get_or_create_tab() -> Result<Arc<Tab>, String> {
    if let Some(tab) = GLOBAL_TAB.get() {
        return Ok(tab.clone());
    }

    let mode = BROWSER_MODE.load(Ordering::Relaxed);

    match mode {
        // === 模式 2：连接到已经手动打开的 Chrome (CDP) ===
        2 => {
            println!("🔗 [浏览器模式: 连接已有 Chrome] 正在连接 localhost:9222...");
            
            // 先通过 HTTP 获取 WebSocket URL
            let ws_url = get_cdp_ws_url("http://127.0.0.1:9222")
                .map_err(|e| format!("无法连接已有浏览器。请确保已以调试模式启动 Chrome:\n\
                    /Applications/Google\\ Chrome.app/Contents/MacOS/Google\\ Chrome --remote-debugging-port=9222\n\
                    错误: {}", e))?;
            
            let browser = Browser::connect(ws_url)
                .map_err(|e| format!("CDP 连接失败: {}", e))?;
            
            // 获取已存在的标签页
            let tab = {
                let tabs = browser.get_tabs().lock().unwrap();
                if let Some(first_tab) = tabs.first() {
                    first_tab.clone()
                } else {
                    drop(tabs);
                    browser.new_tab().map_err(|e| format!("新建标签页失败: {:?}", e))?
                }
            };

            let _ = GLOBAL_BROWSER.set(browser);
            let _ = GLOBAL_TAB.set(tab.clone());
            Ok(tab)
        }

        // === 模式 1：持久化 profile（保留登录态） ===
        1 => {
            // 使用固定的 user_data_dir
            let data_dir = dirs_next::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("free-api-agent-browser");
            
            println!("💾 [浏览器模式: 持久化] 数据目录: {}", data_dir.display());

            // ⚠️ 核心防闪退机制：杀掉因为代码热重载残留下来的僵尸 Chrome 进程，并强行删除死锁文件
            // 完美解决你在视频和环境里遇到的“谷歌浏览器弹出来闪一下就消失/报错”的问题。
            #[cfg(target_family = "unix")]
            let _ = std::process::Command::new("pkill").args(["-9", "-f", "free-api-agent-browser"]).output();
            
            let _ = std::fs::remove_file(data_dir.join("SingletonLock"));
            std::thread::sleep(std::time::Duration::from_millis(200)); // 等待系统内核回收进程

            let options = LaunchOptions::default_builder()
                .headless(false)
                .idle_browser_timeout(Duration::from_secs(36000))
                .user_data_dir(Some(data_dir))
                .args(vec![
                    "--no-sandbox".as_ref(),
                    "--disable-setuid-sandbox".as_ref(),
                    "--disable-gpu".as_ref(),
                    "--window-size=1280,800".as_ref(),
                    "--disable-dev-shm-usage".as_ref(),
                ])
                .build()
                .unwrap_or_default();

            let browser = Browser::new(options).map_err(|e| format!("拉起浏览器失败: {}", e))?;
            let tab = browser.new_tab().map_err(|e| format!("新建标签页失败: {:?}", e))?;

            let _ = GLOBAL_BROWSER.set(browser);
            let _ = GLOBAL_TAB.set(tab.clone());
            Ok(tab)
        }

        // === 模式 0：默认（干净临时 profile） ===
        _ => {
            println!("🧹 [浏览器模式: 临时] 每次启动全新 profile");

            let options = LaunchOptions::default_builder()
                .headless(false)
                .idle_browser_timeout(Duration::from_secs(36000))
                .args(vec![
                    "--no-sandbox".as_ref(),
                    "--disable-setuid-sandbox".as_ref(),
                    "--disable-gpu".as_ref(),
                    "--window-size=1280,800".as_ref(),
                    "--disable-dev-shm-usage".as_ref(),
                ])
                .build()
                .unwrap_or_default();

            let browser = Browser::new(options).map_err(|e| format!("拉起浏览器失败: {}", e))?;
            let tab = browser.new_tab().map_err(|e| format!("新建标签页失败: {:?}", e))?;

            let _ = GLOBAL_BROWSER.set(browser);
            let _ = GLOBAL_TAB.set(tab.clone());
            Ok(tab)
        }
    }
}

/// 通过 HTTP 获取 Chrome 的 WebSocket debugger URL
fn get_cdp_ws_url(base: &str) -> Result<String, String> {
    let url = format!("{}/json/version", base);
    let resp = reqwest::blocking::get(&url)
        .map_err(|e| format!("HTTP 请求失败: {}", e))?;
    let json: serde_json::Value = resp.json()
        .map_err(|e| format!("解析 JSON 失败: {}", e))?;
    json["webSocketDebuggerUrl"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "未找到 webSocketDebuggerUrl".to_string())
}

#[derive(Serialize, Deserialize)]
pub struct BrowserAction {
    pub action: String,
    pub url: Option<String>,
    pub id: Option<u32>,
    pub text: Option<String>,
}

pub fn run_browser_dom(command_str: &str) -> (String, String, bool) {
    let tab = match get_or_create_tab() {
        Ok(t) => t,
        Err(e) => return (String::new(), format!("获取Tab失败: {:?}", e), false),
    };

    // --- 解析指令 ---
    let parts: Vec<&str> = command_str.splitn(3, ' ').collect();
    let cmd_type = parts.get(0).unwrap_or(&"").to_lowercase();
    let arg1 = parts.get(1).map(|s| s.to_string());
    let arg2 = parts.get(2).map(|s| s.to_string());

    match cmd_type.as_str() {
        // ===== 导航类 =====
        "goto" | "navigate" => {
            let target_url = arg1.unwrap_or_default();
            if !target_url.starts_with("http") {
                return (String::new(), "URL 格式不正确，需要以 http 开头".to_string(), false);
            }
            if let Err(e) = tab.navigate_to(&target_url) {
                return (String::new(), format!("跳转失败: {:?}", e), false);
            }
            
            // ⚠️ 极其核心的保命修改：坚决不能调用 tab.wait_until_navigated()！
            // 像 kimi.com 这种基于 websocket 和重型框架的 AI 单页应用，
            // 它的 loadEvent 有极高概率永远不会触发，导致 headless_chrome 的这行代码发生死锁，程序被彻底卡死。
            // 做法：给它 1.5 秒缓冲，直接让该指令通过，把它交给大模型的下一轮 wait_idle 去处理。
            std::thread::sleep(Duration::from_millis(1500));
            
            let title = tab.get_title().unwrap_or_default();
            (format!("触发跳转！标题可能还在加载中: {}", title), String::new(), true)
        }
        "back" => {
            let js = "window.history.back(); true;";
            let _ = tab.evaluate(js, false);
            std::thread::sleep(Duration::from_millis(1500));
            let title = tab.get_title().unwrap_or_default();
            (format!("✅ 已后退，当前页: {}", title), String::new(), true)
        }
        "forward" => {
            let js = "window.history.forward(); true;";
            let _ = tab.evaluate(js, false);
            std::thread::sleep(Duration::from_millis(1500));
            let title = tab.get_title().unwrap_or_default();
            (format!("✅ 已前进，当前页: {}", title), String::new(), true)
        }
        "refresh" | "reload" => {
            let js = "location.reload(); true;";
            let _ = tab.evaluate(js, false);
            std::thread::sleep(Duration::from_millis(2000));
            let title = tab.get_title().unwrap_or_default();
            (format!("✅ 页面已刷新: {}", title), String::new(), true)
        }
        "tab_url" | "url" => {
            match tab.evaluate("window.location.href;", false) {
                Ok(remote_obj) => {
                    let url = remote_obj.value.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    (format!("当前 URL: {}", url), String::new(), true)
                }
                Err(e) => (String::new(), format!("获取URL失败: {:?}", e), false),
            }
        }

        "wait" => {
            // wait 2  → 等待2秒
            // wait    → 默认等待1秒
            let secs: f64 = arg1.as_deref().and_then(|s| s.parse().ok()).unwrap_or(1.0);
            let ms = (secs * 1000.0) as u64;
            std::thread::sleep(Duration::from_millis(ms));
            (format!("✅ 已等待 {:.1} 秒", secs), String::new(), true)
        }
        "wait_idle" => {
            // 智能等待 DOM 稳定（800ms 内没有新元素加载才继续），默认最大超时 10 秒
            let timeout_ms: u64 = arg1.as_deref().and_then(|s| s.parse().ok()).unwrap_or(10000);
            let js = format!(r#"
                new Promise((resolve) => {{
                    let timeoutId;
                    let maxTimeoutId;
                    const observer = new MutationObserver(() => {{
                        clearTimeout(timeoutId);
                        timeoutId = setTimeout(() => finish('stable'), 800);
                    }});
                    const finish = (reason) => {{
                        observer.disconnect();
                        clearTimeout(timeoutId);
                        clearTimeout(maxTimeoutId);
                        resolve(reason);
                    }};
                    observer.observe(document.body, {{ childList: true, subtree: true, attributes: true }});
                    timeoutId = setTimeout(() => finish('stable'), 800);
                    maxTimeoutId = setTimeout(() => finish('timeout'), {});
                }});
            "#, timeout_ms);
            match tab.evaluate(&js, true) {
                Ok(res) => {
                    let result = res.value.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    if result == "stable" {
                        (format!("✅ DOM 已完全加载并进入稳定状态"), String::new(), true)
                    } else {
                        (format!("⏳ DOM 稳定检测超时 ({} ms)，但已允许继续操作", timeout_ms), String::new(), true)
                    }
                }
                Err(e) => (String::new(), format!("智能等待失败: {:?}", e), false),
            }
        }


        // ===== 交互类 =====
        "extract" | "look" => {
            let js = r#"
            (function() {
                // 1. 清理旧 ID
                document.querySelectorAll('[data-tauri-agent-id]').forEach(el => {
                    el.removeAttribute('data-tauri-agent-id');
                    el.style.outline = "";
                });

                const isVisible = (el) => {
                    const style = window.getComputedStyle(el);
                    if (style.display === 'none' || style.visibility === 'hidden' || style.opacity === '0' || style.pointerEvents === 'none') return false;
                    const rect = el.getBoundingClientRect();
                    return rect.width > 2 && rect.height > 2; // 过滤掉太小的元素
                };

                const isTopLevel = (el) => {
                    const rect = el.getBoundingClientRect();
                    const x = rect.left + rect.width / 2;
                    const y = rect.top + rect.height / 2;
                    if (x < 0 || x >= window.innerWidth || y < 0 || y >= window.innerHeight) return true;
                    const topEl = document.elementFromPoint(x, y);
                    return !topEl || el.contains(topEl) || topEl.contains(el);
                };

                // 选择候选元素
                let candidates = Array.from(document.querySelectorAll('a, button, input, textarea, select, [role="button"], [role="link"], [contenteditable="true"], .btn, .button')).filter(isVisible);
                
                // 补充文本节点
                let treeWalker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, null, false);
                let textNodes = [];
                let node;
                while(node = treeWalker.nextNode()) {
                   if(node.textContent.trim().length > 2) {
                       let p = node.parentElement;
                       if(p && isVisible(p) && !p.closest('a, button, input, textarea, select')) {
                           textNodes.push(p);
                       }
                   }
                }

                let all = Array.from(new Set([...candidates, ...textNodes])).filter(isTopLevel);
                
                let resultLines = all.map((el, index) => {
                    const id = index + 1;
                    const rect = el.getBoundingClientRect();
                    if (rect.top > window.innerHeight || rect.bottom < 0) return null; // 视口外的不显示，但保留 ID

                    el.setAttribute('data-tauri-agent-id', id);
                    el.style.outline = "2px solid rgba(255, 0, 0, 0.5)"; // 红色虚线框

                    let text = (el.innerText || el.getAttribute('aria-label') || el.getAttribute('title') || el.placeholder || "").trim();
                    if (!text) {
                        if (el.tagName === 'INPUT') text = `输入框(${el.type})`;
                        else if (el.querySelector('img')) text = "图片按钮";
                        else text = "交互元素";
                    }

                    // 优化：清理多余的换行和连续空格，限制最高30字符
                    text = text.replace(/[\r\n\s]+/g, ' ').trim();
                    const shortText = text.substring(0, 30) + (text.length > 30 ? "..." : "");

                    return `[${id}] <${el.tagName.toLowerCase()}> ${shortText} (X:${Math.round(rect.left + rect.width/2)}, Y:${Math.round(rect.top + rect.height/2)})`;
                }).filter(l => l !== null);

                let status = `【页面状态】: 视口 ${window.innerWidth}x${window.innerHeight}, 滚动 ${Math.round(window.scrollY)}/${document.body.scrollHeight}`;
                return status + "\n【可用元素清单】:\n" + resultLines.join('\n');
            })();
            "#;
            match tab.evaluate(js, false) {
                Ok(remote_obj) => {
                    let text = remote_obj.value.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    (text, String::new(), true)
                }
                Err(e) => (String::new(), format!("提取失败: {}", e), false),
            }
        }
        "click" => {
            let id = arg1.as_deref().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let js = format!(r#"
                (function() {{
                    let el = document.querySelector('[data-tauri-agent-id="{}"]');
                    if (!el) return "NOT_FOUND";
                    
                    el.scrollIntoView({{behavior: 'instant', block: 'center'}});
                    
                    // 模拟真实交互序列
                    const events = ['mouseenter', 'mouseover', 'mousedown', 'mouseup', 'click'];
                    events.forEach(name => {{
                        el.dispatchEvent(new MouseEvent(name, {{bubbles: true, cancelable: true, view: window}}));
                    }});

                    // 兜底：如果是 A 标签且没跳转，强制跳转
                    if (el.tagName.toLowerCase() === 'a' && el.href && !el.href.startsWith('javascript:')) {{
                        setTimeout(() => {{ window.location.href = el.href; }}, 100);
                    }}
                    return "OK";
                }})();
            "#, id);
            match tab.evaluate(&js, false) {
                Ok(res) => {
                    let val = res.value.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    if val == "OK" {
                        (format!("✅ 成功点击元素 [{}]", id), String::new(), true)
                    } else {
                        (String::new(), format!("❌ 找不到元素 [{}]", id), false)
                    }
                },
                Err(e) => (String::new(), format!("点击出错: {}", e), false),
            }
        }
        "type" => {
            let id = arg1.as_deref().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let val = arg2.unwrap_or_default();
            let escaped_val = val.replace('\\', "\\\\").replace('\'', "\\'").replace('\n', "\\n");
            
            // --- 核心改进：先清空，再输入 ---
            let js_prepare = format!(r#"
                (function() {{
                    const el = document.querySelector('[data-tauri-agent-id="{}"]');
                    if (!el) return "NOT_FOUND";
                    el.scrollIntoView({{behavior: 'instant', block: 'center'}});
                    el.focus();
                    
                    // 兼容 React 和 Vue 的深度清空 Hack 写法
                    let nativeInputValueSetter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value");
                    let setter = nativeInputValueSetter ? nativeInputValueSetter.set : null;
                    if (setter) {{
                        setter.call(el, '');
                    }} else {{
                        el.value = '';
                    }}

                    if(el.isContentEditable) el.innerText = '';
                    
                    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    return "OK";
                }})();
            "#, id);

            match tab.evaluate(&js_prepare, false) {
                Ok(res) => {
                    let status = res.value.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    if status != "OK" {
                        return (String::new(), format!("❌ 找不到输入框 [{}]", id), false);
                    }
                },
                Err(e) => return (String::new(), format!("输入准备失败: {:?}", e), false),
            }

            // 模拟真实按键输入
            if !val.is_empty() {
                if let Err(e) = tab.type_str(&val) {
                   return (String::new(), format!("键盘模拟输入失败: {:?}", e), false);
                }
            }
            
            (format!("✅ 成功输入: {}", val), String::new(), true)
        }
        "select" => {
            // select 8 option_value → 选择下拉框
            let id = arg1.as_deref().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let option_val = arg2.unwrap_or_default();
            let escaped_val = option_val.replace('\\', "\\\\").replace('\'', "\\'");
            let js = format!(r#"
                let el = document.querySelector('[data-tauri-agent-id="{}"]');
                if (el && el.tagName === 'SELECT') {{
                    el.value = '{}';
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    true;
                }} else {{ false; }}
            "#, id, escaped_val);
            match tab.evaluate(&js, false) {
                Ok(res) if res.value.as_ref().and_then(|v| v.as_bool()).unwrap_or(false) => {
                    (format!("✅ 下拉框 [{}] 已选择: {}", id, option_val), String::new(), true)
                }
                _ => (String::new(), format!("❌ 无法选择元素 [{}] 或它不是下拉框", id), false),
            }
        }
        "press" => {
            let key = arg1.unwrap_or_else(|| "Enter".to_string());
            if tab.press_key(&key).is_err() { return (String::new(), format!("❌ 按键 {} 失败", key), false); }
            (format!("✅ 成功按下 [{}] 键", key), String::new(), true)
        }
        "hover" => {
            let id = arg1.as_deref().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let js = format!(r#"
                let el = document.querySelector('[data-tauri-agent-id="{}"]');
                if (el) {{
                    el.scrollIntoView({{behavior: 'instant', block: 'center'}});
                    el.dispatchEvent(new MouseEvent('mouseenter', {{bubbles: true}}));
                    el.dispatchEvent(new MouseEvent('mouseover', {{bubbles: true}}));
                    true;
                }} else {{ false; }}
            "#, id);
            match tab.evaluate(&js, false) {
                Ok(res) if res.value.as_ref().and_then(|v| v.as_bool()).unwrap_or(false) => {
                    std::thread::sleep(Duration::from_millis(500));
                    (format!("✅ 鼠标已悬停在元素 [{}] 上", id), String::new(), true)
                }
                _ => (String::new(), format!("❌ 找不到编号为 [{}] 的元素", id), false),
            }
        }

        // ===== 读取类 =====
        "read" => {
            let js = "document.body.innerText;";
            match tab.evaluate(js, false) {
                Ok(remote_obj) => {
                    let t = remote_obj.value.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    let preview = if t.chars().count() > 5000 { t.chars().take(5000).collect::<String>() + "\n..." } else { t };
                    (format!("【当前页正文】：\n{}", preview), String::new(), true)
                }
                Err(e) => (String::new(), format!("读取失败: {}", e), false),
            }
        }
        "screenshot" => {
            match tab.capture_screenshot(headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png, None, None, true) {
                Ok(data) => {
                    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data);
                    (format!("data:image/png;base64,{}", b64), String::new(), true)
                }
                Err(e) => (String::new(), format!("截图失败: {:?}", e), false),
            }
        }

        // ===== 滚动类 =====
        "scroll" => {
            let direction = arg1.unwrap_or_else(|| "down".to_string());
            let js = match direction.as_str() {
                "up" => "window.scrollBy({ top: -window.innerHeight * 0.8, behavior: 'smooth' }); true;",
                "top" => "window.scrollTo({ top: 0, behavior: 'smooth' }); true;",
                "bottom" => "window.scrollTo({ top: document.body.scrollHeight, behavior: 'smooth' }); true;",
                _ => "window.scrollBy({ top: window.innerHeight * 0.8, behavior: 'smooth' }); true;",
            };
            let _ = tab.evaluate(js, false);
            std::thread::sleep(Duration::from_millis(800));
            (format!("✅ 页面已向 {} 滚动", direction), String::new(), true)
        }

        // ===== 万能 JS =====
        "eval" | "js" => {
            // eval document.title → 运行任意 JS 并返回结果
            let code = format!("{} {}", arg1.unwrap_or_default(), arg2.unwrap_or_default());
            match tab.evaluate(&code, false) {
                Ok(remote_obj) => {
                    let result = remote_obj.value
                        .map(|v| if v.is_string() { v.as_str().unwrap_or("").to_string() } else { v.to_string() })
                        .unwrap_or_else(|| "undefined".to_string());
                    (format!("JS 结果: {}", result), String::new(), true)
                }
                Err(e) => (String::new(), format!("JS 执行失败: {:?}", e), false),
            }
        }

        _ => (String::new(), format!("❌ 未知浏览器指令: {}", cmd_type), false),
    }
}

