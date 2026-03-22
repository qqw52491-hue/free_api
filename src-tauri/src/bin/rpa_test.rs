use anyhow::Result;
use headless_chrome::{Browser, LaunchOptions};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead};
use std::time::Duration;

#[derive(Deserialize, Debug)]
struct AgentCommand {
    tool: String,
    url: Option<String>,
    question: Option<String>, // 给 Kimi 准备的问题
}

#[derive(Serialize, Debug)]
struct AgentResponse {
    status: String,
    message: String,
    data: Option<String>,
}

fn main() -> Result<()> {
    println!("🚀 Rust RPA Agent 测试终端启动！");
    println!("💡 这个程序模拟了 MCP (Model Context Protocol) 服务端。");
    println!("💡 在实际应用中，大模型 (如 Claude/Cursor) 会通过标准输入偷偷发送 JSON 给这个程序，它就会自动干活。");
    println!("💡 请在下方粘贴这行 JSON 并回车体验【全自动物理键盘盲打 Kimi】：\n{{\"tool\": \"ask_kimi\", \"url\": \"https://kimi.moonshot.cn\", \"question\": \"你好，请用一句话夸夸我\"}}\n");

    let stdin = io::stdin();
    
    // 启动监听循环，充当底层协议服务端
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        // 解析输入的 JSON
        match serde_json::from_str::<AgentCommand>(&line) {
            Ok(cmd) => {
                println!("\n🤖 收到指令: [{}]，正在执行物理界操作...", cmd.tool);
                
                // 路由指令到对应的机械臂动作
                let response = match cmd.tool.as_str() {
                    "screenshot" => {
                        if let Some(url) = cmd.url {
                            perform_browser_task(&url)
                        } else {
                            error_response("缺少 url 参数")
                        }
                    }
                    "extract_dom" => {
                        if let Some(url) = cmd.url {
                            perform_extract_dom(&url)
                        } else {
                            error_response("缺少 url 参数")
                        }
                    }
                    "ask_kimi" => {
                        if let Some(url) = cmd.url {
                            // 给 Kimi 传入预设好的问题，若没有则默认 "你好"
                            let q = cmd.question.unwrap_or_else(|| "你好".to_string());
                            perform_ask_kimi(&url, &q)
                        } else {
                            error_response("缺少 url 参数")
                        }
                    }
                    _ => error_response(&format!("抱歉，不支持 '{}'", cmd.tool)),
                };
                
                // 将执行结果转化为 JSON 打印出来
                let result_json = serde_json::to_string(&response)?;
                println!("📤 执行完毕，返回给大模型的响应: {}\n", result_json);
            }
            Err(e) => {
                println!("❌ JSON 解析失败: {}", e);
            }
        }
    }

    Ok(())
}

fn perform_browser_task(url: &str) -> AgentResponse {
    // 拉起浏览器，调试时 headless 设为 false，看看幽灵是怎么自己动鼠标的！
    let options = LaunchOptions::default_builder()
        .headless(false) // 这里设为 false，让你亲眼看到浏览器自己弹出来并且被操作！
        .build()
        .expect("创建浏览器配置失败");

    let browser = match Browser::new(options) {
        Ok(b) => b,
        Err(e) => return error_response(&format!("浏览器拉起失败: {}", e)),
    };

    let tab = match browser.wait_for_initial_tab() {
        Ok(t) => t,
        Err(_) => return error_response("无法获取初始标签页"),
    };

    println!("🌐 正在操控浏览器导航至: {}", url);
    if tab.navigate_to(url).is_err() || tab.wait_until_navigated().is_err() {
        return error_response("页面导航超时被墙，或网址有误");
    }

    println!("⏳ 等待页面渲染 (3秒)...");
    std::thread::sleep(Duration::from_secs(3));

    println!("📸 页面渲染完毕，准备底层截图...");
    match tab.capture_screenshot(
        headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
        None,
        None,
        true, // 截取全屏长图
    ) {
        Ok(png_data) => {
            let file_name = "test_screenshot.png".to_string();
            // 保存到当前目录
            let path = std::env::current_dir().unwrap().join(&file_name);
            if std::fs::write(&path, &png_data).is_ok() {
                AgentResponse {
                    status: "success".to_string(),
                    message: "底层截图成功".to_string(),
                    data: Some(path.to_string_lossy().to_string()),
                }
            } else {
                error_response("文件保存失败，是不是没权限？")
            }
        }
        Err(_) => error_response("截图操作遇到异常"),
    }
}

