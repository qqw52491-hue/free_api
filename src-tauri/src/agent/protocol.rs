use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// 协议与多 Agent 基线保留字黑名单：禁止扩展字段与基线强类型字段碰撞。
/// 该集合由单元测试 `test_baseline_reserved_fields_completeness` 守卫，
/// 若基线 struct 增删字段，测试将立即报错，防止黑名单隐式腐烂。
pub const BASELINE_RESERVED_FIELDS: &[&str] = &[
    // 通用协议 & 执行元数据
    "child_run_id",
    "task_id",
    "attempt_id",
    "role_id",
    "contract_id",
    "parent_run_id",
    "parent_session_id",
    "model_id",
    "model_tier",
    "reasoning_effort",
    "persona",
    "timeout_secs",
    "input_payload",
    "custom_schema",
    "focus_domains",
    "specialized_context",
    "status",
    "duration_ms",
    "completion_tokens",
    "prompt_tokens",
    "error_kind",
    "raw_result_json",
    "raw_text",
    "payload",
    "kind",
    "steps_used",
    "extra",
    // 审查员基线
    "findings",
    "confidence",
    "evidence_refs",
    "missing_evidence",
    "routing",
    "withdrawn_findings",
    "objection_verdicts",
    "scorecard",
    // 工兵基线
    "changed_files",
    "test_command",
    "test_status",
    "test_output_tail",
    "workspace_copy",
    "patch_ref",
    // 侦察基线
    "action_log",
];

/// 判定给定字段名是否属于基线保留字
pub fn is_reserved_field_name(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    BASELINE_RESERVED_FIELDS.contains(&lower.as_str())
}

/// 角色关注领域（Focus Domain）：声明角色在调度时期望系统底层自动装配的专项材料
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusDomain {
    ChangedFiles,
    GitDiff,
    LastVerifyLog,
}

impl FocusDomain {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ChangedFiles => "changed_files",
            Self::GitDiff => "git_diff",
            Self::LastVerifyLog => "last_verify_log",
        }
    }

    pub fn display_label(&self) -> &'static str {
        match self {
            Self::ChangedFiles => "变更文件列表 (changed_files)",
            Self::GitDiff => "关键代码差异 (git_diff)",
            Self::LastVerifyLog => "最近验证/构建日志 (last_verify_log)",
        }
    }

    pub fn from_domain_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "changed_files" | "files" => Some(Self::ChangedFiles),
            "git_diff" | "diff" => Some(Self::GitDiff),
            "last_verify_log" | "verify_log" | "test_log" => Some(Self::LastVerifyLog),
            _ => None,
        }
    }

    pub fn all() -> &'static [FocusDomain] {
        &[
            Self::ChangedFiles,
            Self::GitDiff,
            Self::LastVerifyLog,
        ]
    }
}

impl std::fmt::Display for FocusDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 校验自定义扩展字段名称的合法性（防碰撞 + 命名语法约束）
pub fn validate_custom_field_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("自定义扩展字段名不能为空".to_string());
    }
    if is_reserved_field_name(trimmed) {
        return Err(format!(
            "字段名 '{}' 命中基线协议保留字黑名单，禁止用作扩展字段！",
            trimmed
        ));
    }
    let is_valid_ident = trimmed.starts_with(|c: char| c.is_ascii_lowercase())
        && trimmed.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && trimmed.len() >= 2
        && trimmed.len() <= 64;

    if !is_valid_ident {
        return Err(format!(
            "字段名 '{}' 不符合命名规范：必须是全小写 snake_case（以小写字母开头，仅含字母/数字/下划线，长度 2-64）",
            trimmed
        ));
    }
    Ok(())
}

/// 扩展字段支持的基础数据类型（v1 收紧为标量与字符串数组，杜绝嵌套对象引发的前端排版与解析灾难）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContractFieldType {
    #[serde(rename = "string")]
    String,
    #[serde(rename = "string[]")]
    StringArray,
    #[serde(rename = "number")]
    Number,
    #[serde(rename = "boolean")]
    Boolean,
}

impl ContractFieldType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::String => "string",
            Self::StringArray => "string[]",
            Self::Number => "number",
            Self::Boolean => "boolean",
        }
    }

    pub fn from_type_str(s: &str) -> Result<Self, String> {
        match s.trim().to_lowercase().as_str() {
            "string" | "str" | "text" => Ok(Self::String),
            "string[]" | "string_array" | "array<string>" | "list[str]" => Ok(Self::StringArray),
            "number" | "int" | "float" | "integer" => Ok(Self::Number),
            "boolean" | "bool" => Ok(Self::Boolean),
            other => Err(format!(
                "不支持的字段类型: '{}'。v1 扩展字段仅支持标量与字符串数组: 'string' | 'string[]' | 'number' | 'boolean'",
                other
            )),
        }
    }
}

impl std::fmt::Display for ContractFieldType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 单个扩展字段契约定义（声明式驱动模型 Prompt 与运行时校验）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractFieldDef {
    pub name: String,
    pub field_type: ContractFieldType,
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_values: Option<Vec<String>>,
}

impl ContractFieldDef {
    pub fn new(
        name: impl Into<String>,
        field_type: ContractFieldType,
        description: impl Into<String>,
        required: bool,
    ) -> Result<Self, String> {
        let name_str = name.into();
        validate_custom_field_name(&name_str)?;
        Ok(Self {
            name: name_str,
            field_type,
            description: description.into(),
            required,
            allowed_values: None,
        })
    }

    /// 静态常量式安全构造器（若名字非法直接 panic，仅供内部已知安全常量调用）
    pub fn must_new(
        name: impl Into<String>,
        field_type: ContractFieldType,
        description: impl Into<String>,
        required: bool,
    ) -> Self {
        Self::new(name, field_type, description, required).expect("静态字段名必须合法")
    }

    pub fn with_allowed_values(mut self, allowed: Vec<String>) -> Self {
        self.allowed_values = Some(allowed);
        self
    }

