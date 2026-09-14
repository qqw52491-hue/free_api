//! 原生内置互联网搜索与网页正文 Markdown 提取模块 (web_search & web_fetch)
//!
//! 设计哲学：
//! 1. 0 外部 Node/Python 进程依赖，纯 Rust 异步并发与零进程膨胀；
//! 2. 支持多搜索引擎聚合与优雅自动降级 (Tavily -> Brave -> Wikipedia/Public Knowledge)；
//! 3. 原生集成网页正文提纯转 Markdown (Jina Reader 镜像 + 本地 HTML 规则清洗降级)；
//! 4. 引用格式标准结构化，无缝直连客户端独立的微信式原生 WebView 预览窗口。

use super::types::DispatchResult;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchResultItem {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub source: String,
}

/// TinyFish 免费档内置密钥：随仓库分发（私有仓库），保证零配置开箱即用。
/// 环境变量 TINYFISH_API_KEY 设置时优先于内置值（便于换 key / 多 key 轮换）。
/// 泄露风险评估：免费档限流 30 次/分钟 Search，最坏后果是免费额度被蹭。
const DEFAULT_TINYFISH_API_KEY: &str = "sk-tinyfish-UDFwFLn_4R2v-28ewv6CBdoh1UnThZzC";

/// TinyFish 密钥解析：环境变量优先，缺省回落内置密钥。
fn tinyfish_key() -> String {
    match std::env::var("TINYFISH_API_KEY") {
        Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => DEFAULT_TINYFISH_API_KEY.to_string(),
    }
}

/// 执行聚合网络搜索
pub async fn execute_web_search(
    query: &str,
    max_results: usize,
) -> Result<Vec<WebSearchResultItem>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("搜索关键词不能为空".to_string());
    }

    let limit = if max_results == 0 {
        5
    } else {
        max_results.min(10)
    };

    // 0. TinyFish 免费 Agent 搜索 API (X-API-Key 认证，Search 免费 30 次/分钟)；
    //    API 级稳定性远高于 Bing 免费抓取，内置密钥零配置开箱即用。
    let tinyfish_key = tinyfish_key();
    if !tinyfish_key.is_empty() {
        if let Ok(res) = search_tinyfish(q, &tinyfish_key, limit).await {
            if !res.is_empty() {
                return Ok(res);
            }
        }
    }

    // 1. 免 Key 实时全网搜索兜底 (基于 Bing 开放 RSS 协议，0 成本 0 配置 实时全网检索)
    if let Ok(res) = search_bing_free(q, limit).await {
        if !res.is_empty() {
            return Ok(res);
        }
    }

    // 2. 检查是否有用户配置或环境变量中的 Tavily API Key (高阶可选增强)
    if let Ok(key) = std::env::var("TAVILY_API_KEY") {
        if !key.trim().is_empty() {
            if let Ok(res) = search_tavily(q, key.trim(), limit).await {
                if !res.is_empty() {
                    return Ok(res);
                }
            }
        }
    }

    // 3. 检查是否有 Brave Search API Key (高阶可选增强)
    if let Ok(key) = std::env::var("BRAVE_API_KEY") {
        if !key.trim().is_empty() {
            if let Ok(res) = search_brave(q, key.trim(), limit).await {
                if !res.is_empty() {
                    return Ok(res);
                }
            }
        }
    }

    // 4. 百科权威知识库检索 (免 Key)
    let wiki_res = search_wikipedia(q, limit).await.unwrap_or_default();
    if !wiki_res.is_empty() {
        return Ok(wiki_res);
    }

    // 5. 若无结果，返回友好提示
    Ok(vec![WebSearchResultItem {
        title: format!("关于 '{}' 的网络检索建议", q),
        url: format!("https://cn.bing.com/search?q={}", urlencoding(q)),
        snippet: format!("未检索到关于 '{}' 的即时权威摘要。如需高精度全局网络搜索，可配置 TAVILY_API_KEY 或 BRAVE_API_KEY。", q),
        source: "Aggregated Search".to_string(),
    }])
}

