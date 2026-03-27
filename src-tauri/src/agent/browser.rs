use std::sync::{Arc, OnceLock};
use tokio::time::Duration;
use headless_chrome::{Browser, Tab, LaunchOptions};
use serde::{Deserialize, Serialize};

static GLOBAL_BROWSER: OnceLock<Browser> = OnceLock::new();
static GLOBAL_TAB: OnceLock<Arc<Tab>> = OnceLock::new();

pub fn get_or_create_tab() -> Result<Arc<Tab>, String> {
    if let Some(tab) = GLOBAL_TAB.get() {
        return Ok(tab.clone());
    }

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
    let tab = browser
        .new_tab()
        .map_err(|e| format!("新建标签页失败: {:?}", e))?;

    let _ = GLOBAL_BROWSER.set(browser);
    let _ = GLOBAL_TAB.set(tab.clone());

    Ok(tab)
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

    let (action, url, id, text) = if command_str.trim().starts_with('{') {
        let action = "extract".to_string();
        (action, None, None, None)
    } else {
        let parts: Vec<&str> = command_str.splitn(3, ' ').collect();
        let cmd_type = parts.get(0).unwrap_or(&"").to_lowercase();
        match cmd_type.as_str() {
            "goto" | "navigate" => (
                "navigate".to_string(),
                parts.get(1).map(|s| s.to_string()),
                None,
                None,
            ),
            "extract" | "look" => ("extract".to_string(), None, None, None),
            "click" => {
                let id = parts.get(1).and_then(|s| s.parse::<u32>().ok());
                ("click".to_string(), None, id, None)
            }
            "type" => {
                let id = parts.get(1).and_then(|s| s.parse::<u32>().ok());
                let val = parts.get(2).map(|s| s.to_string());
                ("type".to_string(), None, id, val)
            }
            "press" => {
                let key = parts.get(1).map(|s| s.to_string());
                ("press".to_string(), None, None, key)
            }
            "read" => ("read".to_string(), None, None, None),
            "scroll" => {
                let dir = parts.get(1).map(|s| s.to_string());
                ("scroll".to_string(), None, None, dir)
            }
            "hover" => {
                let id = parts.get(1).and_then(|s| s.parse::<u32>().ok());
                ("hover".to_string(), None, id, None)
            }
            "screenshot" => ("screenshot".to_string(), None, None, None),
            _ => ("unknown".to_string(), None, None, None),
        }
    };

    match action.as_str() {
        "navigate" => {
            let target_url = url.unwrap_or_default();
            if !target_url.starts_with("http") {
                return (String::new(), "URL 格式不正确".to_string(), false);
            }
            if let Err(e) = tab.navigate_to(&target_url) {
                return (String::new(), format!("跳转失败: {:?}", e), false);
            }
            if let Err(e) = tab.wait_until_navigated() {
                return (String::new(), format!("加载等待失败: {:?}", e), false);
            }
            let title = tab.get_title().unwrap_or_default();
            (format!("成功跳转！标题: {}", title), String::new(), true)
        }
        "extract" => {
            let js = r#"
            (function() {
                const isVisible = (elem) => !!( elem.offsetWidth || elem.offsetHeight || elem.getClientRects().length );
                const interactables = Array.from(document.querySelectorAll('a, button, input, textarea, select, [contenteditable="true"], [role="button"], [role="link"], [role="menuitem"], [tabindex]:not([tabindex="-1"]), [class*="button" i], [class*="btn" i]')).filter(isVisible);

                let textNodes = [];
                let treeWalker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, null, false);
                let currentNode;
                while(currentNode = treeWalker.nextNode()) {
                    if(currentNode.textContent.trim().length > 1) {
                        let parent = currentNode.parentElement;
                        if(parent && isVisible(parent) && !parent.closest('a, button, input, textarea, select, [contenteditable="true"], [role="button"]')) {
                            textNodes.push(parent);
                        }
                    }
                }

                let allElements = Array.from(new Set([...interactables, ...textNodes]));
                let results = allElements.map((el, i) => {
                    const rect = el.getBoundingClientRect();
                    if (rect.width === 0 || rect.height === 0 || rect.bottom < 0 || rect.top > window.innerHeight) return null;

                    let id = i + 1;
                    el.setAttribute('data-tauri-agent-id', id);
                    el.style.outline = "2px solid red";

                    let text = el.getAttribute('aria-label') || el.getAttribute('title') || el.innerText || el.value || el.placeholder;
                    if (!text || text.trim() === '') {
                        if (el.tagName === 'INPUT' && el.type === 'checkbox') text = el.checked ? "已勾选复选框" : "未勾选复选框";
                        else if (el.tagName === 'INPUT') text = "输入框";
                        else if (el.querySelector('svg') || el.querySelector('img')) text = "图标/图片按钮";
                        else text = "无文本交互区";
                    }

                    let tag = el.tagName.toLowerCase();
                    let cx = Math.round(rect.x + rect.width / 2);
                    let cy = Math.round(rect.y + rect.height / 2);
                    return `[${id}] <${tag}> (X:${cx}, Y:${cy}): ${text.substring(0, 60).replace(/\n/g, ' ')}`;
                }).filter(r => r !== null);

                let scrollStatus = `【页面状态】: 视口宽度 ${window.innerWidth}, 视口高度 ${window.innerHeight}, 当前滚动高度 ${Math.round(window.scrollY)} / 总高度 ${document.body.scrollHeight}`;
                return scrollStatus + '\n【当前屏幕交互元素清单】:\n' + results.join('\n');
            })();
            "#;
            match tab.evaluate(js, false) {
                Ok(remote_obj) => {
                    let text = remote_obj.value.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    (text, String::new(), true)
                }
                Err(e) => (String::new(), format!("提取DOM失败: {}", e), false),
            }
        }
        "click" => {
            let id = id.unwrap_or(0);
            let js = format!(r#"
                let el = document.querySelector('[data-tauri-agent-id="{}"]');
                if (el) {{
                    el.scrollIntoView({{behavior: 'instant', block: 'center'}});
                    el.dispatchEvent(new MouseEvent('mouseover', {{bubbles: true}}));
                    el.dispatchEvent(new MouseEvent('mousedown', {{bubbles: true}}));
                    el.dispatchEvent(new MouseEvent('mouseup', {{bubbles: true}}));
                    el.dispatchEvent(new MouseEvent('click', {{bubbles: true}}));
                    if (el.tagName.toLowerCase() === 'a' && el.href) window.location.href = el.href;
                    true;
                }} else {{ false; }}
            "#, id);
            match tab.evaluate(&js, false) {
                Ok(res) if res.value.as_ref().and_then(|v| v.as_bool()).unwrap_or(false) => (format!("✅ 成功点击元素 [{}]", id), String::new(), true),
                _ => (String::new(), format!("❌ 找不到元素 [{}]", id), false),
            }
        }
        "type" => {
            let id = id.unwrap_or(0);
            let val = text.unwrap_or_default();
            let js_type = format!(r#"
                let el = document.querySelector('[data-tauri-agent-id="{}"]');
                if (el) {{
                    el.scrollIntoView({{behavior: 'instant', block: 'center'}});
                    el.focus();
                    el.value = '{}';
                    if(el.isContentEditable) el.innerText = '{}';
                    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    true;
                }} else {{ false; }}
            "#, id, val, val);
            if tab.evaluate(&js_type, false).is_err() { return (String::new(), format!("❌ 找不到元素 [{}]", id), false); }
            let _ = tab.type_str(&val);
            (format!("✅ 成功输入: {}", val), String::new(), true)
        }
        "press" => {
            let key = text.unwrap_or_else(|| "Enter".to_string());
            if tab.press_key(&key).is_err() { return (String::new(), format!("❌ 按键 {} 失败", key), false); }
            (format!("✅ 成功按下 [{}] 键", key), String::new(), true)
        }
        "scroll" => {
            let direction = text.unwrap_or_else(|| "down".to_string());
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
        "hover" => {
            let id = id.unwrap_or(0);
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
        _ => (String::new(), format!("❌ 未知指令: {}", action), false),
    }
}