fn error_response(msg: &str) -> AgentResponse {
    AgentResponse {
        status: "error".to_string(),
        message: msg.to_string(),
        data: None,
    }
}

// ================= 最新黑科技：极简交互树提取 =================
fn perform_extract_dom(url: &str) -> AgentResponse {
    let options = LaunchOptions::default_builder().headless(false).build().unwrap();
    let browser = match Browser::new(options) {
        Ok(b) => b,
        Err(e) => return error_response(&format!("浏览器拉起失败: {}", e)),
    };

    let tab = match browser.new_tab() {
        Ok(t) => t,
        Err(_) => return error_response("无法获取初始标签页"),
    };

    println!("🌐 正在导航至: {}", url);
    if tab.navigate_to(url).is_err() || tab.wait_until_navigated().is_err() {
        return error_response("页面导航超时被墙，或网址有误");
    }

    println!("⏳ 等待大模型级别的现代网页 (如 Kimi) 渲染完整结构 (等 5 秒)...");
    std::thread::sleep(Duration::from_secs(5));

    println!("🔍 正在通过 CDP 神之手直接注入 JS 吸取元素坐标...");

    // 核心奥义：不仅抓标准的 button，还要抓现代前端 (Vue/React) 最喜欢用的 contenteditable 富文本输入框！
    // 同时把页面上所有可见的、能点的文字（伪按钮）也全吸出来
    let js_script = r#"
        (function() {
            const interactableSelectors = 'a, button, input, textarea, select, [role="button"], [role="link"], [role="textbox"], [contenteditable="true"]';
            let interactables = Array.from(document.querySelectorAll(interactableSelectors));
            
            // 找出带有文字的可能元素 (扫除那些不写 button 标签、只用 div+click 的流氓组件)
            let textNodes = [];
            let treeWalker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, null, false);
            let currentNode;
            while(currentNode = treeWalker.nextNode()) {
                if(currentNode.textContent.trim().length > 0) {
                    let parent = currentNode.parentElement;
                    if(parent && window.getComputedStyle(parent).display !== 'none' && !parent.closest(interactableSelectors)) {
                        textNodes.push(parent);
                    }
                }
            }
            
            // 合并并去重
            let allElements = Array.from(new Set([...interactables, ...textNodes]));
            
            let results = allElements.map(el => {
                const rect = el.getBoundingClientRect();
                if (rect.width === 0 || rect.height === 0 || rect.y < 0) return null; // 排除不可见或滚动条外的
                
                let text = "";
                let isInput = false;
                if (el.tagName.toLowerCase() === 'input' || el.tagName.toLowerCase() === 'textarea') {
                    text = el.value || el.placeholder || "";
                    isInput = true;
                } else if (el.isContentEditable) {
                    text = el.innerText || "【这里是一个可以输入的富文本框】";
                    isInput = true;
                } else {
                    text = el.innerText || "";
                }
                
                let is_clickable = el.matches(interactableSelectors) || window.getComputedStyle(el).cursor === 'pointer';
                
                return {
                    type: isInput ? "input_area" : (is_clickable ? "button" : "text"),
                    text: text.trim().substring(0, 50).replace(/\n/g, ' '),
                    x: Math.round(rect.x + rect.width / 2),
                    y: Math.round(rect.y + rect.height / 2)
                };
            }).filter(e => e && e.text.length > 0 && e.x > 0 && e.y > 0);
            
            return JSON.stringify(results);
        })()
    "#;

    match tab.evaluate(js_script, false) {
        Ok(remote_object) => {
            if let Some(val) = remote_object.value {
                if let Some(json_str) = val.as_str() {
                    // 这个 json_str 就是直接可以喂给大模型吃的精简地图！
                    println!("\n=========== 下面这就是提取出来的极简地图（喂给大模型的干粮） ===========");
                    // 格式化打印出来让你看清楚
                    let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap();
                    println!("{}", serde_json::to_string_pretty(&parsed).unwrap());
                    println!("========================================================================\n");
                    
                    AgentResponse {
                        status: "success".to_string(),
                        message: "极简交互树提取成功".to_string(),
                        data: Some("提取信息已打印在上方".to_string()),
                    }
                } else {
                    error_response("返回值不是字符串")
                }
            } else {
                error_response("脚本执行没有返回值")
            }
        }
        Err(_) => error_response("脚本神之手注入失败"),
    }
}