    pub fn validate_value(&self, val: Option<&Value>) -> Result<(), String> {
        let val = match val {
            Some(v) if !v.is_null() => v,
            _ => {
                if self.required {
                    return Err(format!("缺少必填字段: '{}' ({})", self.name, self.description));
                } else {
                    return Ok(());
                }
            }
        };

        match self.field_type {
            ContractFieldType::String => {
                let s = val.as_str().ok_or_else(|| {
                    format!("字段 '{}' 类型不匹配: 期望 string，实际为 {}", self.name, val)
                })?;
                if let Some(ref allowed) = self.allowed_values {
                    if !allowed.iter().any(|item| item == s) {
                        return Err(format!(
                            "字段 '{}' 取值非法: '{}' 不在可选值域 {:?} 内",
                            self.name, s, allowed
                        ));
                    }
                }
            }
            ContractFieldType::StringArray => {
                let arr = val.as_array().ok_or_else(|| {
                    format!("字段 '{}' 类型不匹配: 期望 string[]，实际为 {}", self.name, val)
                })?;
                for (idx, elem) in arr.iter().enumerate() {
                    if !elem.is_string() {
                        return Err(format!(
                            "字段 '{}' 的第 {} 项类型不匹配: 期望 string，实际为 {}",
                            self.name, idx, elem
                        ));
                    }
                }
            }
            ContractFieldType::Number => {
                if !val.is_number() {
                    return Err(format!(
                        "字段 '{}' 类型不匹配: 期望 number，实际为 {}",
                        self.name, val
                    ));
                }
            }
            ContractFieldType::Boolean => {
                if !val.is_boolean() {
                    return Err(format!(
                        "字段 '{}' 类型不匹配: 期望 boolean，实际为 {}",
                        self.name, val
                    ));
                }
            }
        }
        Ok(())
    }
}

fn default_true() -> bool {
    true
}

/// 扩展契约模式（Contract Schema）：定义一组特化扩展字段及闭集校验规则
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractSchema {
    #[serde(default)]
    pub fields: Vec<ContractFieldDef>,
    /// 是否开启闭集白名单校验：若开启，产物中若包含未在此声明的未知字段，直接拒收并报警
    #[serde(default = "default_true")]
    pub strict_closed_set: bool,
}

impl ContractSchema {
    pub fn new(fields: Vec<ContractFieldDef>) -> Self {
        Self {
            fields,
            strict_closed_set: true,
        }
    }

    pub fn with_strict_closed_set(mut self, strict: bool) -> Self {
        self.strict_closed_set = strict;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn add_field(&mut self, field: ContractFieldDef) -> Result<(), String> {
        if self.fields.iter().any(|f| f.name == field.name) {
            return Err(format!("重复添加字段 '{}'", field.name));
        }
        self.fields.push(field);
        Ok(())
    }

    pub fn field_names(&self) -> BTreeSet<&str> {
        self.fields.iter().map(|f| f.name.as_str()).collect()
    }

    /// 渲染格式化 Prompt 指引，直接注入到大模型角色提示词中
    pub fn render_prompt_instructions(&self) -> String {
        if self.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        lines.push("【输出扩展契约规范】".to_string());
        lines.push("你在输出最终 JSON 时，除标准基线字段外，还必须严格遵守以下扩展字段约定（严禁臆造或拼错字段名，多余未知字段将被系统拒收）：".to_string());
        for f in &self.fields {
            let req_str = if f.required { "必填" } else { "选填" };
            let enum_str = if let Some(ref allowed) = f.allowed_values {
                format!("，可选值: [{}]", allowed.join(", "))
            } else {
                String::new()
            };
            lines.push(format!(
                "- `{}` ({}, {}{}): {}",
                f.name,
                f.field_type.as_str(),
                req_str,
                enum_str,
                f.description
            ));
        }
        lines.push("请确保 JSON 顶级键中包含上述必填字段，且键名与数据类型完全吻合。".to_string());
        lines.join("\n")
    }

    /// 渲染自定义入参的 Prompt 指引，指导主 Agent 在派发该角色时应该/可以传入哪些参数
    pub fn render_input_prompt_instructions(&self, role_name: &str) -> String {
        if self.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        lines.push(format!("【角色「{}」自定义入参规范 (custom_inputs)】", role_name));
        lines.push("派发该角色时，请在调用参数的 `custom_inputs` 对象中传入以下键值对（若无特殊需要，必填项必须提供）：".to_string());
        for f in &self.fields {
            let req_str = if f.required { "必填" } else { "选填" };
            let enum_str = if let Some(ref allowed) = f.allowed_values {
                format!("，可选值: [{}]", allowed.join(", "))
            } else {
                String::new()
            };
            lines.push(format!(
                "- `{}` ({}, {}{}): {}",
                f.name,
                f.field_type.as_str(),
                req_str,
                enum_str,
                f.description
            ));
        }
        lines.join("\n")
    }

    /// 校验产物负载的形态（类型、必填性、闭集白名单）
    pub fn validate_shape(&self, payload: &Value) -> Result<(), String> {
        if self.is_empty() {
            return Ok(());
        }

        let map = payload.as_object().ok_or_else(|| {
            format!("交付产物不是合法的 JSON 对象，无法进行契约校验: {}", payload)
        })?;

        // 1. 闭集白名单校验：除基线保留字段外，所有的额外键都必须在 self.fields 声明列表中
        if self.strict_closed_set {
            let declared_names = self.field_names();
            for (key, _) in map {
                if is_reserved_field_name(key) {
                    continue;
                }
                if !declared_names.contains(key.as_str()) {
                    return Err(format!(
                        "闭集校验失败：检测到未在契约中声明的未知扩展字段 '{}'（可用扩展字段: {:?}）。请纠正拼写或移除该字段。",
                        key, declared_names
                    ));
                }
            }
        }

        // 2. 逐字段校验定义
        for field in &self.fields {
            let val = map.get(&field.name);
            field.validate_value(val)?;
        }

        Ok(())
    }

    /// 清洗 payload：仅保留 custom_schema 中声明过的有效扩展字段，剔除未声明的任意模型幻觉键
    pub fn sanitize_extra(&self, payload: &Value) -> std::collections::HashMap<String, Value> {
        let mut clean = std::collections::HashMap::new();
        if let Some(map) = payload.as_object() {
            let declared_names = self.field_names();
            for (key, val) in map {
                if declared_names.contains(key.as_str()) {
                    clean.insert(key.clone(), val.clone());
                }
            }
        }
        clean
    }
}

/// 默认子任务派发超时（秒）
pub const DEFAULT_DISPATCH_TIMEOUT_SECS: u64 = 60;

fn default_attempt_id() -> u32 {
    1
}

fn default_tier() -> String {
    "flash".to_string()
}

fn default_effort() -> String {
    "high".to_string()
}

fn default_timeout_secs() -> u64 {
    DEFAULT_DISPATCH_TIMEOUT_SECS
}

/// 任务与执行尝试联合键：用于宿主幂等收件与排重
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskAttemptKey {
    pub task_id: String,
    pub attempt_id: u32,
}

impl TaskAttemptKey {
    pub fn new(task_id: impl Into<String>, attempt_id: u32) -> Self {
        Self {
            task_id: task_id.into(),
            attempt_id,
        }
    }
}

/// 通用派发信封（Dispatch Envelope）：宿主向子沙箱派发任务的标准数据结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DispatchEnvelope {
    /// 逻辑任务标识（如 "plan_review", "fix_bug"）
    pub task_id: String,
    /// 执行尝试计数（从 1 开始，重试递增，解决迟到与幂等识别）
    #[serde(default = "default_attempt_id")]
    pub attempt_id: u32,
    /// 目标角色名（如 "child_reviewer", "child_writer", "child_worker"）
    pub role_id: String,
    /// 绑定的交付契约及版本（如 "reviewer.v1", "writer.v1", "worker.v1"）
    pub contract_id: String,
    /// 父级运行 ID
    pub parent_run_id: String,
    /// 父级会话 ID
    pub parent_session_id: String,
    /// 选用模型
    pub model_id: String,
    /// 模型 Tier（如 "flash", "pro"）
    #[serde(default = "default_tier")]
    pub model_tier: String,
    /// 推理强度（如 "high", "medium", "low"）
    #[serde(default = "default_effort")]
    pub reasoning_effort: String,
    /// 角色提示词设定
    #[serde(default)]
    pub persona: String,
    /// 超时时间（秒）
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// 领域特定入参载荷（由具体角色解析，宿主通用层不干涉内部字段）
    #[serde(default)]
    pub input_payload: Value,
    /// 扩展字段契约模式（声明此任务必须/可选返回的自定义字段）
    #[serde(default)]
    pub custom_schema: ContractSchema,
    /// 角色声明的关注域清单
    #[serde(default)]
    pub focus_domains: Vec<FocusDomain>,
    /// 系统已自动采集装配的专属材料
    #[serde(default)]
    pub specialized_context: Option<String>,
}

