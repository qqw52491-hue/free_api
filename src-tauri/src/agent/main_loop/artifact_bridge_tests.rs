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

    /// 实弹病灶（session 2a9ffcd2 视频渲染 22 连拦）：会话 tmp 累积上万实体
    /// （node_modules + Chrome 剖面 + 帧序列），旧 1000 条上限按 readdir 先到
    /// 先得——垃圾路径耗尽预算后真目标 robot_editor.js 漏收 → 假零交集。
    /// 修复契约：洪泛目录不下钻，真实交付文件必须始终入集合。
    #[test]
    fn flood_dirs_never_starve_real_target_entries() {
        let session_tmp = std::env::temp_dir().join(format!("fac-flood-{}", std::process::id()));
        // 洪泛源 1：node_modules（散布多子目录 × 每目录多文件）
        for i in 0..30 {
            let d = session_tmp.join(format!("node_modules/pkg-{}/dist", i));
            std::fs::create_dir_all(&d).unwrap();
            for j in 0..80 {
                std::fs::write(d.join(format!("f{}.js", j)), "x").unwrap();
            }
        }
        // 洪泛源 2：渲染帧序列（单目录数百文件）
        let frames = session_tmp.join("anime_story/out/frames");
        std::fs::create_dir_all(&frames).unwrap();
        for i in 0..500 {
            std::fs::write(frames.join(format!("frame_{:04}.png", i)), "x").unwrap();
        }
        // 洪泛源 3：Chrome 临时剖面
        let chrome = session_tmp.join("com.google.Chrome.ab12CD/Sessions");
        std::fs::create_dir_all(&chrome).unwrap();
        for i in 0..50 {
            std::fs::write(chrome.join(format!("Tabs_{}", i)), "x").unwrap();
        }
        // 真目标：被洪泛包围的声明文件
        let scenes = session_tmp.join("anime_story/src/scenes");
        std::fs::create_dir_all(&scenes).unwrap();
        std::fs::write(scenes.join("robot_editor.js"), "scene").unwrap();
        std::fs::write(session_tmp.join("anime_magic_sprout.mp4"), "mp4").unwrap();

        let mut touched = Vec::new();
        extend_touched_with_session_tmp(&mut touched, &session_tmp);

        // 契约：声明目标永远在场——无论洪泛多凶
        assert!(
            touched
                .iter()
                .any(|p| file_paths_match("anime_story/src/scenes/robot_editor.js", p)),
            "真声明目标必须入对账集，touched.len()={}",
            touched.len()
        );
        assert!(touched.iter().any(|p| p == "tmp/anime_magic_sprout.mp4"));
        // node_modules 内部不应出现在集合（目录本体在、内容不下钻）
        assert!(
            !touched
                .iter()
                .any(|p| p.contains("node_modules/pkg-0/dist/f0.js")),
            "node_modules 内部文件不应污染对账集"
        );
        assert!(touched.iter().any(|p| p == "tmp/node_modules"));
        assert!(touched.len() < 4000);

        let _ = std::fs::remove_dir_all(&session_tmp);
    }
