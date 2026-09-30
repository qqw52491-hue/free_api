use crate::agent::types::DispatchResult;
use crate::db::DbState;

pub fn dispatch(db: &DbState, action: &str, params: &serde_json::Value) -> DispatchResult {
    let act = if !action.is_empty() {
        action
    } else {
        params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list")
    };

    match act {
        "list" => {
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("获取数据库锁失败: {}", e),
                        success: false,
                        route: "skill_manager:list".to_string(),
                    }
                }
            };
            let mut stmt = match conn.prepare("SELECT id, kind, name, description, content, enabled, updated_at FROM agent_custom_instructions ORDER BY updated_at DESC") {
                Ok(s) => s,
                Err(e) => return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("查询失败: {}", e),
                    success: false,
                    route: "skill_manager:list".to_string(),
                },
            };
            let rows: Vec<serde_json::Value> = stmt
                .query_map([], |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "kind": row.get::<_, String>(1)?,
                        "name": row.get::<_, String>(2)?,
                        "description": row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        "content": row.get::<_, String>(4)?,
                        "enabled": row.get::<_, i64>(5)? != 0,
                        "updated_at": row.get::<_, String>(6)?,
                    }))
                })
                .ok()
                .map(|iter| iter.flatten().collect())
                .unwrap_or_default();

            DispatchResult { view: None,
                stdout: serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()),
                stderr: String::new(),
                success: true,
                route: "skill_manager:list".to_string(),
            }
        }
        "add" => {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let content = params
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let kind = params
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("skill");
            let custom_desc = params
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string());
            let model_id = params
                .get("model_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if name.is_empty() || content.is_empty() {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: "❌ 缺少必填参数 name 或 content".to_string(),
                    success: false,
                    route: "skill_manager:add".to_string(),
                };
            }

            // 触发卡是可选子结果：LLM 提炼失败不阻断保存，但必须显式可见
            let mut card_failure: Option<String> = None;
            let desc = if let Some(d) = custom_desc.filter(|s| !s.is_empty()) {
                d
            } else if kind == "skill" {
                let block_res: Result<String, String> = (|| {
                    let handle = tokio::runtime::Handle::try_current();
                    let f = async {
                        let model_cfg = if let Some(ref mid) =
                            model_id.filter(|s| !s.trim().is_empty())
                        {
                            crate::llm::ModelConfig::from_db_state(db, mid)?
                        } else {
                            let conn = db.0.lock().map_err(|e| e.to_string())?;
                            let mut stmt = conn
                                .prepare("SELECT m.id FROM models m JOIN platforms p ON p.id = m.platform_id WHERE m.enabled = 1 ORDER BY m.created_at DESC")
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
                                if let Ok(cfg) = crate::llm::ModelConfig::from_db_state(db, &id) {
                                    found = Some(cfg);
                                    break;
                                }
                            }
                            found.ok_or_else(|| "未找到可用的大模型配置".to_string())?
                        };

                        crate::agent::skill_context::generate_llm_skill_summary(
                            name, content, &model_cfg,
                        )
                        .await
                    };
                    if let Ok(h) = handle {
                        h.block_on(f)
                    } else {
                        futures::executor::block_on(f)
                    }
                })();
                match block_res {
                    Ok(summary) => summary,
                    Err(error) => {
                        // 只记录错误类别，不回显模型配置等敏感内容
                        eprintln!(
                            "⚠️ [skill_manager] Skill [{}] 触发卡提炼失败: {}",
                            name, error
                        );
                        card_failure = Some(error);
                        String::new()
                    }
                }
            } else {
                String::new()
            };

            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("数据库锁获取失败: {}", e),
                        success: false,
                        route: "skill_manager:add".to_string(),
                    }
                }
            };

            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            // description 列为 NOT NULL DEFAULT ''：空卡片必须写空字符串，写 NULL 会违反约束
            let desc_val = desc.as_str();

            if let Err(e) = conn.execute(
                "INSERT INTO agent_custom_instructions (id, kind, name, description, content, enabled, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
                rusqlite::params![id, kind, name, desc_val, content, now],
            ) {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("保存 Skill [{}] 写库失败: {}", name, e),
                    success: false,
                    route: "skill_manager:add:db_error".to_string(),
                };
            }

            let kind_label = if kind == "skill" {
                "Skill"
            } else {
                "自定义提示词"
            };
            let card_note = match desc.trim() {
                s if s.is_empty() => {
                    if card_failure.is_some() {
                        "⚠️ 技能触发卡未生成（LLM 提炼失败；内容已保存，可重新调用 add 补充 description）".to_string()
                    } else {
                        String::new()
                    }
                }
                s => format!("💡 技能触发卡: {}", s),
            };

            DispatchResult { view: None,
                stdout: format!("✅ 成功保存 {} [{}]！\n{}", kind_label, name, card_note),
                stderr: String::new(),
                success: true,
                route: "skill_manager:add".to_string(),
            }
        }
        // 自助安装技能仓库：机械做 clone/copy → 探测 SKILL.md → 登记 DB 卡片
        // （kind='skill'，懒加载路由）。歧义结构（无 SKILL.md）只回清单，
        // 由模型自行判断后用 add 手工提炼——结构识别是认知不推给工具猜。
        "install" => {
            let url = params
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if url.is_empty() {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: "❌ 缺少必填参数 url（Git 仓库地址或本地目录路径）".to_string(),
                    success: false,
                    route: "skill_manager:install".to_string(),
                };
            }
            let name_override = params
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());

            // FREE_API_SKILLS_DIR 可重定位技能根目录（测试隔离/用户自选盘位）
            let skills_root = std::env::var("FREE_API_SKILLS_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| {
                    dirs_next::data_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                        .join("free-api-chat")
                        .join("skills")
                });
            if let Err(e) = std::fs::create_dir_all(&skills_root) {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("❌ 技能目录创建失败: {}", e),
                    success: false,
                    route: "skill_manager:install".to_string(),
                };
            }

            // 仓库名 = url 末段去 .git；本地路径取目录名
            let repo_name = url
                .rsplit('/')
                .next()
                .unwrap_or("repo")
                .trim_end_matches(".git")
                .to_string();
            let repo_name = if repo_name.is_empty() {
                "repo".to_string()
            } else {
                repo_name
            };
            let dest = skills_root.join(&repo_name);

            // —— 获取阶段：本地目录复制 or git clone（depth 1 瘦身）——
            let fetch_note = if dest.exists() {
                format!("已存在（跳过获取）：{}", dest.display())
            } else if url.starts_with("http") || url.starts_with("git@") {
                let out = std::process::Command::new("git")
                    .args(["clone", "--depth", "1", url])
                    .arg(&dest)
                    .output();
                match out {
                    Ok(o) if o.status.success() => {
                        // .git 不入技能资产：瘦身 + 防误把历史当内容
                        let _ = std::fs::remove_dir_all(dest.join(".git"));
                        format!("git clone --depth 1 → {}", dest.display())
                    }
                    Ok(o) => {
                        return DispatchResult { view: None,
                            stdout: String::new(),
                            stderr: format!(
                                "❌ git clone 失败: {}",
                                String::from_utf8_lossy(&o.stderr)
                                    .chars()
                                    .take(400)
                                    .collect::<String>()
                            ),
                            success: false,
                            route: "skill_manager:install:clone".to_string(),
                        };
                    }
                    Err(e) => {
                        return DispatchResult { view: None,
                            stdout: String::new(),
                            stderr: format!("❌ git 不可用或 clone 启动失败: {}", e),
                            success: false,
                            route: "skill_manager:install:clone".to_string(),
                        };
                    }
                }
            } else {
                let src = std::path::PathBuf::from(url);
                if !src.is_dir() {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("❌ url 非 http 地址且本地目录不存在: {}", url),
                        success: false,
                        route: "skill_manager:install:src".to_string(),
                    };
                }
                match copy_dir_recursive(&src, &dest) {
                    Ok(()) => format!("本地复制 → {}", dest.display()),
                    Err(e) => {
                        return DispatchResult { view: None,
                            stdout: String::new(),
                            stderr: format!("❌ 本地目录复制失败: {}", e),
                            success: false,
                            route: "skill_manager:install:src".to_string(),
                        };
                    }
                }
            };

            // —— 探测阶段：找 SKILL.md（含大小写变体），限定深度防深树漫游 ——
            let mut skill_files = Vec::new();
            collect_skill_files(&dest, &dest, 0, &mut skill_files);
            skill_files.sort();

            if skill_files.is_empty() {
                let top: Vec<String> = std::fs::read_dir(&dest)
                    .map(|rd| {
                        rd.flatten()
                            .map(|e| e.file_name().to_string_lossy().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                return DispatchResult { view: None,
                    stdout: format!(
                        "⚠️ 获取成功但探测结果：该仓库未发现 SKILL.md，无法自动登记卡片。\n{}\n顶层结构: {}\n建议：人工查看后用 `skill_manager.add` 提炼要点入库，或在 content 里写明本地路径 {}/ 作为资产引用。",
                        fetch_note,
                        top.join(", "),
                        dest.display()
                    ),
                    stderr: String::new(),
                    success: true,
                    route: "skill_manager:install:no_skill_md".to_string(),
                };
            }

            // —— 登记阶段：每个 SKILL.md 一张 DB 卡片（同名覆盖更新）——
            const MAX_AUTO_REGISTER: usize = 8;
            let now = chrono::Utc::now().to_rfc3339();
            let mut registered: Vec<serde_json::Value> = Vec::new();
            let mut errors: Vec<String> = Vec::new();
            for (idx, md_path) in skill_files.iter().enumerate() {
                if idx >= MAX_AUTO_REGISTER {
                    errors.push(format!(
                        "共 {} 个 SKILL.md，超出自动登记上限 {}，其余请手工 add",
                        skill_files.len(),
                        MAX_AUTO_REGISTER
                    ));
                    break;
                }
                let skill_dir = md_path.parent().unwrap_or(&dest);
                let skill_name = name_override
                    .clone()
                    .filter(|_| skill_files.len() == 1)
                    .unwrap_or_else(|| {
                        skill_dir
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| repo_name.clone())
                    });
                let body = std::fs::read_to_string(md_path).unwrap_or_default();
                let desc = frontmatter_value(&body, "when_to_use")
                    .or_else(|| frontmatter_value(&body, "description"))
                    .unwrap_or_else(|| {
                        body.lines()
                            .find(|l| !l.trim().is_empty() && !l.starts_with("---"))
                            .map(|l| l.trim().trim_start_matches('#').trim().chars().take(120).collect())
                            .unwrap_or_else(|| format!("{} 技能包", skill_name))
                    });
                // 卡片正文 = 指针 + 手册截断体：模型被注入后即知手册与资产路径，
                // 再按需 local_file 细读——12K 上限内放最有行动力的头部内容。
                let body_clip: String = body.trim().chars().take(9_000).collect();
                let content = format!(
                    "【技能资产包·自动安装】\n本地目录: {}\n手册: {}\n来源: {}\n资产/模板/脚本等同目录下，按需用 local_file 读取。\n\n{}",
                    skill_dir.display(),
                    md_path.display(),
                    url,
                    body_clip
                );

                let conn = match db.0.lock() {
                    Ok(c) => c,
                    Err(e) => {
                        errors.push(format!("DB 锁失败: {}", e));
                        break;
                    }
                };
                let existing: Option<String> = conn
                    .query_row(
                        "SELECT id FROM agent_custom_instructions WHERE kind='skill' AND name=?1",
                        rusqlite::params![skill_name],
                        |r| r.get(0),
                    )
                    .ok();
                let res = if let Some(id) = existing {
                    conn.execute(
                        "UPDATE agent_custom_instructions SET description=?1, content=?2, enabled=1, updated_at=?3 WHERE id=?4",
                        rusqlite::params![desc, content, now, id],
                    )
                    .map(|_| "updated")
                } else {
                    conn.execute(
                        "INSERT INTO agent_custom_instructions (id, kind, name, description, content, enabled, created_at, updated_at) VALUES (?1,'skill',?2,?3,?4,1,?5,?5)",
                        rusqlite::params![uuid::Uuid::new_v4().to_string(), skill_name, desc, content, now],
                    )
                    .map(|_| "added")
                };
                drop(conn);
                match res {
                    Ok(how) => registered.push(serde_json::json!({
                        "name": skill_name,
                        "action": how,
                        "manual": md_path.display().to_string(),
                        "when_to_use": desc,
                    })),
                    Err(e) => errors.push(format!("登记 [{}] 失败: {}", skill_name, e)),
                }
            }

            let out = serde_json::json!({
                "installed_to": dest.display().to_string(),
                "fetch": fetch_note,
                "registered": registered,
                "errors": errors,
                "next": "已入路由索引（kind=skill 懒加载）。本轮可用 next_tool_hint 立即取回手册；下轮起 triage 可自动推荐。",
            });
            DispatchResult { view: None,
                stdout: serde_json::to_string_pretty(&out).unwrap_or_default(),
                stderr: String::new(),
                success: errors.is_empty(),
                route: "skill_manager:install".to_string(),
            }
        }
        "delete" => {
            let target = params
                .get("id")
                .or_else(|| params.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let confirm = params
                .get("confirm")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if target.is_empty() {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: "❌ 缺少必填参数 id 或 name".to_string(),
                    success: false,
                    route: "skill_manager:delete".to_string(),
                };
            }
            if !confirm {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("⚠️ 警告：删除 [{}] 将永久移除配置，请确认并在 command 中附带 confirm: true", target),
                    success: false,
                    route: "skill_manager:delete".to_string(),
                };
            }
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("数据库锁获取失败: {}", e),
                        success: false,
                        route: "skill_manager:delete".to_string(),
                    }
                }
            };
            let affected = conn.execute(
                "DELETE FROM agent_custom_instructions WHERE id=?1 OR name=?1",
                rusqlite::params![target],
            );
            match affected {
                Ok(0) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!(
                            "❌ 未找到名为 [{}] 的 Skill 或提示词；请先用 list 查看现有条目",
                            target
                        ),
                        success: false,
                        route: "skill_manager:delete:not_found".to_string(),
                    };
                }
                Err(e) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("删除 Skill [{}] 写库失败: {}", target, e),
                        success: false,
                        route: "skill_manager:delete".to_string(),
                    };
                }
                Ok(_) => {}
            }

            DispatchResult { view: None,
                stdout: format!("✅ 已成功删除 [{}]", target),
                stderr: String::new(),
                success: true,
                route: "skill_manager:delete".to_string(),
            }
        }
        _ => DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!(
                "❌ 未知的 skill_manager 动作 action='{}'。支持的动作: list, add, install, delete",
                act
            ),
            success: false,
            route: "skill_manager:unknown".to_string(),
        },
    }
}