impl DispatchEnvelope {
    pub fn new(
        task_id: impl Into<String>,
        role_id: impl Into<String>,
        contract_id: impl Into<String>,
        parent_run_id: impl Into<String>,
        parent_session_id: impl Into<String>,
        model_id: impl Into<String>,
        input_payload: Value,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            attempt_id: default_attempt_id(),
            role_id: role_id.into(),
            contract_id: contract_id.into(),
            parent_run_id: parent_run_id.into(),
            parent_session_id: parent_session_id.into(),
            model_id: model_id.into(),
            model_tier: default_tier(),
            reasoning_effort: default_effort(),
            persona: String::new(),
            timeout_secs: default_timeout_secs(),
            input_payload,
            custom_schema: ContractSchema::default(),
            focus_domains: Vec::new(),
            specialized_context: None,
        }
    }

    pub fn reviewer(
        parent_run_id: impl Into<String>,
        parent_session_id: impl Into<String>,
        model_id: impl Into<String>,
        input_payload: Value,
    ) -> Self {
        Self::new(
            "review",
            "child_reviewer",
            "reviewer.v1",
            parent_run_id,
            parent_session_id,
            model_id,
            input_payload,
        )
    }

    pub fn writer(
        parent_run_id: impl Into<String>,
        parent_session_id: impl Into<String>,
        model_id: impl Into<String>,
        input_payload: Value,
    ) -> Self {
        let mut env = Self::new(
            "writer",
            "child_writer",
            "writer.v1",
            parent_run_id,
            parent_session_id,
            model_id,
            input_payload,
        );
        env.timeout_secs = 180;
        env
    }

    pub fn worker(
        parent_run_id: impl Into<String>,
        parent_session_id: impl Into<String>,
        model_id: impl Into<String>,
        input_payload: Value,
    ) -> Self {
        Self::new(
            "worker",
            "child_worker",
            "worker.v1",
            parent_run_id,
            parent_session_id,
            model_id,
            input_payload,
        )
    }

    pub fn with_task_id(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = task_id.into();
        self
    }

    pub fn with_attempt_id(mut self, attempt_id: u32) -> Self {
        self.attempt_id = attempt_id;
        self
    }

    pub fn with_tier(mut self, tier: impl Into<String>) -> Self {
        self.model_tier = tier.into();
        self
    }

    pub fn with_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = effort.into();
        self
    }

    pub fn with_persona(mut self, persona: impl Into<String>) -> Self {
        self.persona = persona.into();
        self
    }

    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    pub fn with_schema(mut self, schema: ContractSchema) -> Self {
        self.custom_schema = schema;
        self
    }

    pub fn with_custom_schema(self, schema: ContractSchema) -> Self {
        self.with_schema(schema)
    }

    pub fn with_custom_field(mut self, field: ContractFieldDef) -> Result<Self, String> {
        self.custom_schema.add_field(field)?;
        Ok(self)
    }

    pub fn with_focus_domains(mut self, domains: Vec<FocusDomain>) -> Self {
        self.focus_domains = domains;
        self
    }

    pub fn with_specialized_context(mut self, ctx: impl Into<String>) -> Self {
        self.specialized_context = Some(ctx.into());
        self
    }

    pub fn attempt_key(&self) -> TaskAttemptKey {
        TaskAttemptKey::new(&self.task_id, self.attempt_id)
    }

    /// 使用信封绑定的扩展契约对交付回执进行校验与守卫。
    /// 若回执执行成功但违反了扩展字段约束（如缺少必填字段、类型不符、包含未声明未知字段），
    /// 将就地将回执标记为 invalid_output，并记录 contract_violation 错误信息。
    pub fn validate_delivery_receipt(&self, receipt: &mut DeliveryReceipt) -> Result<(), String> {
        if self.custom_schema.is_empty() {
            return Ok(());
        }
        if !receipt.is_success() {
            return Ok(());
        }
        if let Err(err) = receipt.validate_against_schema(&self.custom_schema) {
            receipt.status = "invalid_output".to_string();
            receipt.error_kind =
                Some(crate::agent::types::ChildErrorKind::ContractViolation(err.clone()).db_value());
            return Err(err);
        }
        Ok(())
    }

    /// 统一异步派发执行：根据 role_id 自动委派给对应沙箱执行体。
    /// 若绑定了扩展契约模式，将自动生成契约约束格式规范追加至角色提示词中。
    pub async fn dispatch(
        &self,
        app: Option<tauri::AppHandle>,
        db: &crate::db::DbState,
        workspace_root: &std::path::Path,
        interrupt_rx: tokio::sync::watch::Receiver<u64>,
    ) -> Result<String, String> {
        let final_persona = if !self.custom_schema.is_empty() {
            let instructions = self.custom_schema.render_prompt_instructions();
            if self.persona.is_empty() {
                instructions
            } else {
                format!("{}\n\n{}", self.persona, instructions)
            }
        } else {
            self.persona.clone()
        };

        match self.role_id.as_str() {
            "child_writer" | "writer" => {
                crate::agent::child_writer::spawn_child_writer(
                    app,
                    db,
                    &self.parent_run_id,
                    &self.parent_session_id,
                    workspace_root,
                    &self.model_id,
                    &self.reasoning_effort,
                    &self.input_payload,
                    final_persona,
                    interrupt_rx,
                )
                .await
            }
            "child_worker" | "worker" => {
                crate::agent::child_worker::spawn_child_worker(
                    app,
                    db,
                    &self.parent_run_id,
                    &self.parent_session_id,
                    workspace_root,
                    &self.model_id,
                    &self.reasoning_effort,
                    &self.input_payload,
                    final_persona,
                    interrupt_rx,
                )
                .await
            }
            _ => {
                let schema_opt = if self.custom_schema.is_empty() {
                    None
                } else {
                    Some(self.custom_schema.clone())
                };
                crate::agent::child_reviewer::spawn_child_reviewer(
                    app,
                    db,
                    &self.parent_run_id,
                    &self.parent_session_id,
                    workspace_root,
                    &self.model_id,
                    &self.model_tier,
                    &self.reasoning_effort,
                    &self.input_payload,
                    &final_persona,
                    schema_opt,
                    interrupt_rx,
                )
                .await
            }
        }
    }
}