/// TinyFish Agent 搜索适配器 (免费档 Search 30 次/分钟；认证: X-API-Key 请求头)
async fn search_tinyfish(
    query: &str,
    api_key: &str,
    limit: usize,
) -> Result<Vec<WebSearchResultItem>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .get("https://api.search.tinyfish.ai/")
        .header("X-API-Key", api_key)
        .query(&[("query", query)])
        .send()
        .await
        .map_err(|e| format!("TinyFish 请求失败: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("TinyFish HTTP 状态码: {}", resp.status()));
    }

    let data: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("TinyFish JSON 解析失败: {}", e))?;

    let mut results = Vec::new();
    if let Some(arr) = data.get("results").and_then(|v| v.as_array()) {
        for item in arr.iter().take(limit) {
            let title = item
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let url = item
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let snippet = item
                .get("snippet")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if url.is_empty() {
                continue;
            }
            results.push(WebSearchResultItem {
                title,
                url,
                snippet,
                source: "TinyFish".to_string(),
            });
        }
    }
    Ok(results)
}

/// Tavily AI 搜索引擎适配器 (每月 1000 次免费)
async fn search_tavily(
    query: &str,
    api_key: &str,
    limit: usize,
) -> Result<Vec<WebSearchResultItem>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/128.0.0.0")
        .build()
        .map_err(|e| e.to_string())?;

    let payload = json!({
        "api_key": api_key,
        "query": query,
        "search_depth": "basic",
        "include_answer": true,
        "max_results": limit,
    });

    let resp = client
        .post("https://api.tavily.com/search")
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Tavily 请求失败: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Tavily HTTP 状态码: {}", resp.status()));
    }

    let data: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Tavily JSON 解析失败: {}", e))?;

    let mut results = Vec::new();
    if let Some(arr) = data.get("results").and_then(|v| v.as_array()) {
        for item in arr.iter().take(limit) {
            let title = item
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let url = item
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let snippet = item
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !title.is_empty() && !url.is_empty() {
                results.push(WebSearchResultItem {
                    title,
                    url,
                    snippet,
                    source: "Tavily".to_string(),
                });
            }
        }
    }

    Ok(results)
}

/// Brave 搜索引擎适配器
async fn search_brave(
    query: &str,
    api_key: &str,
    limit: usize,
) -> Result<Vec<WebSearchResultItem>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/128.0.0.0")
        .build()
        .map_err(|e| e.to_string())?;

    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
        urlencoding(query),
        limit
    );

    let resp = client
        .get(&url)
        .header("Accept", "application/json")
        .header("X-Subscription-Token", api_key)
        .send()
        .await
        .map_err(|e| format!("Brave 请求失败: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Brave HTTP 状态码: {}", resp.status()));
    }

    let data: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Brave JSON 解析失败: {}", e))?;

    let mut results = Vec::new();
    if let Some(arr) = data
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(|v| v.as_array())
    {
        for item in arr.iter().take(limit) {
            let title = item
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let url = item
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let snippet = item
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !title.is_empty() && !url.is_empty() {
                results.push(WebSearchResultItem {
                    title,
                    url,
                    snippet,
                    source: "Brave Search".to_string(),
                });
            }
        }
    }

    Ok(results)
}

/// Wikipedia 权威中文/英文开放知识库检索 (免 Key)
async fn search_wikipedia(query: &str, limit: usize) -> Result<Vec<WebSearchResultItem>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("FreeApiChat/1.0 (https://free-api-chat.app; contact@free-api-chat.app)")
        .build()
        .map_err(|e| e.to_string())?;

    // 先查中文维基百科
    let zh_url = format!(
        "https://zh.wikipedia.org/w/api.php?action=opensearch&search={}&limit={}&format=json",
        urlencoding(query),
        limit
    );

    let resp = client.get(&zh_url).send().await;
    if let Ok(r) = resp {
        if r.status().is_success() {
            if let Ok(arr) = r.json::<serde_json::Value>().await {
                if let Some(res) = parse_wikipedia_opensearch(&arr, limit) {
                    if !res.is_empty() {
                        return Ok(res);
                    }
                }
            }
        }
    }

    // 英文维基百科后备
    let en_url = format!(
        "https://en.wikipedia.org/w/api.php?action=opensearch&search={}&limit={}&format=json",
        urlencoding(query),
        limit
    );
    if let Ok(r) = client.get(&en_url).send().await {
        if r.status().is_success() {
            if let Ok(arr) = r.json::<serde_json::Value>().await {
                if let Some(res) = parse_wikipedia_opensearch(&arr, limit) {
                    if !res.is_empty() {
                        return Ok(res);
                    }
                }
            }
        }
    }

    Ok(Vec::new())
}

