use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScopeRole {
    Primary,
    Source,
    Destination,
    Reference,
}

impl ResourceScopeRole {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Source => "source",
            Self::Destination => "destination",
            Self::Reference => "reference",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScopeKind {
    Filesystem,
    Opaque,
}

impl ResourceScopeKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Opaque => "opaque",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScopeSource {
    UserExplicit,
    SessionSelected,
    EvaluationConfig,
    ToolVerified,
    Inferred,
}

impl ResourceScopeSource {
    fn as_str(&self) -> &'static str {
        match self {
            Self::UserExplicit => "user_explicit",
            Self::SessionSelected => "session_selected",
            Self::EvaluationConfig => "evaluation_config",
            Self::ToolVerified => "tool_verified",
            Self::Inferred => "inferred",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScopeConfidence {
    Verified,
    Declared,
    Provisional,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScopeValidation {
    Pending,
    Valid,
    Invalid,
}

impl ResourceScopeValidation {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Valid => "valid",
            Self::Invalid => "invalid",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScopeLifecycle {
    Bound,
    Expanded,
    Invalidated,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceRelation {
    Inside,
    Outside,
    Unknown,
}

impl ResourceRelation {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Inside => "inside",
            Self::Outside => "outside",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceOperation {
    Read,
    Write,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceScopeBinding {
    pub role: ResourceScopeRole,
    pub kind: ResourceScopeKind,
    pub locator: String,
    pub source: ResourceScopeSource,
    pub confidence: ResourceScopeConfidence,
    pub validation: ResourceScopeValidation,
    pub lifecycle: ResourceScopeLifecycle,
    pub observed_at: String,
    pub validated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefaultResolution {
    pub tool_or_service_identity: String,
    pub when_omitted_or_relative: ResourceRelation,
    pub locator: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceResolutionMismatch {
    pub tool_or_service_identity: String,
    pub requested: String,
    pub resolved: String,
    pub relation: ResourceRelation,
    pub occurrences: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceScopeObservation {
    pub requested: Option<String>,
    pub resolved: Option<String>,
    pub primary: Option<String>,
    pub relation: ResourceRelation,
    pub operation: ResourceOperation,
    pub should_guard: bool,
}

impl ResourceScopeObservation {
    pub fn render(&self) -> String {
        let requested = self.requested.as_deref().unwrap_or("unprovided");
        let resolved = self
            .resolved
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| "unresolved(default semantics unknown)".to_string());
        let primary = self.primary.as_deref().unwrap_or("unbound");
        let mut output = format!(
            "[Resource scope]\nrequested: {}\nresolved: {}\nprimary: {}\nrelation: {}",
            requested,
            resolved,
            primary,
            self.relation.as_str(),
        );
        if self.should_guard {
            output.push_str("\nwrite guard: outside declared resource scope");
        }
        output
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceScopeContract {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub bindings: Vec<ResourceScopeBinding>,
    #[serde(default)]
    pub tool_default_resolution: Vec<ToolDefaultResolution>,
    #[serde(default)]
    pub resolution_mismatches: Vec<ResourceResolutionMismatch>,
}

impl Default for ResourceScopeContract {
    fn default() -> Self {
        Self {
            revision: 0,
            bindings: Vec::new(),
            tool_default_resolution: Vec::new(),
            resolution_mismatches: Vec::new(),
        }
    }
}

impl ResourceScopeContract {
    pub fn bind_primary_filesystem(locator: &str, source: ResourceScopeSource) -> Self {
        let mut contract = Self::default();
        contract.bind_filesystem(ResourceScopeRole::Primary, locator, source);
        contract
    }

    pub fn bind_filesystem(
        &mut self,
        role: ResourceScopeRole,
        locator: &str,
        source: ResourceScopeSource,
    ) {
        self.bind_filesystem_with_validation(
            role,
            locator,
            source,
            ResourceScopeValidation::Pending,
        );
    }

    pub fn bind_filesystem_with_validation(
        &mut self,
        role: ResourceScopeRole,
        locator: &str,
        source: ResourceScopeSource,
        validation: ResourceScopeValidation,
    ) {
        if role == ResourceScopeRole::Primary
            && self.bindings.iter().any(|binding| {
                binding.role == ResourceScopeRole::Primary
                    && binding.lifecycle != ResourceScopeLifecycle::Invalidated
            })
        {
            self.rebind_primary_filesystem(locator, source);
            if let Some(primary) = self.primary_filesystem_mut() {
                primary.validation = validation.clone();
                primary.confidence = confidence_for(&primary.source, &validation);
                primary.validated_at = validation_timestamp(&validation);
            }
            return;
        }

        self.revision = self.revision.saturating_add(1);
        self.bindings.push(ResourceScopeBinding {
            role,
            kind: ResourceScopeKind::Filesystem,
            locator: locator.to_string(),
            source: source.clone(),
            confidence: confidence_for(&source, &validation),
            validation: validation.clone(),
            lifecycle: ResourceScopeLifecycle::Bound,
            observed_at: now_marker(),
            validated_at: validation_timestamp(&validation),
        });
    }

    pub fn rebind_primary_filesystem(&mut self, locator: &str, source: ResourceScopeSource) {
        for binding in &mut self.bindings {
            if binding.role == ResourceScopeRole::Primary
                && binding.lifecycle != ResourceScopeLifecycle::Invalidated
            {
                binding.lifecycle = ResourceScopeLifecycle::Invalidated;
            }
        }
        self.revision = self.revision.saturating_add(1);
        self.bindings.push(ResourceScopeBinding {
            role: ResourceScopeRole::Primary,
            kind: ResourceScopeKind::Filesystem,
            locator: locator.to_string(),
            source: source.clone(),
            confidence: confidence_for(&source, &ResourceScopeValidation::Pending),
            validation: ResourceScopeValidation::Pending,
            lifecycle: ResourceScopeLifecycle::Bound,
            observed_at: now_marker(),
            validated_at: None,
        });
    }

    pub fn primary_filesystem(&self) -> Option<&ResourceScopeBinding> {
        self.bindings.iter().rev().find(|binding| {
            binding.role == ResourceScopeRole::Primary
                && binding.kind == ResourceScopeKind::Filesystem
                && binding.lifecycle != ResourceScopeLifecycle::Invalidated
        })
    }

    fn primary_filesystem_mut(&mut self) -> Option<&mut ResourceScopeBinding> {
        self.bindings.iter_mut().rev().find(|binding| {
            binding.role == ResourceScopeRole::Primary
                && binding.kind == ResourceScopeKind::Filesystem
                && binding.lifecycle != ResourceScopeLifecycle::Invalidated
        })
    }

    pub fn observe_filesystem_tool(
        &mut self,
        tool_identity: &str,
        params: &Value,
        resolved_locator: Option<&str>,
        operation: ResourceOperation,
    ) -> ResourceScopeObservation {
        let requested = requested_locator(params);
        let primary = self
            .primary_filesystem()
            .map(|binding| binding.locator.clone());
        let relation = match (primary.as_deref(), resolved_locator) {
            (Some(primary), Some(resolved)) => filesystem_relation(primary, resolved),
            _ => ResourceRelation::Unknown,
        };
        let should_guard = operation == ResourceOperation::Write
            && relation == ResourceRelation::Outside
            && self.primary_filesystem().is_some_and(|binding| {
                matches!(
                    binding.confidence,
                    ResourceScopeConfidence::Verified | ResourceScopeConfidence::Declared
                )
            });
        self.record_default_resolution(
            tool_identity,
            &requested,
            resolved_locator,
            relation.clone(),
        );
        self.record_resolution_mismatch(
            tool_identity,
            requested.as_deref(),
            resolved_locator,
            primary.as_deref(),
            relation.clone(),
        );
        ResourceScopeObservation {
            requested,
            resolved: resolved_locator.map(str::to_string),
            primary,
            relation,
            operation,
            should_guard,
        }
    }

    pub fn observe_unknown_tool(
        &mut self,
        tool_identity: &str,
        params: &Value,
        operation: ResourceOperation,
    ) -> ResourceScopeObservation {
        let requested = requested_locator(params);
        self.record_default_resolution(tool_identity, &requested, None, ResourceRelation::Unknown);
        ResourceScopeObservation {
            requested,
            resolved: None,
            primary: self
                .primary_filesystem()
                .map(|binding| binding.locator.clone()),
            relation: ResourceRelation::Unknown,
            operation,
            should_guard: false,
        }
    }

    pub fn render_projection(&self) -> String {
        if self.bindings.is_empty() {
            return String::new();
        }
        let mut lines = vec![format!("[Active resource contract v{}]", self.revision)];
        for binding in self
            .bindings
            .iter()
            .filter(|binding| binding.lifecycle != ResourceScopeLifecycle::Invalidated)
        {
            lines.push(format!(
                "{} {}: {}",
                binding.role.as_str(),
                binding.kind.as_str(),
                binding.locator
            ));
            lines.push(format!(
                "binding: {}; validation: {}",
                binding.source.as_str(),
                binding.validation.as_str()
            ));
        }
        if let Some(default_resolution) = self.tool_default_resolution.last() {
            let locator = default_resolution.locator.as_deref().unwrap_or("unknown");
            lines.push(format!(
                "service default filesystem context: {}; relation: {}",
                locator,
                default_resolution.when_omitted_or_relative.as_str()
            ));
        }
        if let Some(mismatch) = self.resolution_mismatches.last() {
            lines.push(format!(
                "resolution mismatch: {} requested {} but resolved {}; relation: {}; occurrences: {}",
                mismatch.tool_or_service_identity,
                mismatch.requested,
                mismatch.resolved,
                mismatch.relation.as_str(),
                mismatch.occurrences
            ));
        }
        lines.push(
            "relative resource locators: preserve; resolved relation must be checked".to_string(),
        );
        lines.join("\n")
    }

    fn record_resolution_mismatch(
        &mut self,
        tool_identity: &str,
        requested: Option<&str>,
        resolved: Option<&str>,
        primary: Option<&str>,
        relation: ResourceRelation,
    ) {
        let (Some(requested), Some(resolved), Some(primary)) = (requested, resolved, primary)
        else {
            return;
        };
        if !Path::new(requested).is_absolute()
            || !Path::new(resolved).is_absolute()
            || filesystem_relation(primary, requested) != ResourceRelation::Inside
            || relation != ResourceRelation::Outside
        {
            return;
        }
        if let Some(existing) = self.resolution_mismatches.iter_mut().find(|mismatch| {
            mismatch.tool_or_service_identity == tool_identity
                && mismatch.requested == requested
                && mismatch.resolved == resolved
        }) {
            existing.occurrences = existing.occurrences.saturating_add(1);
            return;
        }
        self.revision = self.revision.saturating_add(1);
        self.resolution_mismatches.push(ResourceResolutionMismatch {
            tool_or_service_identity: tool_identity.to_string(),
            requested: requested.to_string(),
            resolved: resolved.to_string(),
            relation,
            occurrences: 1,
        });
        if self.resolution_mismatches.len() > 4 {
            self.resolution_mismatches.remove(0);
        }
    }

    fn record_default_resolution(
        &mut self,
        tool_identity: &str,
        requested: &Option<String>,
        resolved_locator: Option<&str>,
        relation: ResourceRelation,
    ) {
        let is_relative_or_omitted = requested
            .as_deref()
            .map(|locator| !Path::new(locator).is_absolute())
            .unwrap_or(true);
        if is_relative_or_omitted {
            self.tool_default_resolution.push(ToolDefaultResolution {
                tool_or_service_identity: tool_identity.to_string(),
                when_omitted_or_relative: relation,
                locator: resolved_locator.map(str::to_string),
            });
            if self.tool_default_resolution.len() > 4 {
                self.tool_default_resolution.remove(0);
            }
        }
    }
}

pub fn is_known_filesystem_tool(tool_identity: &str) -> bool {
    let leaf = tool_identity.rsplit('/').next().unwrap_or(tool_identity);
    matches!(
        leaf,
        "read_text_file"
            | "read_file"
            | "read_multiple_files"
            | "list_directory"
            | "list_directory_with_sizes"
            | "directory_tree"
            | "search_files"
            | "ripgrep_search"
            | "ripgrep_search_files"
            | "write_file"
            | "edit_file"
            | "move_file"
            | "create_directory"
    )
}

impl ResourceScopeContract {
    /// 优先依据工具执行结果中的事实分类：已知文件系统工具解析输出中的定位符，
    /// 其余工具若在结果中自报了绝对解析根（如 resolved_root 约定字段）同样按事实分类；
    /// 只有既不在已知清单、结果又无定位符时才产生 unknown 结论。
    pub fn observe_tool_result(
        &mut self,
        tool_identity: &str,
        params: &Value,
        output: &str,
        operation: ResourceOperation,
    ) -> Option<ResourceScopeObservation> {
        let requested = requested_locator(params);
        let resolved_from_output = resolved_locator_from_output(output);
        if is_known_filesystem_tool(tool_identity) {
            // 文件系统工具的相对定位符语义确定=文件相对 primary，直接解析
            // （与执行层 ensure_allowed_path 同一口径："."/"src/x"→工作区内）
            let resolved = resolved_from_output.or_else(|| {
                requested
                    .as_deref()
                    .and_then(|locator| self.resolve_to_primary(locator))
            });
            return Some(self.observe_filesystem_tool(
                tool_identity,
                params,
                resolved.as_deref(),
                operation,
            ));
        }
        if let Some(resolved) = resolved_from_output.as_deref() {
            return Some(self.observe_filesystem_tool(
                tool_identity,
                params,
                Some(resolved),
                operation,
            ));
        }
        requested.map(|req| {
            // 不透明工具：绝对路径天然可归类；相对路径只在来自 cwd 键时解析
            // （cwd 语义跨工具通用=工作目录）。path/source 等键的相对值语义
            // 未知（可能是服务内部 ID），不推测 → 维持 unknown。
            let joined = if Path::new(&req).is_absolute() {
                Some(req.clone())
            } else if params.get("cwd").and_then(Value::as_str) == Some(req.as_str()) {
                self.resolve_to_primary(&req)
            } else {
                None
            };
            match joined {
                Some(res) => self.observe_filesystem_tool(
                    tool_identity,
                    params,
                    Some(&res),
                    operation,
                ),
                None => self.observe_unknown_tool(tool_identity, params, operation),
            }
        })
    }

    /// 定位符按 primary 绑定解析：绝对路径原样返回；相对路径 join primary
    /// 后词法规整（"."/"src/x"→界内，"../etc"→界外）。无 primary 绑定时 None。
    fn resolve_to_primary(&self, locator: &str) -> Option<String> {
        if Path::new(locator).is_absolute() {
            return Some(locator.to_string());
        }
        self.primary_filesystem().map(|binding| {
            lexically_normalize(&Path::new(&binding.locator).join(locator))
                .to_string_lossy()
                .to_string()
        })
    }

    /// 仅在已知文件系统写工具且请求已明确为绝对越界定位符时守卫；不推测相对路径。
    pub fn guard_known_outside_write(
        &mut self,
        tool_identity: &str,
        params: &Value,
    ) -> Option<ResourceScopeObservation> {
        if !is_known_filesystem_tool(tool_identity) {
            return None;
        }
        let requested = requested_locator(params)?;
        if !Path::new(&requested).is_absolute() {
            return None;
        }
        let observation = self.observe_filesystem_tool(
            tool_identity,
            params,
            Some(&requested),
            ResourceOperation::Write,
        );
        observation.should_guard.then_some(observation)
    }
}

fn resolved_locator_from_output(output: &str) -> Option<String> {
    let value: Value = serde_json::from_str(output).ok()?;
    find_absolute_locator(&value)
}

fn find_absolute_locator(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => {
            // resolved_root 是进程内工具（如联邦检索）自报的真实解析根：
            // 依据工具自己的执行事实分类，而不是按工具名硬编码语义。
            for key in [
                "resolved",
                "resolved_path",
                "resolved_root",
                "cwd",
                "path",
                "workspace",
            ] {
                if let Some(locator) = object.get(key).and_then(Value::as_str) {
                    if Path::new(locator).is_absolute() {
                        return Some(locator.to_string());
                    }
                }
            }
            object.values().find_map(find_absolute_locator)
        }
        Value::Array(items) => items.iter().find_map(find_absolute_locator),
        _ => None,
    }
}

fn confidence_for(
    source: &ResourceScopeSource,
    validation: &ResourceScopeValidation,
) -> ResourceScopeConfidence {
    match validation {
        ResourceScopeValidation::Invalid => ResourceScopeConfidence::Provisional,
        ResourceScopeValidation::Valid if *source == ResourceScopeSource::ToolVerified => {
            ResourceScopeConfidence::Verified
        }
        _ => ResourceScopeConfidence::Declared,
    }
}

fn validation_timestamp(validation: &ResourceScopeValidation) -> Option<String> {
    match validation {
        ResourceScopeValidation::Pending => None,
        ResourceScopeValidation::Valid | ResourceScopeValidation::Invalid => Some(now_marker()),
    }
}

fn now_marker() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn requested_locator(params: &Value) -> Option<String> {
    let object = params.as_object()?;
    ["path", "cwd", "source", "destination", "locator"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str).map(str::to_string))
}

fn filesystem_relation(primary: &str, resolved: &str) -> ResourceRelation {
    let primary_path = Path::new(primary);
    let resolved_path = Path::new(resolved);
    if !primary_path.is_absolute() || !resolved_path.is_absolute() {
        return ResourceRelation::Unknown;
    }
    if is_path_within(resolved_path, primary_path) {
        ResourceRelation::Inside
    } else {
        ResourceRelation::Outside
    }
}

fn is_path_within(candidate: &Path, root: &Path) -> bool {
    let candidate_components: Vec<Component<'_>> = candidate.components().collect();
    let root_components: Vec<Component<'_>> = root.components().collect();
    candidate_components.starts_with(&root_components)
}

/// 词法级路径规整：不动文件系统，把 ".." 和 "." 组件折掉——
/// 供观测层把相对定位符 join primary 后归类 inside/outside。
pub(crate) fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn primary_contract() -> ResourceScopeContract {
        ResourceScopeContract::bind_primary_filesystem(
            "/workspace/free_api",
            ResourceScopeSource::UserExplicit,
        )
    }

    #[test]
    fn known_filesystem_read_at_primary_is_inside_without_parameter_rewrite() {
        let mut contract = primary_contract();
        let params = json!({"path": "."});
        let observation = contract.observe_filesystem_tool(
            "read_text_file",
            &params,
            Some("/workspace/free_api"),
            ResourceOperation::Read,
        );

        assert_eq!(params, json!({"path": "."}));
        assert_eq!(observation.relation, ResourceRelation::Inside);
        assert!(!observation.should_guard);
        assert!(observation.render().contains("relation: inside"));
    }

    #[test]
    fn known_filesystem_read_outside_is_annotated_but_not_blocked() {
        let mut contract = primary_contract();
        let params = json!({"path": "."});
        let observation = contract.observe_filesystem_tool(
            "read_text_file",
            &params,
            Some("/workspace/other_project"),
            ResourceOperation::Read,
        );

        assert_eq!(observation.relation, ResourceRelation::Outside);
        assert!(!observation.should_guard);
        assert!(observation.render().contains("relation: outside"));
    }

    #[test]
    fn unknown_default_semantics_stay_unknown_and_do_not_block() {
        let mut contract = primary_contract();
        let params = json!({"path": "."});
        let observation =
            contract.observe_unknown_tool("custom_mcp/opaque", &params, ResourceOperation::Read);

        assert_eq!(observation.relation, ResourceRelation::Unknown);
        assert!(!observation.should_guard);
        assert!(observation
            .render()
            .contains("unresolved(default semantics unknown)"));
    }

    #[test]
    fn source_destination_and_reference_bindings_project_without_role_loss() {
        let mut contract = primary_contract();
        contract.bind_filesystem(
            ResourceScopeRole::Destination,
            "/workspace/target",
            ResourceScopeSource::UserExplicit,
        );
        contract.bind_filesystem(
            ResourceScopeRole::Reference,
            "/workspace/reference",
            ResourceScopeSource::ToolVerified,
        );

        let projection = contract.render_projection();
        assert!(projection.contains("primary filesystem: /workspace/free_api"));
        assert!(projection.contains("destination filesystem: /workspace/target"));
        assert!(projection.contains("reference filesystem: /workspace/reference"));
    }

    #[test]
    fn explicit_rebind_increments_revision_and_invalidates_old_primary() {
        let mut contract = primary_contract();
        let previous = contract.revision;
        contract
            .rebind_primary_filesystem("/workspace/second", ResourceScopeSource::EvaluationConfig);

        assert_eq!(contract.revision, previous + 1);
        assert_eq!(
            contract.primary_filesystem().unwrap().locator,
            "/workspace/second"
        );
        assert!(contract.bindings.iter().any(|binding| {
            binding.locator == "/workspace/free_api"
                && binding.lifecycle == ResourceScopeLifecycle::Invalidated
        }));
    }

    #[test]
    fn failed_binding_validation_never_becomes_verified() {
        let mut contract = ResourceScopeContract::default();
        contract.bind_filesystem_with_validation(
            ResourceScopeRole::Primary,
            "/workspace/missing",
            ResourceScopeSource::UserExplicit,
            ResourceScopeValidation::Invalid,
        );

        let binding = contract.primary_filesystem().unwrap();
        assert_eq!(binding.validation, ResourceScopeValidation::Invalid);
        assert_ne!(binding.confidence, ResourceScopeConfidence::Verified);
    }

    #[test]
    fn unknown_tool_schema_never_generates_a_false_guard() {
        let mut contract = primary_contract();
        let observation = contract.observe_unknown_tool(
            "opaque_service/mutate",
            &json!({"project": "elsewhere"}),
            ResourceOperation::Write,
        );

        assert_eq!(observation.relation, ResourceRelation::Unknown);
        assert!(!observation.should_guard);
    }

    #[test]
    fn outside_write_is_guarded_without_mutating_parameters_but_read_is_not() {
        let mut contract = primary_contract();
        let params = json!({"path": "/workspace/outside/file.txt"});
        let write = contract.observe_filesystem_tool(
            "write_file",
            &params,
            Some("/workspace/outside/file.txt"),
            ResourceOperation::Write,
        );
        let read = contract.observe_filesystem_tool(
            "read_text_file",
            &params,
            Some("/workspace/outside/file.txt"),
            ResourceOperation::Read,
        );

        assert_eq!(params, json!({"path": "/workspace/outside/file.txt"}));
        assert!(write.should_guard);
        assert!(!read.should_guard);
    }

    #[test]
    fn active_contract_projection_survives_history_dehydration() {
        let mut contract = primary_contract();
        contract.observe_filesystem_tool(
            "read_text_file",
            &json!({"path": "."}),
            Some("/workspace/free_api/src/lib.rs"),
            ResourceOperation::Read,
        );

        let projection = contract.render_projection();
        assert!(projection.contains("[Active resource contract v"));
        assert!(projection.contains("primary filesystem: /workspace/free_api"));
    }

    #[test]
    fn absolute_target_resolved_outside_becomes_persistent_mismatch_fact() {
        let mut contract = primary_contract();
        let params = json!({"path": "/workspace/free_api/src-tauri"});
        let observation = contract.observe_filesystem_tool(
            "ripgrep_search",
            &params,
            Some("/workspace/mcp-default/hub.js"),
            ResourceOperation::Read,
        );
        contract.observe_filesystem_tool(
            "ripgrep_search",
            &params,
            Some("/workspace/mcp-default/hub.js"),
            ResourceOperation::Read,
        );

        assert_eq!(params, json!({"path": "/workspace/free_api/src-tauri"}));
        assert_eq!(observation.relation, ResourceRelation::Outside);
        assert_eq!(contract.resolution_mismatches.len(), 1);
        assert_eq!(contract.resolution_mismatches[0].occurrences, 2);
        let projection = contract.render_projection();
        assert!(projection.contains("resolution mismatch: ripgrep_search requested /workspace/free_api/src-tauri but resolved /workspace/mcp-default/hub.js"));

        let restored: ResourceScopeContract =
            serde_json::from_value(serde_json::to_value(&contract).unwrap()).unwrap();
        assert_eq!(restored.resolution_mismatches[0].occurrences, 2);
        let legacy: ResourceScopeContract = serde_json::from_value(
            json!({"revision": 0, "bindings": [], "tool_default_resolution": []}),
        )
        .unwrap();
        assert!(legacy.resolution_mismatches.is_empty());
    }

    #[test]
    fn tool_reporting_resolved_root_classifies_relative_locator_as_inside() {
        let mut contract = primary_contract();
        // 内置联邦检索在结果中自报 resolved_root（绝对路径），即使工具不在
        // 已知文件系统清单内也按结果侧事实分类；相对 path 不再永远 unknown。
        let output = r#"{"query":"Marker","total_hits":1,"hits":[{"id_or_path":"src/lib.rs:3"}],"resolved_root":"/workspace/free_api/src"}"#;
        let observation = contract
            .observe_tool_result(
                "search",
                &json!({"query": "Marker", "path": "src"}),
                output,
                ResourceOperation::Read,
            )
            .expect("应产生观察");

        assert_eq!(observation.relation, ResourceRelation::Inside);
        assert!(!observation.should_guard);
        assert!(observation.render().contains("relation: inside"));
    }

    #[test]
    fn tool_without_result_locator_keeps_unknown_relation() {
        let mut contract = primary_contract();
        let output = r#"{"query":"x","total_hits":0,"hits":[]}"#;
        let observation = contract
            .observe_tool_result(
                "opaque_service/query",
                &json!({"path": "data/feed"}),
                output,
                ResourceOperation::Read,
            )
            .expect("传入定位符的未知工具应产生观察");

        assert_eq!(observation.relation, ResourceRelation::Unknown);
        assert!(!observation.should_guard);
    }

    #[test]
    fn relative_cwd_resolves_against_primary_and_dotdot_escapes_outside() {
        let mut contract = primary_contract();
        let output = r#"{"command":"find .","exit_code":0}"#;

        // cwd:"." → primary 根 → inside（实测：local_shell 每步都传 "."，
        // 此前一律 unknown 纯属观测层不认相对路径）
        let obs = contract
            .observe_tool_result(
                "local_shell",
                &json!({"command": "find .", "cwd": "."}),
                output,
                ResourceOperation::Read,
            )
            .unwrap();
        assert_eq!(obs.relation, ResourceRelation::Inside);

        // cwd:"../other" → primary/../other → outside（相对路径逃逸
        // 此前也是 unknown——写守卫对 ../ 形同虚设的缺口一并堵上）
        let obs2 = contract
            .observe_tool_result(
                "local_shell",
                &json!({"command": "ls", "cwd": "../other"}),
                output,
                ResourceOperation::Read,
            )
            .unwrap();
        assert_eq!(obs2.relation, ResourceRelation::Outside);
    }
}