/// 递归复制目录（本地技能源安装用；跳过 .git 防把历史当资产）。
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let to = dst.join(&name);
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// 探测 SKILL.md（大小写不敏感），深度限 4 层防深树漫游。
fn collect_skill_files(
    root: &std::path::Path,
    dir: &std::path::Path,
    depth: usize,
    out: &mut Vec<std::path::PathBuf>,
) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_skill_files(root, &path, depth + 1, out);
        } else if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case("skill.md")
        {
            out.push(path);
        }
    }
}

/// 提取 YAML frontmatter 标量字段（description/when_to_use 等单行值）。
fn frontmatter_value(body: &str, key: &str) -> Option<String> {
    let mut in_fm = false;
    for line in body.lines() {
        let t = line.trim();
        if t == "---" {
            if in_fm {
                return None;
            }
            in_fm = true;
            continue;
        }
        if in_fm {
            if let Some(v) = t.strip_prefix(key).and_then(|s| s.strip_prefix(':')) {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn test_db() -> DbState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_custom_instructions (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL CHECK(kind IN ('prompt', 'skill')),
                name TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                content TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )
        .unwrap();
        DbState(Arc::new(Mutex::new(conn)))
    }

    fn insert_skill(db: &DbState, name: &str) {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO agent_custom_instructions (id, kind, name, description, content, enabled, created_at, updated_at)
             VALUES ('sid-1', 'skill', ?1, '', 'content', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![name],
        )
        .unwrap();
    }

    #[test]
    fn unknown_action_lists_only_implemented_actions() {
        let db = test_db();
        let result = dispatch(&db, "batch_import", &json!({}));
        assert!(!result.success);
        // stderr 开头会回显用户传入的 action 名（合理）；
        // 关键是"支持的动作"推荐清单不得包含未实现 action
        let supported_list = result
            .stderr
            .split("支持的动作:")
            .nth(1)
            .unwrap_or_default();
        assert!(!supported_list.contains("batch_import"));
        assert!(!supported_list.contains("toggle"));
        assert!(supported_list.contains("list"));
        assert!(supported_list.contains("add"));
        assert!(supported_list.contains("install"));
        assert!(supported_list.contains("delete"));
    }

    #[test]
    fn tool_manual_does_not_advertise_unimplemented_actions() {
        let manual = include_str!("../../../prompts/tools/skill_manager.md");
        // 未实现 action 只允许出现在"不支持"的否定性说明行，不允许作为编号动作声明
        for action in ["batch_import", "toggle"] {
            for line in manual.lines() {
                if line.contains(action) {
                    assert!(
                        line.contains("不支持"),
                        "手册把未实现 action {} 声明为受支持动作: {}",
                        action,
                        line
                    );
                }
            }
        }
        assert!(manual.contains("`list`"));
        assert!(manual.contains("`add`"));
        assert!(manual.contains("`delete`"));
    }

    #[test]
    fn add_card_failure_is_visible_but_config_still_saved() {
        // models 表不存在 → 默认模型查找失败 → 触发卡提炼失败。
        // 配置必须仍保存成功，且输出显式说明卡片未生成，不伪造空卡。
        let db = test_db();
        let result = dispatch(
            &db,
            "add",
            &json!({
                "name": "SQL规范检查",
                "content": "检查所有 SQL 查询必须包含 LIMIT。"
            }),
        );
        assert!(
            result.success,
            "卡片失败不应阻断配置保存: {}",
            result.stderr
        );
        assert!(result.stdout.contains("成功保存"));
        assert!(
            result.stdout.contains("触发卡未生成"),
            "输出: {}",
            result.stdout
        );
        // 绝不出现"💡 技能触发卡: "后跟空内容这种伪造成功卡
        assert!(!result.stdout.contains("💡 技能触发卡"));
        // 内容确实入库
        let conn = db.0.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_custom_instructions WHERE name='SQL规范检查'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn add_write_failure_is_not_reported_as_success() {
        // kind 违反 CHECK 约束 → INSERT 失败 → 必须返回 success:false
        let db = test_db();
        let result = dispatch(
            &db,
            "add",
            &json!({
                "name": "bad-kind-skill",
                "content": "内容",
                "kind": "invalid_kind"
            }),
        );
        assert!(!result.success, "写库失败不能报告成功: {}", result.stdout);
        assert!(result.stderr.contains("写库失败"));
    }

    #[test]
    fn delete_missing_skill_fails_honestly() {
        let db = test_db();
        let result = dispatch(
            &db,
            "delete",
            &json!({ "name": "不存在的技能", "confirm": true }),
        );
        assert!(!result.success);
        assert!(result.stderr.contains("未找到"));
    }

    #[test]
    fn delete_removes_existing_skill_with_confirm() {
        let db = test_db();
        insert_skill(&db, "SQL规范检查");

        let no_confirm = dispatch(&db, "delete", &json!({ "name": "SQL规范检查" }));
        assert!(!no_confirm.success, "缺少 confirm 必须拒绝");

        let confirmed = dispatch(
            &db,
            "delete",
            &json!({ "name": "SQL规范检查", "confirm": true }),
        );
        assert!(confirmed.success);
        let conn = db.0.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_custom_instructions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("skill_install_{tag}_{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// FREE_API_SKILLS_DIR 是进程级全局——并行测试互踩会让文件落到对方的
    /// 根目录。install 测试一律持此锁串行化（实测真踩到：root vs root2 竞争）。
    static SKILLS_DIR_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// install 全链路（本地源路径分支）：复制 → 探测 SKILL.md → 登记 DB 卡片。
    /// 实证驱动：opus-video-skills 这类多技能仓 install 后应逐技能登记，
    /// 且卡片正文带本地手册路径指路。
    #[test]
    fn install_local_dir_registers_skill_card_with_pointer() {
        let src = tmp_dir("src");
        let skill_dir = src.join("skills").join("watercolor_pack");
        std::fs::create_dir_all(skill_dir.join("template")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: 水彩动画技能\nwhen_to_use: 制作水彩手绘动画时\n---\n# 水彩动画\n必须用官方脚手架 new_project.sh",
        )
        .unwrap();
        std::fs::write(skill_dir.join("template").join("reel.js"), "// template").unwrap();
        let _env_guard = SKILLS_DIR_ENV_LOCK.lock().unwrap();
        let skills_root = tmp_dir("root");
        std::env::set_var("FREE_API_SKILLS_DIR", &skills_root);

        let db = test_db();
        let r = dispatch(&db, "install", &json!({ "url": src.to_str().unwrap() }));
        assert!(r.success, "install 应成功: {} {}", r.stdout, r.stderr);
        assert!(r.stdout.contains("watercolor_pack"), "输出: {}", r.stdout);

        // 资产真的复制过去了
        assert!(skills_root
            .join(src.file_name().unwrap())
            .join("skills/watercolor_pack/SKILL.md")
            .exists());

        // DB 卡片：kind=skill + 触发卡取 when_to_use + content 含本地路径
        let conn = db.0.lock().unwrap();
        let (kind, desc, content): (String, String, String) = conn
            .query_row(
                "SELECT kind, description, content FROM agent_custom_instructions WHERE name='watercolor_pack'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(kind, "skill");
        assert_eq!(desc, "制作水彩手绘动画时");
        assert!(content.contains("SKILL.md"), "content: {}", content);
        assert!(content.contains("new_project.sh"), "content: {}", content);
        drop(conn);

        // 重装 → 更新不重复（同名 upsert）
        let r2 = dispatch(&db, "install", &json!({ "url": src.to_str().unwrap() }));
        assert!(r2.success);
        let conn = db.0.lock().unwrap();
        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_custom_instructions WHERE name='watercolor_pack'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cnt, 1, "重复 install 不应产生重复卡片");
    }

    /// 无 SKILL.md 的仓库：返回结构清单不自动登记——歧义结构交模型判断。
    #[test]
    fn install_without_skill_md_returns_manifest_not_card() {
        let src = tmp_dir("noskill");
        std::fs::write(src.join("README.md"), "# just a lib").unwrap();
        let _env_guard = SKILLS_DIR_ENV_LOCK.lock().unwrap();
        let skills_root = tmp_dir("root2");
        std::env::set_var("FREE_API_SKILLS_DIR", &skills_root);

        let db = test_db();
        let r = dispatch(&db, "install", &json!({ "url": src.to_str().unwrap() }));
        assert!(r.success);
        assert!(r.stdout.contains("未发现 SKILL.md"), "输出: {}", r.stdout);
        assert!(r.stdout.contains("README.md"), "应回结构清单: {}", r.stdout);
        let conn = db.0.lock().unwrap();
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_custom_instructions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(cnt, 0, "无 SKILL.md 不应登记卡片");
    }
}
