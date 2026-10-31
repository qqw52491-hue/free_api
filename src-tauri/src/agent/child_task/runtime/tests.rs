use super::*;

#[tokio::test]
async fn cancellation_before_channel_registration_survives_and_parent_waits_for_exit() {
    let token = uuid::Uuid::new_v4().to_string();
    let binding = TaskBinding {
        parent_session: format!("p-{token}"),
        parent_run: format!("r-{token}"),
        child_session: format!("task-{token}"),
        child_run: token.clone(),
    };
    let (done_tx, done) = watch::channel(false);
    let (cancel, cancel_rx) = watch::channel(false);
    RUNS.lock().unwrap().insert(
        token.clone(),
        Running {
            binding: binding.clone(),
            done,
            cancel,
            paused: Arc::new(Mutex::new(None)),
        },
    );
    request_cancel(&token).unwrap();
    let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let checked = exited.clone();
    let child = binding.child_session.clone();
    let future = Box::pin(async move {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        crate::agent::control::intervention_channels()
            .lock()
            .await
            .insert(child.clone(), tx);
        assert!(matches!(
            rx.recv().await,
            Some(crate::agent::control::HumanControl::Cancel)
        ));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        checked.store(true, std::sync::atomic::Ordering::SeqCst);
        crate::agent::control::intervention_channels()
            .lock()
            .await
            .remove(&child);
        Ok(())
    });
    let session = binding.child_session.clone();
    let id = token.clone();
    let core = tokio::spawn(async move {
        let result = supervise(future, &session, cancel_rx).await;
        done_tx.send_replace(true);
        RUNS.lock().unwrap().remove(&id);
        result
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        stop_children(&binding.parent_run),
    )
    .await
    .unwrap();
    assert!(exited.load(std::sync::atomic::Ordering::SeqCst));
    assert!(core.await.unwrap().is_ok());
    assert!(!attached(&token));
}

#[tokio::test]
async fn panicking_executor_is_an_error_and_not_a_completed_goal() {
    let (_cancel, rx) = watch::channel(false);
    let error = supervise(
        Box::pin(async {
            panic!("fixture executor failure");
            #[allow(unreachable_code)]
            Ok(())
        }),
        "panic-fixture",
        rx,
    )
    .await
    .unwrap_err();
    assert!(error.contains("子执行器已退出"));
}

#[tokio::test(start_paused = true)]
async fn human_wait_does_not_spend_model_rounds_at_the_normal_wait_deadline_and_remains_cancellable(
) {
    let root = std::env::temp_dir().join(format!("mail-human-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let db = DbState(Arc::new(Mutex::new(
        crate::db::init_db(&root.join("test.db")).unwrap(),
    )));
    let parent = crate::agent::agent_mailbox::Actor::new(
        &uuid::Uuid::new_v4().to_string(),
        &uuid::Uuid::new_v4().to_string(),
    );
    let id = crate::agent::child_reviewer::begin_child_run(
        &db,
        &parent.run,
        &parent.session,
        &root,
        "mock",
        0,
        "task",
        None,
    )
    .unwrap();
    let binding = TaskBinding {
        parent_session: parent.session.clone(),
        parent_run: parent.run.clone(),
        child_session: format!("task-{id}"),
        child_run: id.clone(),
    };
    let (_done_tx, done) = watch::channel(false);
    let (stop, _stop_rx) = watch::channel(false);
    RUNS.lock().unwrap().insert(
        id.clone(),
        Running {
            binding,
            done,
            cancel: stop,
            paused: Arc::new(Mutex::new(Some(json!({"message":"真实的待审批请求"})))),
        },
    );
    let (cancel, mut rx) = watch::channel(0);
    let query_db = db.clone();
    let query_id = id.clone();
    let waiter = tokio::spawn(async move {
        crate::agent::agent_mailbox::wait(&query_db, &parent, Some(&query_id), 1, &mut rx).await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(600)).await;
    assert!(!waiter.is_finished());
    cancel.send_replace(1);
    assert!(waiter.await.unwrap().is_err());
    RUNS.lock().unwrap().remove(&id);
    let _ = std::fs::remove_dir_all(root);
}
