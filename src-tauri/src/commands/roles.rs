use crate::agent::tools::role_manager;
use crate::db::DbState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::State;

/// 角色卡列表（含禁用项，供管理面板展示）
#[tauri::command]
pub async fn list_role_cards(state: State<'_, DbState>) -> Result<Vec<role_manager::RoleCard>, String> {
    role_manager::list_cards(state.inner(), true)
}

/// 新建或整卡更新角色卡
#[tauri::command]
pub async fn upsert_role_card(state: State<'_, DbState>, card: Value) -> Result<role_manager::RoleCard, String> {
    role_manager::upsert_card(state.inner(), &card)
}

/// 启用/禁用切换
#[tauri::command]
pub async fn toggle_role_card(state: State<'_, DbState>, name: String) -> Result<String, String> {
    role_manager::toggle_card(state.inner(), &Value::String(name))
}

/// 删除角色卡
#[tauri::command]
pub async fn delete_role_card(state: State<'_, DbState>, name: String) -> Result<String, String> {
    role_manager::delete_card(state.inner(), &Value::String(name))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedRoleResponse {
    pub name: String,
    pub label: String,
    pub allowed_kinds: Vec<String>,
    pub hard_scope: String,
    pub scope_hints: String,
    pub default_test_command: String,
    pub persona_prompt: String,
    pub focus_domains: Vec<String>,
    #[serde(default)]
    pub input_fields: Vec<GeneratedSchemaField>,
    #[serde(default)]
    pub schema_fields: Vec<GeneratedSchemaField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedSchemaField {
    pub name: String,
    pub field_type: String,
    pub required: bool,
    pub description: String,
    #[serde(default)]
    pub allowed_values_str: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleIncubatorMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRoleIncubatorResponse {
    pub reply: String,
    pub card: GeneratedRoleResponse,
}

fn resolve_model_config(
    state: &State<'_, DbState>,
    model_id: Option<String>,
) -> Result<crate::llm::ModelConfig, String> {
    if let Some(ref mid) = model_id.filter(|s| !s.trim().is_empty()) {
        crate::llm::ModelConfig::from_db(state, mid)
    } else {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT m.id FROM models m \
                 JOIN platforms p ON p.id = m.platform_id \
                 WHERE m.enabled = 1 \
                 ORDER BY (CASE WHEN lower(m.name) LIKE '%flash%' OR lower(m.name) LIKE '%fast%' THEN 1 ELSE 2 END), m.created_at DESC"
            )
            .map_err(|e| e.to_string())?;
        let model_ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .ok()
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        drop(stmt);
        drop(conn);

        let mut found = None;
        for id in model_ids {
            if let Ok(cfg) = crate::llm::ModelConfig::from_db(state, &id) {
                found = Some(cfg);
                break;
            }
        }
        found.ok_or_else(|| "未找到可用的大模型配置，请先在模型管理中配置 API Key".to_string())
    }
}

fn extract_content_from_llm_response(json_resp: &Value) -> Result<String, String> {
    let message = json_resp
        .get("choices")
        .and_then(|arr| arr.get(0))
        .and_then(|c| c.get("message"));

    let content_opt = message.and_then(|m| {
        if let Some(s) = m.get("content").and_then(|v| v.as_str()) {
            Some(s.to_string())
        } else if let Some(arr) = m.get("content").and_then(|v| v.as_array()) {
            let parts: Vec<String> = arr
                .iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()))
                .collect();
            if !parts.is_empty() {
                Some(parts.join("\n"))
            } else {
                None
            }
        } else {
            None
        }
    });

    if let Some(c) = content_opt.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Ok(c)
    } else if let Some(reasoning) = message
        .and_then(|m| m.get("reasoning_content"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        Ok(reasoning.to_string())
    } else {
        Err(format!(
            "大模型响应中未找到有效正文: {}",
            serde_json::to_string(json_resp).unwrap_or_default()
        ))
    }
}