// ================= 最强物理特种兵：挂载本机缓存 + 连续对话流 =================
fn perform_ask_kimi(url: &str, question: &str) -> AgentResponse {
    use std::path::Path;

    // 绝招：不用你原生的 Chrome 目录了（因为你的号太多会触发选择账号界面，把自动化卡死）
    // 我们在这里建一个这个 AI 专属的“持久化独立缓存目录”
    let env_dir = std::env::current_dir().unwrap();
    let rpa_profile_path = env_dir.join(".rpa_profile");
    
    let options = LaunchOptions::default_builder()
        .headless(false)
        .user_data_dir(Some(rpa_profile_path)) // 👈 让 AI 拥有自己独立的浏览器分身，而且永不掉线！
        .build()
        .unwrap();

    let browser = match Browser::new(options) {
        Ok(b) => b,
        Err(e) => return error_response(&format!("拉起失败（你是不是没彻底退出 Chrome 程序？导致锁冲突了）：{}", e)),
    };

    let tab = match browser.new_tab() {
        Ok(t) => t,
        Err(_) => return error_response("新建标签页失败"),
    };

    println!("🌐 戴上你本人的登录面具，正在杀入: {}", url);
    if tab.navigate_to(url).is_err() || tab.wait_until_navigated().is_err() {
        return error_response("网太卡或导航崩溃了");
    }

    println!("⏳ 正在扫描输入框...");
    std::thread::sleep(Duration::from_secs(4)); 
    
    // 【第一步】锁定框
    let chat_input = match tab.wait_for_element("div[contenteditable='true']") {
        Ok(el) => el,
        Err(_) => return error_response("找不到输入框！难道还有反爬虫？"),
    };

    println!("🎯 已锁定 Kimi 聊天框，强击输入第一句话: \"{}\"", question);
    chat_input.click().ok();
    std::thread::sleep(Duration::from_millis(500)); 
    let _ = chat_input.type_into(question);
    
    std::thread::sleep(Duration::from_millis(500)); 
    let _ = tab.press_key("Enter");

    // 【第二步】重点来了！固定死等 10 秒听 Kimi 胡扯完
    println!("💥 第一波已发送！固定发呆 10 秒，等 Kimi 回答完...");
    println!("⏰ (等待中...)");
    std::thread::sleep(Duration::from_secs(10));

    // 【第三步】神之手再次出击，直接就当前对话追问！
    println!("🔄 10秒结束！强行插入二次追问！");
    // 通常发送完后焦点还在输入框或者需要重新去找
    let chat_input_2 = match tab.wait_for_element("div[contenteditable='true']") {
         Ok(el) => el,
         Err(_) => return error_response("追问时找不到输入框了！"),
    };
    
    chat_input_2.click().ok();
    std::thread::sleep(Duration::from_millis(500)); 
    
    let followup = "刚才说到哪了？你能把你说的再精简成3个字吗？";
    println!("🤖 第二波物理盲打输入: \"{}\"", followup);
    let _ = chat_input_2.type_into(followup);
    
    std::thread::sleep(Duration::from_millis(500)); 
    let _ = tab.press_key("Enter");

    println!("✨ 连续两次对话物理流打穿完爆！(10秒后程序自动清理退出...)");
    std::thread::sleep(Duration::from_secs(10)); 

    AgentResponse {
        status: "success".to_string(),
        message: "多轮对话全自动击穿完毕！".to_string(),
        data: None,
    }
}

