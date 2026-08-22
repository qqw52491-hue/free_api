//! 静态提示词与内置工具菜单组装。
//!
//! 从 main_loop 抽出的纯组装逻辑：核心系统提示词加载、task_store 数据区说明、
//! 内置本地工具菜单扫描。输出与原内联实现字节级一致，便于前缀缓存。

use tauri::AppHandle;
use tauri::Manager;

/// 加载核心系统提示词：优先资源目录，逐级回退到源码目录，最后用编译期嵌入副本。
pub fn load_base_prompt(app: &AppHandle) -> String {
    let core_logic_embedded = include_str!("../../prompts/core_logic.md");
    let prompt_path = app
        .path()
        .resource_dir()
        .unwrap_or_default()
        .join("prompts/core_logic.md");
    std::fs::read_to_string(&prompt_path)
        .or_else(|_| std::fs::read_to_string("src-tauri/prompts/core_logic.md"))
        .or_else(|_| std::fs::read_to_string("prompts/core_logic.md"))
        .unwrap_or_else(|_| core_logic_embedded.to_string())
}

/// task_store 数据区动作说明（静态常量文本）。
pub fn task_store_menu() -> &'static str {
    r#"
## 会话任务数据区 task_store 动作详情
当采集、解析或批处理产生结构化中间数据时，写入 task_store（tool: "task_store"），参数放在 command 对象中：
- create_resource：创建数据集。参数 kind、label，可选 resource_id、schema、summary、cursor。
- upsert_records：实时写入或更新记录。参数 resource_id、records 数组；每条记录宜提供 id 或 record_key，以保证幂等更新；可同时写 summary、cursor、status。
- list_resources / get_resource：读取数据集目录和元数据。
- list_records：分页读取数据行。参数 resource_id，可选 limit（最多 100）、offset。
- get_record：读取一条记录。参数 resource_id、record_key。
- update_resource：更新资源 label、summary、status、cursor 或 schema。
数据行会在工具返回时立刻持久化，并按当前 session 隔离；模型默认只看到资源目录与摘要，需要具体数据时再分页查询。
"#
}

/// 扫描 prompts/tools/*.md 生成内置本地工具菜单；一个目录都没读到时回退到内置清单。
pub fn build_local_tools_menu(app: &AppHandle) -> String {
    let mut local_tools_menu = String::from("\n## 内置本地工具 (Built-in Tools)\n");
    let candidate_dirs = [
        std::path::PathBuf::from("src-tauri/prompts/tools"),
        std::path::PathBuf::from("prompts/tools"),
        std::path::PathBuf::from("../src-tauri/prompts/tools"),
        app.path()
            .resource_dir()
            .unwrap_or_default()
            .join("prompts/tools"),
    ];

    let mut found_any = false;
    for dir in &candidate_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut read_count = 0;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("md") {
                    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        let summary = content
                            .find("<summary>")
                            .and_then(|start| {
                                content
                                    .find("</summary>")
                                    .map(|end| &content[start + 9..end])
                            })
                            .unwrap_or("无详细描述");
                        local_tools_menu.push_str(&format!("- {}: {}\n", name, summary));
                        read_count += 1;
                    }
                }
            }
            if read_count > 0 {
                found_any = true;
                break;
            }
        }
    }
    if !found_any {
        local_tools_menu.push_str("- finish: 终态交付动作\n- ask_human: 询问人类或请求确认\n- model_manager: 模型规格与渠道配置管理工具\n- mcp_manager: 外部 MCP 插件接入与能力卡管理工具\n- skill_manager: 工作区 Skill 与自定义提示词管理工具\n- web_search: 全网实时搜索引擎\n- web_fetch: 网页正文 Markdown 提取工具\n- task_store: 结构化数据存储与分页检索\n- search: 代码与历史日志联邦检索\n");
    }
    local_tools_menu
}