fn parse_wikipedia_opensearch(
    val: &serde_json::Value,
    limit: usize,
) -> Option<Vec<WebSearchResultItem>> {
    let arr = val.as_array()?;
    if arr.len() < 4 {
        return None;
    }
    let titles = arr.get(1)?.as_array()?;
    let snippets = arr.get(2)?.as_array()?;
    let urls = arr.get(3)?.as_array()?;

    let mut results = Vec::new();
    for i in 0..titles.len().min(limit) {
        let title = titles
            .get(i)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let snippet = snippets
            .get(i)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let url = urls
            .get(i)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !title.is_empty() && !url.is_empty() {
            results.push(WebSearchResultItem {
                title,
                url,
                snippet: if snippet.is_empty() {
                    format!("维基百科关于该主题的条目档案")
                } else {
                    snippet
                },
                source: "Wikipedia".to_string(),
            });
        }
    }
    Some(results)
}

/// 100% 免 API Key 的实时全网搜索引擎 (基于 Bing 开放 RSS 协议)
async fn search_bing_free(query: &str, limit: usize) -> Result<Vec<WebSearchResultItem>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36")
        .build()
        .map_err(|e| e.to_string())?;

    // 优先中文 Bing，支持全球内容
    let url = format!(
        "https://cn.bing.com/search?q={}&format=rss",
        urlencoding(query)
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Bing 请求失败: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Bing HTTP 状态异常: {}", resp.status()));
    }

    let xml = resp.text().await.unwrap_or_default();
    let results = parse_bing_rss_xml(&xml, limit);
    if results.is_empty() {
        // 后备国际站
        let en_url = format!(
            "https://www.bing.com/search?q={}&format=rss",
            urlencoding(query)
        );
        if let Ok(r) = client.get(&en_url).send().await {
            if r.status().is_success() {
                let en_xml = r.text().await.unwrap_or_default();
                return Ok(parse_bing_rss_xml(&en_xml, limit));
            }
        }
    }
    Ok(results)
}

/// 解析 Bing RSS XML 格式的实时全网结果
pub fn parse_bing_rss_xml(xml: &str, limit: usize) -> Vec<WebSearchResultItem> {
    let mut results = Vec::new();
    let mut remaining = xml;

    while let Some(start_idx) = remaining.find("<item>") {
        let after_item = &remaining[start_idx + 6..];
        let end_idx = match after_item.find("</item>") {
            Some(idx) => idx,
            None => break,
        };
        let item_content = &after_item[..end_idx];
        remaining = &after_item[end_idx + 7..];

        let title = extract_xml_tag(item_content, "title");
        let link = extract_xml_tag(item_content, "link");
        let description = extract_xml_tag(item_content, "description");

        if !title.is_empty() && !link.is_empty() {
            results.push(WebSearchResultItem {
                title: clean_xml_text(&title),
                url: link.trim().to_string(),
                snippet: clean_xml_text(&description),
                source: "Bing (免Key直连)".to_string(),
            });
            if results.len() >= limit {
                break;
            }
        }
    }
    results
}

fn extract_xml_tag(content: &str, tag: &str) -> String {
    let open_tag = format!("<{}>", tag);
    let close_tag = format!("</{}>", tag);
    if let Some(start) = content.find(&open_tag) {
        if let Some(end) = content[start + open_tag.len()..].find(&close_tag) {
            return content[start + open_tag.len()..start + open_tag.len() + end].to_string();
        }
    }
    String::new()
}

fn clean_xml_text(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("<strong>", "")
        .replace("</strong>", "")
        .replace("<b>", "")
        .replace("</b>", "")
        .trim()
        .to_string()
}

