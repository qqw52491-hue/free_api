    use super::*;

    #[test]
    fn attachment_registration_needs_project_and_never_overwrites_project_files() {
        let base = std::env::temp_dir().join(format!("free-api-attach-{}", Uuid::new_v4()));
        let root = base.join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("report.txt"), "项目自带的同名文件，绝不能被附件覆盖").unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();

        let project = create_project_row(&conn, "附件项目", root.to_string_lossy().as_ref()).unwrap();
        let session = create_project_session_row(&conn, &project.id, "附件会话", "model-a").unwrap();
        let plain_session_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count) VALUES (?1, '普通会话', 'model-a', ?2, ?2, 0)",
            params![plain_session_id, Utc::now().to_rfc3339()],
        )
        .unwrap();

        let attachment = base.join("upload.bin");
        std::fs::write(&attachment, "attachment body").unwrap();

        // 非项目会话：不登记，返回 None
        let none = register_attachment_for_session(&conn, &plain_session_id, &attachment, "report.txt").unwrap();
        assert!(none.is_none());

        // 项目会话：登记成功，且附件被复制进隔离目录，绝不覆盖项目同名文件
        let resource_id = register_attachment_for_session(&conn, &session.id, &attachment, "report.txt")
            .unwrap()
            .expect("项目会话应登记出句柄");
        assert_eq!(std::fs::read_to_string(root.join("report.txt")).unwrap(), "项目自带的同名文件，绝不能被附件覆盖");
        let imported_dir = root.join(".attachments");
        assert!(imported_dir.exists());
        let preview = read_project_resource_for_session(&conn, &session.id, &resource_id).unwrap();
        assert!(preview.content.contains("attachment body"));
        assert_eq!(std::fs::read_dir(&imported_dir).unwrap().count(), 1);

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn project_resources_are_scoped_to_the_canonical_project_root_and_session() {
        let base = std::env::temp_dir().join(format!("free-api-projects-{}", Uuid::new_v4()));
        let root = base.join("project");
        let outside = base.join("outside.txt");
        let nested = root.join("docs").join("notes.md");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "project-scoped resource").unwrap();
        std::fs::write(&outside, "outside resource").unwrap();
        let db_path = base.join("test.db");
        let conn = crate::db::init_db(&db_path).unwrap();

        let project = create_project_row(&conn, "项目 A", root.to_string_lossy().as_ref()).unwrap();
        let session = create_project_session_row(&conn, &project.id, "会话 A", "model-a").unwrap();
        let resource =
            add_project_resource_row(&conn, &project.id, nested.to_string_lossy().as_ref())
                .unwrap();

        assert_eq!(resource.relative_path, "docs/notes.md");
        assert_eq!(
            project_for_session(&conn, &session.id).unwrap().unwrap().id,
            project.id
        );
        let imported =
            add_project_resource_row(&conn, &project.id, outside.to_string_lossy().as_ref())
                .unwrap();
        assert_eq!(imported.relative_path, "outside.txt");
        assert_eq!(
            project_resources_for_session(&conn, &session.id)
                .unwrap()
                .len(),
            2
        );
        // 直接按原始外部绝对路径做项目文件检索时，canonical_project_file 仍做安全越界拦截
        assert!(canonical_project_file(&conn, &project.id, outside.to_string_lossy().as_ref()).is_err());

        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn child_resource_excerpts_require_same_project_session_and_valid_ids() {
        let base = std::env::temp_dir().join(format!("free-api-project-excerpts-{}", Uuid::new_v4()));
        let root_a = base.join("project-a");
        let root_b = base.join("project-b");
        let resource_a = root_a.join("a.md");
        let resource_b = root_b.join("b.md");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        std::fs::write(&resource_a, "alpha resource").unwrap();
        std::fs::write(&resource_b, "bravo resource").unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let project_a = create_project_row(&conn, "项目 A", root_a.to_string_lossy().as_ref()).unwrap();
        let project_b = create_project_row(&conn, "项目 B", root_b.to_string_lossy().as_ref()).unwrap();
        let session_a = create_project_session_row(&conn, &project_a.id, "会话 A", "model-a").unwrap();
        let resource_a = add_project_resource_row(&conn, &project_a.id, resource_a.to_string_lossy().as_ref()).unwrap();
        let resource_b = add_project_resource_row(&conn, &project_b.id, resource_b.to_string_lossy().as_ref()).unwrap();

        let own = vec![resource_a.resource_id.clone()];
        assert_eq!(
            read_project_resource_excerpts_for_session(&conn, &session_a.id, &own, 100)
                .unwrap()
                .len(),
            1
        );
        assert!(read_project_resource_excerpts_for_session(
            &conn,
            &session_a.id,
            &[resource_b.resource_id.clone()],
            100,
        )
        .unwrap_err()
        .contains("不属于当前会话项目"));
        assert!(read_project_resource_excerpts_for_session(&conn, "detached-session", &own, 100)
            .unwrap_err()
            .contains("未关联项目"));
        assert!(read_project_resource_excerpts_for_session(
            &conn,
            &session_a.id,
            &[resource_a.resource_id.clone(), resource_a.resource_id.clone()],
            100,
        )
        .is_err());
        assert!(read_project_resource_excerpts_for_session(
            &conn,
            &session_a.id,
            &["one".to_string(), "two".to_string(), "three".to_string()],
            100,
        )
        .is_err());

        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn child_resource_excerpts_truncate_and_recanonicalize_every_read() {
        let base = std::env::temp_dir().join(format!("free-api-project-excerpt-budget-{}", Uuid::new_v4()));
        let root = base.join("project");
        let first_path = root.join("first.md");
        let second_path = root.join("second.md");
        let outside_path = base.join("outside.md");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&first_path, "abcdef").unwrap();
        std::fs::write(&second_path, "second resource").unwrap();
        std::fs::write(&outside_path, "outside resource").unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let project = create_project_row(&conn, "项目预算", root.to_string_lossy().as_ref()).unwrap();
        let session = create_project_session_row(&conn, &project.id, "预算会话", "model-a").unwrap();
        let first = add_project_resource_row(&conn, &project.id, first_path.to_string_lossy().as_ref()).unwrap();
        let second = add_project_resource_row(&conn, &project.id, second_path.to_string_lossy().as_ref()).unwrap();

        let single = read_project_resource_excerpts_for_session(
            &conn,
            &session.id,
            &[first.resource_id.clone()],
            4,
        )
        .unwrap();
        assert_eq!(single[0].content, "abcd");
        assert!(single[0].truncated);
        let multi = read_project_resource_excerpts_for_session(
            &conn,
            &session.id,
            &[first.resource_id.clone(), second.resource_id.clone()],
            4,
        )
        .unwrap();
        assert_eq!(multi.len(), 2);
        assert_eq!(multi[0].content, "abcd");
        assert!(multi[0].truncated);
        assert_eq!(multi[1].content, "");
        assert!(multi[1].truncated);

        #[cfg(unix)]
        {
            std::fs::remove_file(&first_path).unwrap();
            std::os::unix::fs::symlink(&outside_path, &first_path).unwrap();
            assert!(read_project_resource_excerpts_for_session(
                &conn,
                &session.id,
                &[first.resource_id],
                100,
            )
            .unwrap_err()
            .contains("项目根目录外"));
        }

        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn deleting_a_project_cascades_resources_and_session_mapping_but_keeps_chat_history() {
        let base = std::env::temp_dir().join(format!("free-api-project-delete-{}", Uuid::new_v4()));
        let root = base.join("project");
        let resource_path = root.join("readme.md");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&resource_path, "registered project resource").unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();

        let project = create_project_row(&conn, "项目 B", root.to_string_lossy().as_ref()).unwrap();
        let session = create_project_session_row(&conn, &project.id, "会话 B", "model-b").unwrap();
        add_project_resource_row(&conn, &project.id, resource_path.to_string_lossy().as_ref())
            .unwrap();

        conn.execute(
            "DELETE FROM agent_projects WHERE id=?1",
            params![project.id],
        )
        .unwrap();

        assert!(project_for_session(&conn, &session.id).unwrap().is_none());
        assert!(project_resources_for_session(&conn, &session.id)
            .unwrap()
            .is_empty());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM agent_project_resources", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM chat_sessions WHERE id=?1",
                params![session.id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );

        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn adding_project_resource_via_session_registers_cross_session_resource() {
        let base = std::env::temp_dir().join(format!("free-api-session-resource-{}", Uuid::new_v4()));
        let root = base.join("project");
        let spec_path = root.join("docs").join("database_v2_spec.md");
        std::fs::create_dir_all(spec_path.parent().unwrap()).unwrap();
        std::fs::write(&spec_path, "# Database V2 Spec\nTables: users, orders").unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();

        let project = create_project_row(&conn, "电商系统", root.to_string_lossy().as_ref()).unwrap();
        let session_a = create_project_session_row(&conn, &project.id, "设计会话 A", "model-a").unwrap();
        let session_b = create_project_session_row(&conn, &project.id, "开发会话 B", "model-b").unwrap();

        // 会话 A 调用 add_project_resource_for_session 注册新文件
        let registered = add_project_resource_for_session(
            &conn,
            &session_a.id,
            "docs/database_v2_spec.md",
            Some("V2数据库设计规范"),
        )
        .unwrap();

        assert_eq!(registered.relative_path, "docs/database_v2_spec.md");
        assert_eq!(registered.label, "V2数据库设计规范");

        // 会话 B 立刻能看到会话 A 注册的项目资源
        let resources_b = project_resources_for_session(&conn, &session_b.id).unwrap();
        assert_eq!(resources_b.len(), 1);
        assert_eq!(resources_b[0].resource_id, registered.resource_id);
        assert_eq!(resources_b[0].label, "V2数据库设计规范");

        // 会话 B 可以读取内容
        let preview = read_project_resource_for_session(&conn, &session_b.id, &registered.resource_id).unwrap();
        assert!(preview.content.contains("# Database V2 Spec"));

        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn inspecting_dragged_paths_distinguishes_dirs_and_files() {
        let base = std::env::temp_dir().join(format!("free-api-inspect-{}", Uuid::new_v4()));
        let dir = base.join("my-project");
        let file = base.join("data.csv");
        let nonexistent = base.join("ghost.txt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "col1,col2\n1,2\n").unwrap();

        let paths = vec![
            dir.to_string_lossy().to_string(),
            file.to_string_lossy().to_string(),
            nonexistent.to_string_lossy().to_string(),
        ];

        let results = inspect_dragged_paths(paths).await.unwrap();
        assert_eq!(results.len(), 3);

        // 目录判定
        assert!(results[0].is_dir);
        assert!(results[0].exists);
        assert_eq!(results[0].name, "my-project");

        // 文件判定
        assert!(!results[1].is_dir);
        assert!(results[1].exists);
        assert_eq!(results[1].name, "data.csv");
        assert!(results[1].size_bytes > 0);

        // 不存在的文件判定
        assert!(!results[2].exists);
        assert_eq!(results[2].name, "ghost.txt");

        let _ = std::fs::remove_dir_all(base);
    }