fn parse_generated_role_from_value(parsed: &Value) -> GeneratedRoleResponse {
    let name = parsed.get("name")
        .and_then(Value::as_str)
        .unwrap_or("custom_role")
        .trim()
        .to_lowercase()
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '_', "_");

    let label = parsed.get("label")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| format!("🤖 {}", name));

    let allowed_kinds: Vec<String> = if let Some(arr) = parsed.get("allowed_kinds").and_then(Value::as_array) {
        arr.iter().filter_map(|v| v.as_str().map(|s| s.trim().to_lowercase())).filter(|s| !s.is_empty()).collect()
    } else if let Some(s) = parsed.get("allowed_kinds").and_then(Value::as_str) {
        s.split(',').map(|k| k.trim().to_lowercase()).filter(|s| !s.is_empty()).collect()
    } else {
        vec!["reviewer".to_string()]
    };

    let hard_scope = parsed.get("hard_scope").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let scope_hints = parsed.get("scope_hints").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let default_test_command = parsed.get("default_test_command").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let persona_prompt = parsed.get("persona_prompt").and_then(Value::as_str).unwrap_or("").trim().to_string();

    let focus_domains: Vec<String> = parsed.get("focus_domains")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_else(|| vec!["git_diff".to_string(), "changed_files".to_string()]);

    let parse_schema_item = |item: &Value| -> Option<GeneratedSchemaField> {
        let n = item.get("name").and_then(Value::as_str)?.trim().to_lowercase().replace(|c: char| !c.is_ascii_alphanumeric() && c != '_', "_");
        if n.is_empty() { return None; }
        let ftype = match item.get("field_type").and_then(Value::as_str).unwrap_or("string") {
            "number" => "number",
            "boolean" => "boolean",
            "string[]" => "string[]",
            _ => "string",
        }.to_string();
        let required = item.get("required").and_then(Value::as_bool).unwrap_or(true);
        let description = item.get("description").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let allowed_values_str = if let Some(arr) = item.get("allowed_values").and_then(Value::as_array) {
            arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
        } else {
            item.get("allowed_values_str").and_then(Value::as_str).unwrap_or("").trim().to_string()
        };
        Some(GeneratedSchemaField {
            name: n,
            field_type: ftype,
            required,
            description,
            allowed_values_str,
        })
    };

    let input_fields: Vec<GeneratedSchemaField> = parsed.get("input_fields")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(parse_schema_item).collect())
        .unwrap_or_default();

    let schema_fields: Vec<GeneratedSchemaField> = parsed.get("schema_fields")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(parse_schema_item).collect())
        .unwrap_or_default();

    GeneratedRoleResponse {
        name,
        label,
        allowed_kinds,
        hard_scope,
        scope_hints,
        default_test_command,
        persona_prompt,
        focus_domains,
        input_fields,
        schema_fields,
    }
}

/// 多轮对话交互式孵化与微调角色卡
#[tauri::command]
pub async fn chat_iterate_role_card_ai(
    state: State<'_, DbState>,
    prompt: String,
    history: Option<Vec<RoleIncubatorMessage>>,
    current_card: Option<Value>,
    role_kind_hint: Option<String>,
    model_id: Option<String>,
) -> Result<ChatRoleIncubatorResponse, String> {
    let prompt_trimmed = prompt.trim();
    if prompt_trimmed.is_empty() {
        return Err("需求描述或微调指令不能为空".to_string());
    }

    let model_cfg = resolve_model_config(&state, model_id)?;
    let client = model_cfg.build_client()?;
    let url = model_cfg.endpoint_url();
    let headers = model_cfg.auth_headers();

    let kind_hint_str = role_kind_hint
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "未指定特定类型，请根据需求智能判断最适合的 child kind (writer, reviewer 或 worker)".to_string());

    let sys_prompt = r#"你是一位顶级 AI Agent 架构专家与对话式角色设计顾问。用户正在与你共同【多轮迭代设计与微调一个专精的角色卡 (Role Card)】。

