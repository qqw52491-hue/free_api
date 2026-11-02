use super::*;
use crate::agent::{
    agent_mailbox::{self, Actor},
    checkpoint::{load_checkpoint, save_checkpoint, AgentCheckpoint},
    context::SandwichContext,
};
use crate::db::DbState;
use serde_json::json;
use std::sync::{Arc, Mutex};

struct Frame {
    db: DbState,
    actor: Actor,
    checkpoint: AgentCheckpoint,
    context: SandwichContext,
    turns: usize,
    received: usize,
}

async fn turn(mut frame: Box<Frame>) -> Result<ControlFlow<Box<Frame>, Box<Frame>>, String> {
    frame.received += agent_mailbox::deliver_main(
        &frame.db,
        &frame.actor,
        &mut frame.context,
        &mut frame.checkpoint,
    )?;
    frame.turns += 1;
    tokio::task::yield_now().await;
    Ok(if frame.turns == 3 {
        ControlFlow::Break(frame)
    } else {
        ControlFlow::Continue(frame)
    })
}

#[test]
fn default_worker_stack_receives_once_and_preserves_checkpoint_across_shared_turns() {
    let root = std::env::temp_dir().join(format!("execution-stack-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let db = DbState(Arc::new(Mutex::new(
        crate::db::init_db(&root.join("test.db")).unwrap(),
    )));
    let actor = Actor::new(
        &uuid::Uuid::new_v4().to_string(),
        &uuid::Uuid::new_v4().to_string(),
    );
    crate::agent::conversation::begin_turn(&db, &actor.session, "mock", "greeting").unwrap();
    crate::agent::run_trace::upsert_snapshot(
        &db,
        &actor.run,
        &actor.session,
        "greeting",
        "running",
        &[],
        None,
    )
    .unwrap();
    let context = SandwichContext::new("system".into(), "greeting".into());
    let mut checkpoint = AgentCheckpoint::new(actor.session.clone(), &context);
    save_checkpoint(&db, &mut checkpoint).unwrap();
    agent_mailbox::send(
        &db,
        &actor,
        &json!({"target":actor.run,"message":"retained material"}),
        Some("one"),
        false,
    )
    .unwrap();
    let frame = Box::new(Frame {
        db,
        actor,
        context,
        checkpoint,
        turns: 0,
        received: 0,
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_stack_size(2 * 1024 * 1024)
        .enable_all()
        .build()
        .unwrap();
    // spawn executes on the 2 MiB worker, rather than block_on's calling thread.
    let frame = runtime.block_on(async { tokio::spawn(run(frame, turn)).await.unwrap().unwrap() });
    assert_eq!(frame.turns, 3);
    assert_eq!(frame.received, 1);
    let saved = load_checkpoint(&frame.db, &frame.actor.session)
        .unwrap()
        .unwrap();
    assert!(serde_json::to_string(&saved)
        .unwrap()
        .contains("retained material"));
    assert_eq!(saved.version, frame.checkpoint.version);
    drop(frame);
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}