/// 抓取任意 URL 并提纯为干净的 Markdown 正文
pub async fn fetch_url_markdown(url: &str) -> Result<String, String> {
    let target_url = url.trim();
    if target_url.is_empty() {
        return Err("URL 不能为空".to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/128.0.0.0 Safari/537.36")
        .build()
        .map_err(|e| e.to_string())?;

    // 优先尝试 Jina Reader 免 Key 高清提取引擎 (https://r.jina.ai/{url})
    let jina_url = format!("https://r.jina.ai/{}", target_url);
    if let Ok(resp) = client.get(&jina_url).send().await {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                let trimmed = text.trim();
                if !trimmed.is_empty() && !trimmed.contains("AuthenticationRequiredError") {
                    return Ok(truncate_markdown(trimmed, 6000));
                }
            }
        }
    }

    // 兜底：直接 GET 原始 HTML 并进行本地清洗
    let resp = client
        .get(target_url)
        .send()
        .await
        .map_err(|e| format!("抓取网页失败: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("网页响应状态异常: {}", resp.status()));
    }

    let html = resp.text().await.unwrap_or_default();
    let cleaned = clean_html_to_markdown(&html);
    Ok(truncate_markdown(&cleaned, 6000))
}

/// 本地高效 HTML 清洗与 Markdown 提纯
pub fn clean_html_to_markdown(html: &str) -> String {
    // 1. 去除无用的 script, style, svg, noscript, nav, header, footer 标签及内部内容
    let mut text = html.to_string();
    let unwanted_tags = [
        "script", "style", "svg", "noscript", "nav", "header", "footer", "iframe",
    ];
    for tag in unwanted_tags {
        let open_pattern = format!("<{}", tag);
        let close_pattern = format!("</{}>", tag);
        while let Some(start) = text.to_ascii_lowercase().find(&open_pattern) {
            if let Some(end) = text.to_ascii_lowercase()[start..].find(&close_pattern) {
                text.replace_range(start..start + end + close_pattern.len(), " ");
            } else if let Some(tag_end) = text[start..].find('>') {
                text.replace_range(start..start + tag_end + 1, " ");
            } else {
                break;
            }
        }
    }

    // 2. 将段落与标题标签转换为换行
    text = text.replace("<p>", "\n\n").replace("</p>", "\n");
    text = text
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n");
    text = text.replace("<h1>", "\n# ").replace("</h1>", "\n");
    text = text.replace("<h2>", "\n## ").replace("</h2>", "\n");
    text = text.replace("<h3>", "\n### ").replace("</h3>", "\n");
    text = text.replace("<li>", "\n- ").replace("</li>", "");

    // 3. 剥除所有剩余的 HTML 标签 <...>
    let mut in_tag = false;
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }

    // 4. 清理 HTML 实体与多余空白
    let unescaped = result
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");

    // 5. 压缩多余连续空行
    let mut lines = Vec::new();
    for line in unescaped.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            lines.push(trimmed);
        }
    }
    lines.join("\n\n")
}

fn truncate_markdown(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}\n\n*(正文过长，已截断前 {} 字符)*", truncated, max_chars)
    }
}

fn urlencoding(s: &str) -> String {
    let mut encoded = String::new();
    for b in s.as_bytes() {
        match *b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*b as char);
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{:02X}", b)),
        }
    }
    encoded
}

fn block_on_async<F: std::future::Future + Send + 'static>(future: F) -> F::Output
where
    F::Output: Send + 'static,
{
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to build tokio runtime")
            .block_on(future)
    })
    .join()
    .expect("Web search worker thread panicked")
}