/// 通用交付回执（Delivery Receipt）：子任务终态出库后交付给宿主记录与消费的凭据
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeliveryReceipt {
    /// 对应的子任务物理运行 ID
    pub child_run_id: String,
    /// 逻辑任务 ID
    pub task_id: String,
    /// 执行尝试号
    pub attempt_id: u32,
    /// 任务类型（"review", "writer", "worker"）
    pub kind: String,
    /// 宿主观测的执行状态（"succeeded", "timed_out", "failed", "cancelled", "invalid_output" 等）
    pub status: String,
    /// 真实执行耗时（毫秒）
    pub duration_ms: u64,
    /// 输出 token 数
    pub completion_tokens: i64,
    /// 输入 token 数
    pub prompt_tokens: i64,
    /// 异常分类（若有）
    pub error_kind: Option<String>,
    /// 结构化领域载荷 JSON（成功时有效）
    pub payload: Option<Value>,
    /// 原始返回文本/JSON（带截断留存，用于半截非法 JSON 排查诊断）
    pub raw_text: Option<String>,
}

impl DeliveryReceipt {
    pub fn attempt_key(&self) -> TaskAttemptKey {
        TaskAttemptKey::new(&self.task_id, self.attempt_id)
    }

    /// 是否属于基础设施/执行异常（超时、中断、崩溃、请求层失败，非业务拒绝）
    pub fn is_infra_failure(&self) -> bool {
        self.error_kind
            .as_deref()
            .map(|raw| crate::agent::types::ChildErrorKind::parse(raw).is_infra_failure())
            .unwrap_or(false)
    }

    /// 是否正常执行成功
    pub fn is_success(&self) -> bool {
        self.status == "succeeded" && !self.is_infra_failure()
    }

    /// 强类型提取审查员交付产物
    pub fn as_reviewer_delivery(&self) -> Option<crate::agent::child_reviewer::ReviewerDeliveryData> {
        self.payload
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .or_else(|| {
                self.raw_text
                    .as_deref()
                    .and_then(|t| serde_json::from_str(t).ok())
            })
    }

    /// 强类型提取写入工兵交付产物
    pub fn as_writer_result(&self) -> Option<crate::agent::child_writer::WriterResult> {
        self.payload
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .or_else(|| {
                self.raw_text
                    .as_deref()
                    .and_then(|t| serde_json::from_str(t).ok())
            })
    }

