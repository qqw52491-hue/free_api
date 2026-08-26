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

#[derive(Clone, Copy)]
struct BuiltinToolSpec {
    name: &'static str,
    summary: &'static str,
    actions: &'static [&'static str],
}

/// 内置工具的首轮最小可执行契约。
///
/// 它只承载稳定的工具名、动作、必填字段与确认条件；长范例、罕见边界
/// 和外部 MCP schema 仍由 tool_manual 按需加载。真实 handler 继续负责最终校验。
const BUILTIN_TOOL_SPECS: &[BuiltinToolSpec] = &[
    BuiltinToolSpec {
        name: "finish",
        summary: "终态交付；仅在所有完成门控已闭合时使用",
        actions: &["finish(summary: string)"],
    },
    BuiltinToolSpec {
        name: "ask_human",
        summary: "请求人类输入、选择或高风险确认",
        actions: &["ask_human(message: string, form?: object)"],
    },
    BuiltinToolSpec {
        name: "get_step_output",
        summary: "分页取回已有工具输出的原文",
        actions: &["get_step_output(output_id: string, offset?: number, limit?: number)"],
    },
    BuiltinToolSpec {
        name: "search",
        summary: "检索会话历史、记忆与工作区代码",
        actions: &["search(query: string, kinds?: history|code|all, path?: string for code)"],
    },
    BuiltinToolSpec {
        name: "task_store",
        summary: "会话内结构化数据写入、分页读取与状态更新",
        actions: &[
            "create_resource(kind: string, label: string)",
            "upsert_records(resource_id: string, records: array)",
            "list_resources()",
            "get_resource(resource_id: string)",
            "list_records(resource_id: string, limit?: <=100, offset?: number)",
            "get_record(resource_id: string, record_key: string)",
            "update_resource(resource_id: string, label?|summary?|status?|cursor?|schema?)",
        ],
    },
    BuiltinToolSpec {
        name: "web_search",
        summary: "全网检索并返回带来源的结果",
        actions: &["web_search(query: string, max_results?: <=10)"],
    },
    BuiltinToolSpec {
        name: "web_fetch",
        summary: "提取网页正文为 Markdown",
        actions: &["web_fetch(url: string)"],
    },
    BuiltinToolSpec {
        name: "model_manager",
        summary: "维护模型规格与平台模型绑定",
        actions: &[
            "list_specs()",
            "upsert_spec(name: string, context_window?|max_tokens?|supports_thinking?|default_temperature?|description?)",
            "delete_spec(name|id: string, confirm: true)",
            "list_platforms()",
            "add_model(platform_id|platform_name: string, model_name: string, ...)",
            "delete_model(model_id|model_name: string, confirm: true)",
        ],
    },
    BuiltinToolSpec {
        name: "mcp_manager",
        summary: "接入、启停、批量导入和删除外部 MCP 插件",
        actions: &[
            "list()",
            "add_http(url: string, bearer_token?|token?|name?|transport?|model_id?)",
            "add_stdio(name: string, command: string, args?|env?|capability_card?)",
            "batch_import(input_text: string, model_id?)",
            "toggle(name: string, enabled: boolean)",
            "delete(name: string, confirm: true)",
        ],
    },
    BuiltinToolSpec {
        name: "skill_manager",
        summary: "管理工作区自定义 Skill 与提示词",
        actions: &[
            "list()",
            "add(name: string, content: string, kind?|description?|model_id?)",
            "batch_import(input_text: string, model_id?)",
            "toggle(id|name: string, enabled: boolean)",
            "delete(id|name: string, confirm: true)",
        ],
    },
];

/// 从稳定的内置契约渲染首轮工具目录。
///
/// 目录不再解析 Markdown，因此手册格式和资源目录可用性不会改变可执行工具集。
pub fn build_local_tools_menu() -> String {
    let mut local_tools_menu = String::from("\n## 内置本地工具 (Built-in Tools)\n");
    for tool in BUILTIN_TOOL_SPECS {
        local_tools_menu.push_str(&format!("- {}: {}\n", tool.name, tool.summary));
        for action in tool.actions {
            local_tools_menu.push_str(&format!("  - {}\n", action));
        }
    }
    local_tools_menu
}

#[cfg(test)]
mod tests {
    use super::{build_local_tools_menu, BUILTIN_TOOL_SPECS};

    #[test]
    fn builtin_catalog_exposes_first_turn_required_fields_and_confirmation() {
        let menu = build_local_tools_menu();
        assert!(menu.contains("add(name: string, content: string"));
        assert!(menu.contains("delete(id|name: string, confirm: true)"));
        assert!(menu.contains("upsert_records(resource_id: string, records: array)"));
        assert!(menu.contains("get_step_output(output_id: string"));
    }

    #[test]
    fn every_builtin_contract_has_a_summary_and_action() {
        assert!(BUILTIN_TOOL_SPECS.iter().all(|tool| {
            !tool.name.is_empty() && !tool.summary.is_empty() && !tool.actions.is_empty()
        }));
    }
}
