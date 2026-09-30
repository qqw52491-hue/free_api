    use super::{build_touched_set, extend_touched_with_session_tmp, normalize_tmp_declared, tmp_target_near_misses};
    use crate::agent::finish_guard::{diff::file_paths_match, DiffContext};
    use std::path::PathBuf;

    /// 实弹病灶：PPT 产物任务声明 `tmp/SWE-2_Introduction.pptx`，产物物理落在
    /// 会话 tmp（工作区外）。桥接后 touched 集必须含 `tmp/<rel>` 形态，
    /// 否则零交集门会把"产物已产出"误判成"目标未动"→ finish 死循环。
    #[test]
    fn session_tmp_artifact_bridges_to_tmp_display_form() {
        let session_tmp = PathBuf::from("/data/free-api-chat/tmp/sid-1");
        let d = DiffContext::from_diff_and_paths(
            None,
            &[
                "/data/free-api-chat/tmp/sid-1/SWE-2_Introduction.pptx".to_string(),
                "/data/free-api-chat/tmp/sid-1".to_string(), // mkdir 裸目录 → "tmp/" 哨兵
            ],
        );
        let touched = build_touched_set(&d, &session_tmp);
        assert!(touched
            .iter()
            .any(|p| p == "tmp/SWE-2_Introduction.pptx"));
        // 声明侧两种形态都应对上：带 tmp/ 前缀与裸文件名
        assert!(file_paths_match(
            "tmp/SWE-2_Introduction.pptx",
            "tmp/SWE-2_Introduction.pptx"
        ));
        assert!(touched
            .iter()
            .any(|p| file_paths_match("SWE-2_Introduction.pptx", p)));
    }

    #[test]
    fn cross_session_tmp_is_not_bridged() {
        let session_tmp = PathBuf::from("/data/free-api-chat/tmp/sid-1");
        let d = DiffContext::from_diff_and_paths(
            None,
            &["/data/free-api-chat/tmp/sid-OTHER/evil.pptx".to_string()],
        );
        let touched = build_touched_set(&d, &session_tmp);
        // 跨会话 tmp 不脱 sid 归一 → 声明 tmp/evil.pptx 依然对不上（保守不误放）
        assert!(!touched.iter().any(|p| file_paths_match("tmp/evil.pptx", p)));
    }

    #[test]
    fn workspace_affected_paths_pass_through_unmodified() {
        let session_tmp = PathBuf::from("/data/free-api-chat/tmp/sid-1");
        let d = DiffContext::from_diff_and_paths(None, &["src/main.rs".to_string()]);
        let touched = build_touched_set(&d, &session_tmp);
        assert!(touched.iter().any(|p| p == "src/main.rs"));
    }

    /// 实弹病灶（2026-09 冒烟复测）：tmp/ 写入不入副作用台账也不在工作区
    /// diff 内 → 声明 tmp/x 的任务对账集结构性为空 → 零交集门误伤 finish。
    /// 目录实物必须桥接为 tmp/<rel> 声明形态参与对账。
    #[test]
    fn session_tmp_products_extend_touched_for_zero_intersection_gate() {
        let session_tmp = std::env::temp_dir().join(format!("fac-tmpgate-{}", std::process::id()));
        std::fs::create_dir_all(session_tmp.join("sub")).unwrap();
        std::fs::write(session_tmp.join("hello_smoke.md"), "# smoke").unwrap();
        std::fs::write(session_tmp.join("sub/report.md"), "r").unwrap();

        let mut touched = Vec::new();
        extend_touched_with_session_tmp(&mut touched, &session_tmp);
        assert!(touched.iter().any(|p| p == "tmp/hello_smoke.md"));
        assert!(touched.iter().any(|p| p == "tmp/sub/report.md"));

        // 声明形态归一：物理 session_tmp 声明 → tmp/<rel>
        let abs = session_tmp.join("hello_smoke.md");
        assert_eq!(
            normalize_tmp_declared(&abs.to_string_lossy(), &session_tmp),
            "tmp/hello_smoke.md"
        );
        assert_eq!(normalize_tmp_declared("tmp/x.md", &session_tmp), "tmp/x.md");
        assert_eq!(
            normalize_tmp_declared("src/main.rs", &session_tmp),
            "src/main.rs"
        );

        // 对账：声明 tmp/hello_smoke.md 命中实物
        let hit = touched.iter().any(|p| {
            crate::agent::finish_guard::diff::file_paths_match("tmp/hello_smoke.md", p)
        });
        assert!(hit, "声明 tmp/x 必须与会话 tmp 实物对账命中");

        let _ = std::fs::remove_dir_all(&session_tmp);
    }

    /// 实弹病灶（session 2a9ffcd2，13 连拦空转）：声明 tmp/p5js-test/sketch.html，
    /// 产物落在会话 tmp 根 tmp/sketch.html——同名但层级不符，零交集门正确拦截，
    /// 但旧文案只报工作区 diff（谎称"零改动"），模型永远归不到路径错位真因。
    /// 近失配点名必须同时满足：① basename 命中 ② 全路径不匹配（不误报真命中）。
    #[test]
    fn tmp_target_near_misses_names_subdir_mismatch() {
        let touched = vec![
            "tmp/sketch.html".to_string(),
            "tmp/preview.csv".to_string(),
            "src/main.rs".to_string(),
        ];
        // 声明子目录、产物在根 → 点名
        let misses = tmp_target_near_misses(
            &["tmp/p5js-test/sketch.html".to_string()],
            &touched,
        );
        assert_eq!(misses.len(), 1);
        assert!(misses[0].contains("tmp/p5js-test/sketch.html"));
        assert!(misses[0].contains("tmp/sketch.html"));

        // 声明与产物精确对上 → 不产生近失配（真命中不该误报）
        let exact = tmp_target_near_misses(&["tmp/sketch.html".to_string()], &touched);
        assert!(exact.is_empty());

        // 声明文件根本无同名产物 → 不点名（交给常规零交集文案）
        let no_basename = tmp_target_near_misses(
            &["tmp/p5js-test/other.html".to_string()],
            &touched,
        );
        assert!(no_basename.is_empty());

        // basename 大小写差异同样应点名
        let case = tmp_target_near_misses(
            &["tmp/out/SKETCH.HTML".to_string()],
            &touched,
        );
        assert_eq!(case.len(), 1);
    }