    /// 强类型提取只读侦察任务交付产物
    pub fn as_worker_result(&self) -> Option<crate::agent::child_worker::WorkerResult> {
        self.payload
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .or_else(|| {
                self.raw_text
                    .as_deref()
                    .and_then(|t| serde_json::from_str(t).ok())
            })
    }

    /// 未被审查者撤回的 blocking 异议数
    pub fn unwithdrawn_blocking_count(&self) -> u32 {
        self.as_reviewer_delivery()
            .map(|d| d.unwithdrawn_blocking_count())
            .unwrap_or(0)
    }

    /// 阶段审查的拦截判据（未撤回且非 final_only 的 blocking 异议数）
    pub fn stage_blocking_open(&self) -> u32 {
        self.as_reviewer_delivery()
            .map(|d| d.stage_blocking_open())
            .unwrap_or(0)
    }

    /// final_only 级 blocking 的展示行（带入收尾审查的 carry-over 义务）
    pub fn stage_final_only_lines(&self) -> Vec<String> {
        self.as_reviewer_delivery()
            .map(|d| d.stage_final_only_lines())
            .unwrap_or_default()
    }

    /// 本轮异议动态三元组：(新增, 撤回, 维持)
    pub fn objection_stats(&self) -> (u32, u32, u32) {
        self.as_reviewer_delivery()
            .map(|d| d.objection_stats())
            .unwrap_or((0, 0, 0))
    }

    /// 提取审查发现项
    pub fn findings(&self) -> Vec<crate::agent::child_reviewer::ReviewFinding> {
        self.as_reviewer_delivery()
            .map(|d| d.findings)
            .unwrap_or_default()
    }

    /// 提取审查结论置信度/状态
    pub fn confidence(&self) -> String {
        self.as_reviewer_delivery()
            .map(|d| d.confidence)
            .unwrap_or_else(|| self.status.clone())
    }

    /// 提取审查建议路由
    pub fn routing(&self) -> Option<String> {
        self.as_reviewer_delivery().and_then(|d| d.routing)
    }

    /// 提取审查评分卡
    pub fn scorecard(&self) -> Option<crate::agent::child_reviewer::ReviewScorecard> {
        self.as_reviewer_delivery().and_then(|d| d.scorecard)
    }

    /// 提取缺失证据项
    pub fn missing_evidence(&self) -> Vec<String> {
        self.as_reviewer_delivery()
            .map(|d| d.missing_evidence)
            .unwrap_or_default()
    }

    /// 校验交付产物是否满足给定的扩展契约模式
    pub fn validate_against_schema(&self, schema: &ContractSchema) -> Result<(), String> {
        if schema.is_empty() {
            return Ok(());
        }
        if let Some(ref payload) = self.payload {
            schema.validate_shape(payload)
        } else if let Some(ref raw_text) = self.raw_text {
            if let Ok(val) = serde_json::from_str::<Value>(raw_text) {
                schema.validate_shape(&val)
            } else {
                Err("无法校验契约: 交付产物既无有效 payload 且 raw_text 无法解析为 JSON".to_string())
            }
        } else {
            Err("无法校验契约: 交付产物未包含任何有效 payload 或 raw_text".to_string())
        }
    }
}