【交互机制】：
1. 用户会给出初始需求，或在后续多轮对话中提出任意微调要求（例如：“把测试命令改成 npm test”、“硬围栏再加上 package.json”、“人设里再强调一下暗色模式对比度”、“加一个叫 bug_count 的交付契约”）。
2. 若传入了【当前已有角色卡草案 (current_card)】，你必须在此草案基础上进行【增量修改与强化】，保留既有合理的配置，只根据用户最新的指令修改、扩充或调整相关字段。
3. 若是第一轮对话，则根据用户需求全新创建全套配置。
4. 你必须返回两个部分：
   - reply: 简短、亲切、专业的一两句话对话回复（说明你做了哪些具体修改，或者增加了什么能力，并提示用户还可以怎样继续微调）。
   - card: 完整的最新角色卡配置对象。

【角色卡各字段规范】：
1. name: 唯一标识 slug，全小写字母加下划线（如 vue_tailwind_reviewer, rust_concurrency_guard）。
2. label: 中文展示名称，以最符合身份的 1 个 Emoji 开头（如 "🎨 Vue 样式审查专家"）。
3. allowed_kinds: 挂载类型数组，可选 ["writer"](改码)、["reviewer"](审查)、["worker"](调研探针)。
4. hard_scope: 强制物理围栏路径前缀（逗号分隔，如 "src/, package.json"；不限制填 ""）。
5. scope_hints: 调度唤醒提示词（1~2 句话，说明主 Agent 在何种业务场景下派发此专家）。
6. default_test_command: 基线验证命令（如 "npm test", "cargo test", "npm run build" 或 ""）。
7. persona_prompt: 详实专业的人设提示词（300~500字）。涵盖：专家身份、核心检查/执行标准（至少3项明确指标）、绝不允许犯的红线禁忌、打分评价维度。
8. focus_domains: 系统自动装配材料白名单（数组），可选 "git_diff", "changed_files", "last_verify_log"。
9. input_fields: 专家特化自定义入参数组（name, field_type, required, description, allowed_values_str）。
10. schema_fields: 专家特化交付契约字段数组（name, field_type, required, description, allowed_values_str；无需重复 findings 和 confidence）。

【必须输出的合法 JSON 格式】：
{
  "reply": "已为您将测试命令修改为 npm test，并将 package.json 纳入了硬围栏范围。...",
  "card": {
    "name": "...",
    "label": "...",
    "allowed_kinds": ["..."],
    "hard_scope": "...",
    "scope_hints": "...",
    "default_test_command": "...",
    "persona_prompt": "...",
    "focus_domains": ["..."],
    "input_fields": [ ... ],
    "schema_fields": [ ... ]
  }
}
严格只输出合法 JSON，禁止 markdown 代码块或额外文本。"#;

    let mut messages: Vec<Value> = vec![
        json!({
            "role": "system",
            "content": sys_prompt
        }),
    ];

    if let Some(card_val) = current_card.filter(|v| !v.is_null() && v.is_object()) {
        messages.push(json!({
            "role": "system",
            "content": format!("【当前已有角色卡草案 (基于此增量微调)】:\n{}", serde_json::to_string_pretty(&card_val).unwrap_or_default())
        }));
    }

    if let Some(hist) = history {
        for msg in hist {
            let role = if msg.role == "user" { "user" } else { "assistant" };
            messages.push(json!({
                "role": role,
                "content": msg.content
            }));
        }
    }

    let user_msg = if messages.len() <= 2 {
        format!("【用户需求】: {}\n【倾向的角色类型】: {}", prompt_trimmed, kind_hint_str)
    } else {
        prompt_trimmed.to_string()
    };

    messages.push(json!({
        "role": "user",
        "content": user_msg
    }));

    let mut body = json!({
        "model": model_cfg.model_name,
        "messages": messages,
        "max_tokens": 2048,
        "temperature": 0.3,
        "stream": false
    });

    model_cfg.apply_provider_capabilities(&mut body, "off");

    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        client.post(&url).headers(headers).json(&body).send(),
    )
    .await
    .map_err(|_| "LLM 生成角色配置超时 (90s)".to_string())?
    .map_err(|e| format!("LLM 生成角色配置网络请求失败: {}", e))?;

    let json_resp: Value = resp.json().await.map_err(|e| format!("解析 LLM 返回格式失败: {}", e))?;
    let raw_content = extract_content_from_llm_response(&json_resp)?;

    let json_text = crate::agent::utils::extract_json_from_text(&raw_content)
        .or_else(|| crate::agent::utils::repair_json(&raw_content))
        .unwrap_or(raw_content);

    let parsed: Value = serde_json::from_str(&json_text)
        .map_err(|e| format!("解析角色 JSON 配置失败: {}, 原始文本: {}", e, json_text))?;

    let (reply, card) = if let Some(card_sub) = parsed.get("card").filter(|v| v.is_object()) {
        let r = parsed.get("reply").and_then(Value::as_str).unwrap_or("已为您完成角色配置更新！").to_string();
        let c = parse_generated_role_from_value(card_sub);
        (r, c)
    } else {
        let c = parse_generated_role_from_value(&parsed);
        let r = format!("已为您生成/更新【{}】角色卡配置！", c.label);
        (r, c)
    };

    Ok(ChatRoleIncubatorResponse { reply, card })
}

