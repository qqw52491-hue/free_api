//! 项目附加目录的运行时投影。数据库是持久权威；配置更新只刷新权限，不改任务/审查状态。
//! 这里约束受控文件 API，不是 OS 沙箱；任意 Shell/MCP 仍受各自的执行策略约束。
use crate::agent::context::resource_scope::{
    ResourceScopeKind, ResourceScopeRole, ResourceScopeSource, ResourceScopeValidation,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{LazyLock, RwLock},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryAccess {
    ReadOnly,
    ReadWrite,
}
impl DirectoryAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::ReadWrite => "read_write",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectDirectory {
    pub path: String,
    pub access: DirectoryAccess,
}
#[derive(Clone)]
struct SessionAccess {
    project_id: String,
    primary: PathBuf,
    directories: Vec<ProjectDirectory>,
}
static SESSIONS: LazyLock<RwLock<HashMap<String, SessionAccess>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub(crate) fn bind_session(
    session: &str,
    primary: &Path,
    config: Option<(String, Vec<ProjectDirectory>)>,
) {
    let mut map = SESSIONS.write().unwrap_or_else(|e| e.into_inner());
    if let Some((project_id, directories)) = config {
        map.insert(
            session.into(),
            SessionAccess {
                project_id,
                primary: primary.canonicalize().unwrap_or_else(|_| primary.into()),
                directories,
            },
        );
    } else {
        map.remove(session);
    }
}
pub(crate) fn update_project(project: &str, directories: &[ProjectDirectory]) {
    let mut map = SESSIONS.write().unwrap_or_else(|e| e.into_inner());
    for entry in map.values_mut().filter(|entry| entry.project_id == project) {
        entry.directories = directories.to_vec();
    }
}
pub(crate) fn revoke_project(project: &str) {
    // 保留运行中会话的主目录边界，只撤销附加权限。
    update_project(project, &[]);
}
pub(crate) fn directories(session: Option<&str>, primary: &Path) -> Vec<ProjectDirectory> {
    let Some(session) = session else {
        return vec![];
    };
    let primary = primary.canonicalize().unwrap_or_else(|_| primary.into());
    SESSIONS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(session)
        .filter(|entry| entry.primary == primary)
        .map(|entry| entry.directories.clone())
        .unwrap_or_default()
}
pub(crate) fn is_project_bound(session: &str, primary: &Path) -> bool {
    let primary = primary.canonicalize().unwrap_or_else(|_| primary.into());
    SESSIONS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(session)
        .is_some_and(|entry| entry.primary == primary)
}

/// 最深的显式目录规则优先，支持只读父目录内单独授权可写子目录，反向也成立。
pub(crate) fn access_for_canonical(
    session: Option<&str>,
    primary: &Path,
    target: &Path,
) -> Option<DirectoryAccess> {
    directories(session, primary)
        .into_iter()
        .filter(|dir| {
            let root = Path::new(&dir.path);
            // 存储的是物理路径；目录后来被换成指向别处的链接时不得迁移权限。
            root.canonicalize().ok().is_some_and(|p| p == root) && target.starts_with(root)
        })
        .max_by_key(|dir| Path::new(&dir.path).components().count())
        .map(|dir| dir.access)
}

pub(crate) fn permission_listing(session: Option<&str>, primary: &Path) -> String {
    directories(session, primary)
        .iter()
        .map(|dir| {
            format!(
                "- {} [{}]",
                dir.path,
                if dir.access == DirectoryAccess::ReadWrite {
                    "可读写"
                } else {
                    "只读"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const MARKER: &str = "\n\n## 附加目录权限（当前项目配置）\n";
pub(crate) fn render(session: &str, primary: &Path) -> String {
    let list = permission_listing(Some(session), primary);
    format!("{MARKER}{}\n本表显式扩展基础目录白名单，主工作区保持可读写；附加目录使用绝对路径，最深显式目录规则优先。local_file、search、view_image 按本表校验；local_shell 的 cwd 仅接受可写目录（只读参考请用文件/搜索工具）。Shell 任意脚本及外部 MCP 不属于 OS 只读沙箱，MCP 还需服务端独立授权。目录权限不替代方案 scope 或审查，也不切换主仓库的 git/diff 基线。", if list.is_empty() { "- 无" } else { &list })
}

/// 只替换可变权限段，保留缓存前缀其余内容和现有计划/义务/审查状态。
pub(crate) fn sync_context(
    session: &str,
    primary: &Path,
    context: &mut crate::agent::context::SandwichContext,
) -> bool {
    let dirs = directories(Some(session), primary);
    let desired: Vec<_> = dirs
        .iter()
        .map(|dir| {
            (
                dir.path.clone(),
                if dir.access == DirectoryAccess::ReadWrite {
                    ResourceScopeRole::Destination
                } else {
                    ResourceScopeRole::Reference
                },
            )
        })
        .collect();
    let existing: Vec<_> = context
        .resource_scope
        .bindings
        .iter()
        .filter(|b| b.source == ResourceScopeSource::ProjectConfigured)
        .map(|b| (b.locator.clone(), b.role.clone()))
        .collect();
    if existing != desired {
        context
            .resource_scope
            .bindings
            .retain(|b| b.source != ResourceScopeSource::ProjectConfigured);
        context.resource_scope.revision = context.resource_scope.revision.saturating_add(1);
        for (path, role) in desired {
            context.resource_scope.bind_filesystem_with_validation(
                role,
                &path,
                ResourceScopeSource::ProjectConfigured,
                ResourceScopeValidation::Valid,
            );
        }
    }
    let old = context
        .project_resource_manifest
        .clone()
        .unwrap_or_default();
    let base = old.split_once(MARKER).map(|(base, _)| base).unwrap_or(&old);
    let next = if is_project_bound(session, primary) {
        format!("{}{}", base, render(session, primary))
    } else {
        base.into()
    };
    if next != old {
        context.project_resource_manifest = Some(next);
        true
    } else {
        false
    }
}

/// Scope 观察仍报告相对主工作区 Outside，但已配置的可写目录不再被误当未授权。
pub(crate) fn scope_write_authorized(
    bindings: &[crate::agent::context::resource_scope::ResourceScopeBinding],
    raw: &str,
) -> bool {
    let p = Path::new(raw);
    if !p.is_absolute() {
        return false;
    }
    let Some(target) = crate::agent::path_guard::canonical_target(p) else {
        return false;
    };
    bindings
        .iter()
        .filter(|b| {
            b.source == ResourceScopeSource::ProjectConfigured
                && b.kind == ResourceScopeKind::Filesystem
        })
        .filter(|b| {
            let root = Path::new(&b.locator);
            root.canonicalize().ok().is_some_and(|p| p == root) && target.starts_with(root)
        })
        .max_by_key(|b| Path::new(&b.locator).components().count())
        .is_some_and(|b| b.role == ResourceScopeRole::Destination)
}

#[cfg(test)]
mod tests;