impl From<crate::agent::child_reviewer::ChildReviewResult> for DeliveryReceipt {
    fn from(res: crate::agent::child_reviewer::ChildReviewResult) -> Self {
        let kind = res.kind.clone().unwrap_or_else(|| "review".to_string());
        let raw_text = res.raw_result_json.clone();
        let payload = raw_text
            .as_deref()
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
            .or_else(|| {
                if kind == "writer" || kind == "worker" {
                    serde_json::to_value(&res).ok()
                } else {
                    serde_json::to_value(res.to_reviewer_delivery()).ok()
                }
            });

        Self {
            child_run_id: res.child_run_id.clone(),
            task_id: res.child_run_id.clone(),
            attempt_id: 1,
            kind,
            status: res.status,
            duration_ms: res.duration_ms,
            completion_tokens: res.completion_tokens,
            prompt_tokens: res.prompt_tokens,
            error_kind: res.error_kind,
            payload,
            raw_text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dispatch_envelope_defaults_and_roundtrip() {
        let envelope = DispatchEnvelope {
            task_id: "task-test-01".to_string(),
            attempt_id: 2,
            role_id: "child_reviewer".to_string(),
            contract_id: "reviewer.v1".to_string(),
            parent_run_id: "parent-r1".to_string(),
            parent_session_id: "parent-s1".to_string(),
            model_id: "gemini-2.5-pro".to_string(),
            model_tier: "pro".to_string(),
            reasoning_effort: "high".to_string(),
            persona: "strict_auditor".to_string(),
            timeout_secs: 120,
            input_payload: serde_json::json!({"question": "security audit"}),
            custom_schema: ContractSchema::default(),
            focus_domains: vec![FocusDomain::GitDiff],
            specialized_context: None,
        };

        let key = envelope.attempt_key();
        assert_eq!(key.task_id, "task-test-01");
        assert_eq!(key.attempt_id, 2);

        let json_str = serde_json::to_string(&envelope).unwrap();
        let recovered: DispatchEnvelope = serde_json::from_str(&json_str).unwrap();
        assert_eq!(recovered, envelope);
    }

    #[test]
    fn test_delivery_receipt_infra_failure_detection() {
        let receipt = DeliveryReceipt {
            child_run_id: "child-1".to_string(),
            task_id: "task-1".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "timed_out".to_string(),
            duration_ms: 60000,
            completion_tokens: 0,
            prompt_tokens: 100,
            error_kind: Some("timed_out".to_string()),
            payload: None,
            raw_text: None,
        };

        assert!(receipt.is_infra_failure());
        assert!(!receipt.is_success());
    }

    #[test]
    fn test_delivery_receipt_corrupted_raw_text_preservation() {
        let receipt = DeliveryReceipt {
            child_run_id: "child-corrupt".to_string(),
            task_id: "task-corrupt".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "invalid_output".to_string(),
            duration_ms: 1500,
            completion_tokens: 20,
            prompt_tokens: 50,
            error_kind: Some("invalid_output".to_string()),
            payload: None,
            raw_text: Some(r#"{"verdict": "pass", broken..."#.to_string()),
        };

        assert!(receipt.is_infra_failure());
        assert_eq!(receipt.raw_text.as_deref(), Some(r#"{"verdict": "pass", broken..."#));
        assert!(receipt.as_reviewer_delivery().is_none());
    }

    #[test]
    fn test_delivery_receipt_delegators_and_from_conversion() {
        use crate::agent::child_reviewer::{ChildReviewResult, ObjectionVerdict, ReviewFinding, ReviewScorecard};

        let res = ChildReviewResult {
            child_run_id: "c-1".to_string(),
            status: "succeeded".to_string(),
            findings: vec![
                ReviewFinding {
                    id: "F1".to_string(),
                    severity: "blocking".to_string(),
                    finality: "regardless".to_string(),
                    location: "src/lib.rs:10".to_string(),
                    current: "old".to_string(),
                    why: "risk".to_string(),
                    suggestion: "fix".to_string(),
                    evidence_strength: "confirmed".to_string(),
                },
                ReviewFinding {
                    id: "F2".to_string(),
                    severity: "blocking".to_string(),
                    finality: "final_only".to_string(),
                    location: "src/main.rs:20".to_string(),
                    current: "todo".to_string(),
                    why: "clean up".to_string(),
                    suggestion: "clean".to_string(),
                    evidence_strength: "confirmed".to_string(),
                },
            ],
            confidence: "high".to_string(),
            evidence_refs: vec!["ref1".to_string()],
            missing_evidence: vec!["missing1".to_string()],
            duration_ms: 1200,
            completion_tokens: 300,
            prompt_tokens: 500,
            error_kind: None,
            raw_result_json: None,
            kind: Some("review".to_string()),
            routing: Some("certain".to_string()),
            withdrawn_findings: vec!["F1".to_string()],
            objection_verdicts: vec![
                ObjectionVerdict {
                    finding_id: "F1".to_string(),
                    verdict: "withdraw".to_string(),
                    reason: "resolved".to_string(),
                }
            ],
            scorecard: Some(ReviewScorecard::new(80, 70, 85, Some(90))),
        };

        let receipt: DeliveryReceipt = res.into();
        assert_eq!(receipt.child_run_id, "c-1");
        assert_eq!(receipt.kind, "review");
        assert_eq!(receipt.status, "succeeded");
        assert_eq!(receipt.duration_ms, 1200);
        assert_eq!(receipt.prompt_tokens, 500);
        assert_eq!(receipt.completion_tokens, 300);
        assert!(receipt.is_success());
        assert!(!receipt.is_infra_failure());

        // Domain delegators
        assert_eq!(receipt.confidence(), "high");
        assert_eq!(receipt.routing().as_deref(), Some("certain"));
        assert_eq!(receipt.missing_evidence(), vec!["missing1"]);
        assert_eq!(receipt.findings().len(), 2);
        assert_eq!(receipt.unwithdrawn_blocking_count(), 1); // F2 is unwithdrawn
        assert_eq!(receipt.stage_blocking_open(), 0); // F1 withdrawn, F2 is final_only
        assert_eq!(receipt.stage_final_only_lines().len(), 1);
        let (added, withdrawn, maintained) = receipt.objection_stats();
        assert_eq!(withdrawn, 1);
        assert_eq!(maintained, 0);
        assert_eq!(added, 1); // F2 is new blocking
        assert!(receipt.scorecard().unwrap().passes_gate());
    }

    #[test]
    fn test_dispatch_envelope_builders() {
        let rev = DispatchEnvelope::reviewer("parent-1", "session-1", "model-r", serde_json::json!({"q": 1}))
            .with_task_id("rev_1")
            .with_tier("pro")
            .with_effort("high")
            .with_persona("auditor")
            .with_attempt_id(2)
            .with_timeout_secs(90);

        assert_eq!(rev.task_id, "rev_1");
        assert_eq!(rev.role_id, "child_reviewer");
        assert_eq!(rev.contract_id, "reviewer.v1");
        assert_eq!(rev.parent_run_id, "parent-1");
        assert_eq!(rev.parent_session_id, "session-1");
        assert_eq!(rev.model_id, "model-r");
        assert_eq!(rev.model_tier, "pro");
        assert_eq!(rev.reasoning_effort, "high");
        assert_eq!(rev.persona, "auditor");
        assert_eq!(rev.attempt_id, 2);
        assert_eq!(rev.timeout_secs, 90);
        assert_eq!(rev.attempt_key().task_id, "rev_1");
        assert_eq!(rev.attempt_key().attempt_id, 2);

        let wrt = DispatchEnvelope::writer("parent-2", "session-2", "model-w", serde_json::json!({"task": "fix"}))
            .with_task_id("wrt_1");
        assert_eq!(wrt.role_id, "child_writer");
        assert_eq!(wrt.contract_id, "writer.v1");
        assert_eq!(wrt.timeout_secs, 180);

        let wrk = DispatchEnvelope::worker("parent-3", "session-3", "model-s", serde_json::json!({"task": "scout"}))
            .with_task_id("wrk_1");
        assert_eq!(wrk.role_id, "child_worker");
        assert_eq!(wrk.contract_id, "worker.v1");
        assert_eq!(wrk.timeout_secs, 60);
    }

    #[test]
    fn test_baseline_reserved_fields_completeness() {
        use crate::agent::child_reviewer::{ChildReviewResult, ReviewerDeliveryData};
        use crate::agent::child_writer::WriterResult;
        use crate::agent::child_worker::WorkerResult;

        // 1. 检查 ReviewerDeliveryData 字段全部被黑名单覆盖
        let rev_delivery = ReviewerDeliveryData {
            findings: vec![],
            confidence: "confirmed".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            routing: None,
            withdrawn_findings: vec![],
            objection_verdicts: vec![],
            scorecard: None,
            extra: Default::default(),
        };
        let val = serde_json::to_value(&rev_delivery).unwrap();
        for key in val.as_object().unwrap().keys() {
            assert!(
                is_reserved_field_name(key),
                "ReviewerDeliveryData 字段 '{}' 未在 BASELINE_RESERVED_FIELDS 中登记，请同步更新保留字黑名单！",
                key
            );
        }

        // 2. 检查 ChildReviewResult 字段
        let child_res = ChildReviewResult {
            child_run_id: "c".to_string(),
            status: "s".to_string(),
            findings: vec![],
            confidence: "c".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 0,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: None,
            raw_result_json: None,
            kind: None,
            routing: None,
            withdrawn_findings: vec![],
            objection_verdicts: vec![],
            scorecard: None,
        };
        let val = serde_json::to_value(&child_res).unwrap();
        for key in val.as_object().unwrap().keys() {
            assert!(
                is_reserved_field_name(key),
                "ChildReviewResult 字段 '{}' 未在 BASELINE_RESERVED_FIELDS 中登记，请同步更新保留字黑名单！",
                key
            );
        }

        // 3. 检查 WriterResult 字段
        let writer_res = WriterResult {
            child_run_id: "w".to_string(),
            status: "s".to_string(),
            findings: vec![],
            confidence: "c".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 0,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: None,
            steps_used: 0,
            changed_files: vec![],
            test_command: "".to_string(),
            test_status: "".to_string(),
            test_output_tail: "".to_string(),
            workspace_copy: "".to_string(),
            patch_ref: None,
        };
        let val = serde_json::to_value(&writer_res).unwrap();
        for key in val.as_object().unwrap().keys() {
            assert!(
                is_reserved_field_name(key),
                "WriterResult 字段 '{}' 未在 BASELINE_RESERVED_FIELDS 中登记，请同步更新保留字黑名单！",
                key
            );
        }

        // 4. 检查 WorkerResult 字段
        let worker_res = WorkerResult {
            child_run_id: "w".to_string(),
            status: "s".to_string(),
            findings: vec![],
            confidence: "c".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 0,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: None,
            steps_used: 0,
            action_log: vec![],
        };
        let val = serde_json::to_value(&worker_res).unwrap();
        for key in val.as_object().unwrap().keys() {
            assert!(
                is_reserved_field_name(key),
                "WorkerResult 字段 '{}' 未在 BASELINE_RESERVED_FIELDS 中登记，请同步更新保留字黑名单！",
                key
            );
        }

        // 5. 检查 DeliveryReceipt 字段
        let receipt = DeliveryReceipt {
            child_run_id: "".to_string(),
            task_id: "".to_string(),
            attempt_id: 1,
            kind: "".to_string(),
            status: "".to_string(),
            duration_ms: 0,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: None,
            payload: None,
            raw_text: None,
        };
        let val = serde_json::to_value(&receipt).unwrap();
        for key in val.as_object().unwrap().keys() {
            assert!(
                is_reserved_field_name(key),
                "DeliveryReceipt 字段 '{}' 未在 BASELINE_RESERVED_FIELDS 中登记，请同步更新保留字黑名单！",
                key
            );
        }

        // 6. 检查 DispatchEnvelope 字段
        let env = DispatchEnvelope::reviewer("p", "s", "m", Value::Null);
        let val = serde_json::to_value(&env).unwrap();
        for key in val.as_object().unwrap().keys() {
            assert!(
                is_reserved_field_name(key),
                "DispatchEnvelope 字段 '{}' 未在 BASELINE_RESERVED_FIELDS 中登记，请同步更新保留字黑名单！",
                key
            );
        }
    }

    #[test]
    fn test_custom_field_name_validation() {
        // 命中保留字必须拒绝
        assert!(validate_custom_field_name("findings").is_err());
        assert!(validate_custom_field_name("confidence").is_err());
        assert!(validate_custom_field_name("status").is_err());
        assert!(validate_custom_field_name("changed_files").is_err());

        // 语法不合规必须拒绝
        assert!(validate_custom_field_name("").is_err());
        assert!(validate_custom_field_name("a").is_err()); // 长度 < 2
        assert!(validate_custom_field_name("1cve").is_err()); // 非小写字母开头
        assert!(validate_custom_field_name("cve-refs").is_err()); // 含连字符
        assert!(validate_custom_field_name("CveRefs").is_err()); // 驼峰大写

        // 合法 snake_case 自定义字段必须通过
        assert!(validate_custom_field_name("cve_refs").is_ok());
        assert!(validate_custom_field_name("risk_level").is_ok());
        assert!(validate_custom_field_name("cvss_score").is_ok());
    }

    #[test]
    fn test_contract_field_def_type_validation() {
        // 1. string 标量及枚举值域校验
        let str_field = ContractFieldDef::must_new("risk_level", ContractFieldType::String, "风险等级", true)
            .with_allowed_values(vec!["critical".into(), "high".into(), "low".into()]);

        assert!(str_field.validate_value(Some(&Value::String("high".into()))).is_ok());
        assert!(str_field.validate_value(Some(&Value::String("unknown".into()))).is_err());
        assert!(str_field.validate_value(Some(&Value::Number(100.into()))).is_err());
        assert!(str_field.validate_value(None).is_err()); // 必填缺失

        // 2. string[] 字符串数组校验
        let arr_field = ContractFieldDef::must_new("cve_refs", ContractFieldType::StringArray, "CVE列表", true);
        assert!(arr_field.validate_value(Some(&serde_json::json!(["CVE-2023-1", "CVE-2023-2"]))).is_ok());
        assert!(arr_field.validate_value(Some(&serde_json::json!([]))).is_ok());
        assert!(arr_field.validate_value(Some(&serde_json::json!(["CVE-1", 123]))).is_err()); // 元素类型错
        assert!(arr_field.validate_value(Some(&serde_json::json!("not_array"))).is_err());

        // 3. 选填字段校验（None 或 Null 允许通过）
        let opt_field = ContractFieldDef::must_new("extra_notes", ContractFieldType::String, "补充说明", false);
        assert!(opt_field.validate_value(None).is_ok());
        assert!(opt_field.validate_value(Some(&Value::Null)).is_ok());
        assert!(opt_field.validate_value(Some(&Value::String("ok".into()))).is_ok());
    }

    #[test]
    fn test_contract_schema_closed_set_and_shape_validation() {
        let schema = ContractSchema::new(vec![
            ContractFieldDef::must_new("cve_refs", ContractFieldType::StringArray, "CVE列表", true),
            ContractFieldDef::must_new("risk_level", ContractFieldType::String, "风险级别", true)
                .with_allowed_values(vec!["critical".into(), "high".into(), "low".into()]),
        ]);

        // 1. 合规输出必须通过
        let valid_payload = serde_json::json!({
            "findings": [],
            "confidence": "confirmed",
            "cve_refs": ["CVE-2024-0001"],
            "risk_level": "high"
        });
        assert!(schema.validate_shape(&valid_payload).is_ok());

        // 2. 模型拼错字段名 (如 cve_ids) 或额外注入垃圾键 → 闭集校验拦截
        let typo_payload = serde_json::json!({
            "findings": [],
            "confidence": "confirmed",
            "cve_ids": ["CVE-2024-0001"], // 拼错字段名！
            "risk_level": "high"
        });
        let err = schema.validate_shape(&typo_payload).unwrap_err();
        assert!(err.contains("闭集校验失败"));
        assert!(err.contains("cve_ids"));

        // 3. 缺少必填项校验
        let missing_payload = serde_json::json!({
            "findings": [],
            "confidence": "confirmed",
            "cve_refs": ["CVE-2024-0001"]
            // 缺少 risk_level
        });
        assert!(schema.validate_shape(&missing_payload).unwrap_err().contains("缺少必填字段: 'risk_level'"));

        // 4. sanitize_extra 清洗测试：仅收纳白名单字段，剔除未声明字段
        let mixed_payload = serde_json::json!({
            "cve_refs": ["CVE-2024-0001"],
            "risk_level": "high",
            "hallucinated_garbage": "some junk",
            "unregistered_flag": true
        });
        let clean = schema.sanitize_extra(&mixed_payload);
        assert_eq!(clean.len(), 2);
        assert!(clean.contains_key("cve_refs"));
        assert!(clean.contains_key("risk_level"));
        assert!(!clean.contains_key("hallucinated_garbage"));
    }

    #[test]
    fn test_contract_schema_prompt_instruction_rendering() {
        let schema = ContractSchema::new(vec![
            ContractFieldDef::must_new("cve_refs", ContractFieldType::StringArray, "关联CVE编号列表", true),
            ContractFieldDef::must_new("risk_level", ContractFieldType::String, "安全风险等级", true)
                .with_allowed_values(vec!["critical".into(), "high".into(), "low".into()]),
            ContractFieldDef::must_new("compliance_notes", ContractFieldType::String, "合规备注", false),
        ]);

        let prompt = schema.render_prompt_instructions();
        assert!(prompt.contains("【输出扩展契约规范】"));
        assert!(prompt.contains("`cve_refs` (string[], 必填): 关联CVE编号列表"));
        assert!(prompt.contains("`risk_level` (string, 必填，可选值: [critical, high, low]): 安全风险等级"));
        assert!(prompt.contains("`compliance_notes` (string, 选填): 合规备注"));
    }

    #[test]
    fn test_dispatch_envelope_validate_delivery_receipt_guard() {
        let envelope = DispatchEnvelope::reviewer("p1", "s1", "m1", Value::Null)
            .with_custom_field(ContractFieldDef::must_new("risk_level", ContractFieldType::String, "风险等级", true))
            .unwrap();

        // 1. 成功交付且字段齐全 → 校验通过
        let mut good_receipt = DeliveryReceipt {
            child_run_id: "r1".to_string(),
            task_id: "t1".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "succeeded".to_string(),
            duration_ms: 100,
            completion_tokens: 50,
            prompt_tokens: 50,
            error_kind: None,
            payload: Some(serde_json::json!({
                "findings": [],
                "confidence": "confirmed",
                "risk_level": "critical"
            })),
            raw_text: None,
        };
        assert!(envelope.validate_delivery_receipt(&mut good_receipt).is_ok());
        assert_eq!(good_receipt.status, "succeeded");

        // 2. 交付物缺少自定义必填字段 → 被信封卫士拦截并打标为 invalid_output
        let mut bad_receipt = DeliveryReceipt {
            child_run_id: "r2".to_string(),
            task_id: "t2".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "succeeded".to_string(),
            duration_ms: 100,
            completion_tokens: 50,
            prompt_tokens: 50,
            error_kind: None,
            payload: Some(serde_json::json!({
                "findings": [],
                "confidence": "confirmed"
                // 缺失 risk_level
            })),
            raw_text: None,
        };
        let val_err = envelope.validate_delivery_receipt(&mut bad_receipt).unwrap_err();
        assert!(val_err.contains("缺少必填字段: 'risk_level'"));
        assert_eq!(bad_receipt.status, "invalid_output");
        assert!(bad_receipt.error_kind.unwrap().contains("contract_violation"));

        // 3. 提取审查交付产物时，扩展字段安全注入 extra 桶中
        let delivery_data = good_receipt.as_reviewer_delivery().expect("应成功提取");
        assert_eq!(delivery_data.get_extra_str("risk_level"), Some("critical"));
    }
}