/// Agent 执行器专用工具分发入口 (web_search / web_fetch)
pub fn dispatch(tool: &str, params: &serde_json::Value) -> DispatchResult {
    let tool_lower = tool.to_ascii_lowercase();

    if tool_lower == "web_search" {
        let query = params
            .get("query")
            .or_else(|| params.get("q"))
            .or_else(|| params.get("keyword"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        if query.is_empty() {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: "web_search 缺少必填参数 'query' (搜索关键词)".to_string(),
                success: false,
                route: "web_search_invalid".to_string(),
            };
        }

        let max_results = params
            .get("max_results")
            .or_else(|| params.get("limit"))
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let query_str = query.to_string();
        // 线程池与 Reactor 安全的同步等待
        match block_on_async(async move { execute_web_search(&query_str, max_results).await }) {
            Ok(results) => {
                let mut md = format!("### 🌐 网络检索结果 (关键词: \"{}\")\n\n", query);
                for (idx, item) in results.iter().enumerate() {
                    md.push_str(&format!(
                        "{}. **[{}]({})**  *(来源: {})*\n   > {}\n\n",
                        idx + 1,
                        item.title,
                        item.url,
                        item.source,
                        item.snippet
                    ));
                }
                DispatchResult { view: None,
                    stdout: md,
                    stderr: String::new(),
                    success: true,
                    route: "web_search_success".to_string(),
                }
            }
            Err(e) => DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!("网络搜索异常: {}", e),
                success: false,
                route: "web_search_failed".to_string(),
            },
        }
    } else if tool_lower == "web_fetch" {
        let url = params
            .get("url")
            .or_else(|| params.get("link"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        if url.is_empty() {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: "web_fetch 缺少必填参数 'url' (目标网页地址)".to_string(),
                success: false,
                route: "web_fetch_invalid".to_string(),
            };
        }

        let url_str = url.to_string();
        match block_on_async(async move { fetch_url_markdown(&url_str).await }) {
            Ok(content) => DispatchResult { view: None,
                stdout: format!("### 📄 网页正文提纯 ({})\n\n{}", url, content),
                stderr: String::new(),
                success: true,
                route: "web_fetch_success".to_string(),
            },
            Err(e) => DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!("提取网页正文失败 ({}): {}", url, e),
                success: false,
                route: "web_fetch_failed".to_string(),
            },
        }
    } else {
        DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!("未知网络工具: {}", tool),
            success: false,
            route: "unknown_web_tool".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_html_to_markdown() {
        let html = r#"
            <html>
                <head><title>Test Page</title><style>.hidden { display: none; }</style></head>
                <body>
                    <script>console.log("bad");</script>
                    <nav>Home | About</nav>
                    <h1>Rust 编程语言</h1>
                    <p>Rust 是一门注重安全与性能的系统级编程语言。&nbsp;&amp;&nbsp;极速并发。</p>
                    <ul>
                        <li>内存安全</li>
                        <li>零成本抽象</li>
                    </ul>
                    <footer>Copyright 2026</footer>
                </body>
            </html>
        "#;
        let md = clean_html_to_markdown(html);
        assert!(md.contains("# Rust 编程语言"));
        assert!(md.contains("Rust 是一门注重安全与性能的系统级编程语言。 & 极速并发。"));
        assert!(md.contains("- 内存安全"));
        assert!(md.contains("- 零成本抽象"));
        assert!(!md.contains("console.log"));
        assert!(!md.contains("display: none"));
        assert!(!md.contains("Copyright 2026"));
    }

    #[test]
    fn test_parse_wikipedia_opensearch() {
        let raw = json!([
            "Rust",
            ["Rust", "Rust (编程语言)"],
            ["化学概念", "一门注重安全与性能的系统语言"],
            [
                "https://zh.wikipedia.org/wiki/Rust",
                "https://zh.wikipedia.org/wiki/Rust_(编程语言)"
            ]
        ]);
        let items = parse_wikipedia_opensearch(&raw, 5).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Rust");
        assert_eq!(items[1].title, "Rust (编程语言)");
        assert_eq!(items[1].source, "Wikipedia");
    }

    #[test]
    fn test_parse_bing_rss_xml() {
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?><rss version="2.0"><channel><item><title>Rust 编程语言官网</title><link>https://www.rust-lang.org/</link><description>一门赋予每个人构建可靠且高效软件能力的语言。</description></item></channel></rss>"#;
        let items = parse_bing_rss_xml(xml, 5);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Rust 编程语言官网");
        assert_eq!(items[0].url, "https://www.rust-lang.org/");
        assert_eq!(items[0].source, "Bing (免Key直连)");
    }

    #[tokio::test]
    async fn test_dispatch_web_search_and_fetch() {
        let search_res = dispatch("web_search", &json!({ "query": "Rust" }));
        assert!(search_res.success);
        assert!(search_res.stdout.contains("网络检索结果"));

        let fetch_res = dispatch("web_fetch", &json!({ "url": "" }));
        assert!(!fetch_res.success);
        assert!(fetch_res.stderr.contains("缺少必填参数"));
    }

    /// 实况联调（无需环境变量，使用内置密钥；默认忽略）：
    /// cargo test search_tinyfish_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn search_tinyfish_live() {
        let key = tinyfish_key();
        if key.trim().is_empty() {
            println!("未配置 TinyFish 密钥，跳过实况验证");
            return;
        }
        let res = search_tinyfish("Hacker News top story", key.trim(), 3)
            .await
            .expect("TinyFish 实况调用失败");
        assert!(!res.is_empty(), "TinyFish 返回空结果");
        for item in &res {
            println!("· {} | {} | {}", item.title, item.url, item.source);
        }
        assert!(res.iter().all(|i| i.source == "TinyFish"));
    }
}
