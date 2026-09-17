    use super::{build_path_scope_manifest, resolve_workspace_dir};
    use std::path::PathBuf;

    /// 自由会话（无 override 无项目）的物理工作区必须回落到会话产物目录，
    /// 绝不许落进程 cwd——cwd 会被 path_guard 双白名单整体放行，模型一探
    /// 就把宿主目录报成"已绑定工作区"。
    #[test]
    fn free_session_workspace_falls_back_to_session_tmp_not_cwd() {
        let sid = format!("ws_fallback_{}", uuid::Uuid::new_v4());
        let ws = resolve_workspace_dir(None, None, &sid);
        let expected = crate::agent::path_guard::get_session_tmp_dir(&sid);
        assert_eq!(ws, expected, "自由会话工作区应为会话产物目录");
        assert_ne!(
            ws,
            std::env::current_dir().unwrap_or_default(),
            "自由会话工作区不得为进程 cwd"
        );
        assert!(ws.is_dir(), "回落目录必须已落盘（canonicalize 依赖存在性）");
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// 显式 override 与项目绑定优先于自由回落，行为不回归。
    #[test]
    fn explicit_workspace_and_override_win_over_fallback() {
        let sid = format!("ws_explicit_{}", uuid::Uuid::new_v4());
        let override_dir = std::env::temp_dir().join(format!("ovr_{}", uuid::Uuid::new_v4()));
        let project_dir = std::env::temp_dir().join(format!("prj_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&override_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        assert_eq!(
            resolve_workspace_dir(Some(override_dir.clone()), Some(project_dir.clone()), &sid),
            override_dir,
            "override 应优先于项目绑定"
        );
        assert_eq!(
            resolve_workspace_dir(None, Some(project_dir.clone()), &sid),
            project_dir,
            "项目绑定应优先于会话 tmp 回落"
        );
        let _ = std::fs::remove_dir_all(&override_dir);
        let _ = std::fs::remove_dir_all(&project_dir);
    }

    /// 文案-机制一致性护栏：作用域卡片必须覆盖 path_guard 的 tmp/ 路由、
    /// artifact_manager 的登记入口、finish 门禁的 tmp/ 直认+禁拷规则。
    /// 上次漂移（文案漏写 tmp/ 直认）导致模型为迎合门禁把产物拷进工作区。
    #[test]
    fn scope_card_covers_tmp_route_register_and_no_copy_rule() {
        let ws = PathBuf::from("/proj/ws");
        let tmp = PathBuf::from("/data/free-api-chat/tmp/sid-1");
        for has_ws in [true, false] {
            let card = build_path_scope_manifest(&ws, &tmp, has_ws);
            for anchor in [
                "tmp/filename.ext",
                "register_file",
                "无需也不许",
                "tmp/",
            ] {
                assert!(
                    card.contains(anchor),
                    "scope card (has_ws={has_ws}) 缺失锚点: {anchor}"
                );
            }
        }
    }

    /// 自由会话卡片必须钉死"未绑定工作区 + tmp 是暂存区非工作区"的语义——
    /// 防模型 pwd 探到会话目录后将其报成"已绑定工作区/工作目录"（实测复现）。
    #[test]
    fn free_session_card_distinguishes_scratch_dir_from_workspace() {
        let tmp = PathBuf::from("/data/free-api-chat/tmp/sid-2");
        let card = build_path_scope_manifest(&tmp, &tmp, false);
        for anchor in ["项目工作区：无（未绑定）", "非工作区", "会话专属临时目录"] {
            assert!(card.contains(anchor), "自由会话卡片缺失语义锚点: {anchor}");
        }
    }