/// 兼容老接口：单次直接生成
#[tauri::command]
pub async fn generate_role_card_ai(
    state: State<'_, DbState>,
    prompt: String,
    role_kind_hint: Option<String>,
    model_id: Option<String>,
) -> Result<GeneratedRoleResponse, String> {
    let res = chat_iterate_role_card_ai(state, prompt, None, None, role_kind_hint, model_id).await?;
    Ok(res.card)
}

/// 使用 AI 润色增强角色人设提示词 (Persona Prompt)
#[tauri::command]
pub async fn polish_persona_prompt_ai(
    state: State<'_, DbState>,
    current_prompt: String,
    role_name: Option<String>,
    role_kind: Option<String>,
    model_id: Option<String>,
) -> Result<String, String> {
    let trimmed = current_prompt.trim();
    if trimmed.is_empty() {
        return Err("提示词内容不能为空".to_string());
    }

    let model_cfg = resolve_model_config(&state, model_id)?;
    let client = model_cfg.build_client()?;
    let url = model_cfg.endpoint_url();
    let headers = model_cfg.auth_headers();

    let sys_prompt = r#"你是一位顶级 AI Agent 提示词工程专家。
请对用户提供的【专家角色人设提示词 (Persona Prompt)】进行深度专业润色与增强。
要求：
1. 强化该专家的角色身份认同、专业壁垒与思维链（CoT）；
2. 细化审查/执行标准，列出清晰的 3~4 项核心关注指标与红线禁忌（绝不允许出现的质量或安全隐患）；
3. 引导结构化审查逻辑与建设性改进行动；
4. 语言紧凑有力、专业度高、信息密度极高，字数控制在 300~500 字；
5. 禁止废话与客套话，不要包含 Markdown 标题或代码块，直接输出纯文本提示词内容。"#;

    let user_msg = format!(
        "【角色标识/名称】: {}\n【角色挂载类型】: {}\n\n【原始人设提示词】:\n{}",
        role_name.unwrap_or_else(|| "未命名专家".to_string()),
        role_kind.unwrap_or_else(|| "通用专家".to_string()),
        trimmed
    );

    let messages = vec![
        json!({
            "role": "system",
            "content": sys_prompt
        }),
        json!({
            "role": "user",
            "content": user_msg
        }),
    ];

    let mut body = json!({
        "model": model_cfg.model_name,
        "messages": messages,
        "max_tokens": 1500,
        "temperature": 0.4,
        "stream": false
    });

    model_cfg.apply_provider_capabilities(&mut body, "off");

    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        client.post(&url).headers(headers).json(&body).send(),
    )
    .await
    .map_err(|_| "LLM 润色提示词超时 (90s)".to_string())?
    .map_err(|e| format!("LLM 润色提示词网络请求失败: {}", e))?;

    let json_resp: Value = resp.json().await.map_err(|e| format!("解析 LLM 返回格式失败: {}", e))?;
    let polished = extract_content_from_llm_response(&json_resp)?;
    Ok(polished)
}
