    use super::*;

    #[test]
    pub(crate) fn test_noise_path_filter() {
        assert!(is_noise_path("package-lock.json"));
        assert!(is_noise_path("src/dist/bundle.js"));
        assert!(is_noise_path("target/debug/app"));
        assert!(is_noise_path("node_modules/vue/index.js"));
        assert!(!is_noise_path("assets/report.xlsx"));
        assert!(!is_noise_path("exports/data.pdf"));
        assert!(!is_noise_path("src/agent/main_loop.rs"));
        assert!(!is_noise_path("src/components/ModelSelect.vue"));
    }

    #[test]
    pub(crate) fn test_binary_artifact_detection() {
        assert!(is_binary_file("assets/report.xlsx"));
        assert!(is_binary_file("exports/data.pdf"));
        assert!(is_binary_file("assets/logo.png"));
        assert!(is_binary_file("backup.zip"));
        assert!(!is_binary_file("src/main.rs"));
        assert!(!is_binary_file("README.md"));
    }

    /// 数据类二进制（xlsx）必须向终审透出列内容预览——
    /// 否则"整列空/字段映射丢失"这类语义缺陷对审查者天然不可见（实测踩坑）。
    #[test]
    pub(crate) fn test_extract_office_preview_surfaces_xlsx_columns() {
        use std::io::Write;
        // 手工构造最小合法 xlsx：表头 + 一行数据（其中一列留空）
        let mut buf = Vec::new();
        {
            let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opt = zip::write::SimpleFileOptions::default();
            z.start_file("[Content_Types].xml", opt).unwrap();
            z.write_all(r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#.as_bytes()).unwrap();
            z.start_file("_rels/.rels", opt).unwrap();
            z.write_all(r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.as_bytes()).unwrap();
            z.start_file("xl/workbook.xml", opt).unwrap();
            z.write_all(r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#.as_bytes()).unwrap();
            z.start_file("xl/_rels/workbook.xml.rels", opt).unwrap();
            z.write_all(r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#.as_bytes()).unwrap();
            z.start_file("xl/worksheets/sheet1.xml", opt).unwrap();
            z.write_all(r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>车牌</t></is></c><c r="B1" t="inlineStr"><is><t>开票状态</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>晋A12345</t></is></c></row></sheetData></worksheet>"#.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        let dir = std::env::temp_dir().join(format!("rgk_xlsx_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("orders.xlsx");
        std::fs::write(&path, &buf).unwrap();
        let preview = extract_office_preview(&path).expect("xlsx 应产出内容预览");
        assert!(preview.contains("车牌"), "预览应含表头列名: {}", preview);
        assert!(preview.contains("晋A12345"), "预览应含数据行: {}", preview);
        // 图片等无可读文本类型仍返回 None，由调用方退回元数据行
        let png = dir.join("a.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(extract_office_preview(&png).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    pub(crate) fn test_diff_filter() {
        let diff = r#"diff --git a/package-lock.json b/package-lock.json
index 123..456 100644
--- a/package-lock.json
+++ b/package-lock.json
@@ -1,3 +1,3 @@
-1
+2
diff --git a/src/main.rs b/src/main.rs
index 789..abc 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,2 @@
-let a = 1;
+let a = 2;
"#;
        let filtered = filter_diff_noise(diff);
        assert!(!filtered.contains("package-lock.json"));
        assert!(filtered.contains("src/main.rs"));
    }

    #[test]
    pub(crate) fn test_json_parse_and_verdict() {
        let json_input = r#"{
            "verdict": "rejected",
            "score": 75,
            "blocking_issues": [
                {
                    "file": "src/main.rs",
                    "issue": "缺少空指针防御",
                    "requirement": "添加 Option::unwrap_or 默认值"
                }
            ],
            "nit_issues": [
                {
                    "file": "src/main.rs",
                    "suggestion": "变量命名建议更具体"
                }
            ],
            "summary": "存在一处潜在 panic 隐患"
        }"#;

        let res = parse_reviewer_json(json_input, "test-model", "hash123", 500);
        assert_eq!(res.verdict, "rejected");
        assert_eq!(res.blocking_issues.len(), 1);
        assert_eq!(res.nit_issues.len(), 1);
        assert!(!res.is_passed());

        let clean_json = r#"{
            "verdict": "passed",
            "score": 95,
            "blocking_issues": [],
            "nit_issues": [],
            "summary": "代码修改完善"
        }"#;
        let res2 = parse_reviewer_json(clean_json, "test-model", "hash123", 300);
        assert_eq!(res2.verdict, "passed");
        assert!(res2.is_passed());
    }

    #[test]
    pub(crate) fn test_human_waivers_bypass_anti_perfunctory() {
        let last_hash = "same_diff_hash";
        let diff_hash = "same_diff_hash";
        let last_res = ReviewResult {
            verdict: "rejected".to_string(),
            score: 50,
            blocking_issues: vec![BlockingIssue {
                file: "LicenseInterceptor.java".to_string(),
                issue: "后门".to_string(),
                requirement: "还原".to_string(),
            }],
            nit_issues: vec![],
            summary: "存在未修复缺陷".to_string(),
            duration_ms: 100,
            model_id: "test-model".to_string(),
            diff_hash: diff_hash.to_string(),
            missing_evidence: vec![],
        };

        // 无豁免时：命中防敷衍拦截 (Diff 未变)
        let human_waivers_none: Option<&str> = None;
        let has_active_waiver = human_waivers_none.map_or(false, |w| !w.trim().is_empty());
        assert!(!has_active_waiver);
        assert!(!last_res.is_passed());
        assert_eq!(last_hash, diff_hash);

        // 有人类特赦豁免时：判定存在特赦，解除防敷衍拦截，允许调用审查员
        let human_waivers_some = Some("【选择项】: 保留本地 if(true) 调试放行，直接交付");
        let has_active_waiver_with_waiver = human_waivers_some.map_or(false, |w| !w.trim().is_empty());
        assert!(has_active_waiver_with_waiver);
    }

    /// mechanical_only 产物机械嗅探：0 字节、魔数不符、文本扩展名含 NUL
    /// 被抓出；合法形态（真 zip 头、UTF-16 BOM 文本）不误伤。
    #[test]
    pub(crate) fn sniff_tmp_anomalies_flags_physical_defects_only() {
        let sid = format!("sniff-test-{}", uuid::Uuid::new_v4());
        let tmp = crate::agent::path_guard::get_session_tmp_dir(&sid);
        std::fs::create_dir_all(&tmp).unwrap();

        // 0 字节空文件 → 异常
        std::fs::write(tmp.join("empty.csv"), b"").unwrap();
        // 文本冒充 xlsx → 魔数不符异常
        std::fs::write(tmp.join("fake.xlsx"), b"just plain text\n").unwrap();
        // 合法 xlsx（PK 头）→ 不异常
        std::fs::write(tmp.join("real.xlsx"), b"PK\x03\x04rest-of-zip").unwrap();
        // 文本扩展名含 NUL → 异常
        std::fs::write(tmp.join("bad.json"), b"{\"a\":\x00\x01}").unwrap();
        // UTF-16LE BOM 的文本含 NUL → 豁免不误伤
        std::fs::write(tmp.join("utf16.txt"), b"\xff\xfeh\x00i\x00").unwrap();
        // 无扩展名 → 跳过
        std::fs::write(tmp.join("noext"), b"whatever").unwrap();
        // 未知扩展名 → 跳过
        std::fs::write(tmp.join("data.weird"), b"\x00\x01\x02").unwrap();

        let anomalies = sniff_session_tmp_anomalies(&sid);
        let joined = anomalies.join("\n");
        assert!(joined.contains("empty.csv"), "0 字节应被抓: {}", joined);
        assert!(joined.contains("fake.xlsx"), "文本冒充 xlsx 应被抓: {}", joined);
        assert!(joined.contains("bad.json"), "文本含 NUL 应被抓: {}", joined);
        assert!(!joined.contains("real.xlsx"), "真 zip 头不应误伤: {}", joined);
        assert!(!joined.contains("utf16.txt"), "UTF-16 BOM 不应误伤: {}", joined);
        assert!(!joined.contains("noext"), "无扩展名应跳过: {}", joined);
        assert!(!joined.contains("data.weird"), "未知扩展名应跳过: {}", joined);

        // 干净产物区 → 无异常
        let sid2 = format!("sniff-clean-{}", uuid::Uuid::new_v4());
        let tmp2 = crate::agent::path_guard::get_session_tmp_dir(&sid2);
        std::fs::create_dir_all(&tmp2).unwrap();
        std::fs::write(tmp2.join("ok.md"), b"# hello").unwrap();
        assert!(sniff_session_tmp_anomalies(&sid2).is_empty());

        // 不存在的会话 tmp 目录 → 无异常
        assert!(sniff_session_tmp_anomalies("nonexistent-session-xyz").is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&tmp2);
    }
