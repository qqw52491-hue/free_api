use super::*;
use std::future::Future;

// Inspect the real production future types without starting a UI or a model.
fn future_bytes<S, T: Future>(_: impl FnOnce(S) -> T) -> usize {
    std::mem::size_of::<T>()
}

#[test]
fn desktop_entry_and_shared_driver_future_sizes() {
    let turn = future_bytes(turn::turn_main);
    let driver = future_bytes(|s: Box<turn::MainExecution<'_, '_>>| {
        crate::agent::execution::run(s, turn::turn_main)
    });
    let entry = future_bytes(|s: turn::MainExecution<'_, '_>| {
        run_agent_main_loop(
            s.app,
            s.state,
            s.registry_state,
            s.model_routing,
            s.goal,
            s.auto_pilot,
            Some(s.final_session_id),
            s.log_context,
            Some(s.transient_llm_request_retries),
            None,
            s.plan_mode,
            None,
        )
    });
    eprintln!("desktop={entry}, driver={driver}, turn={turn}");
    assert!(
        entry < 16 * 1024,
        "desktop entry inlines large futures: {entry}"
    );
    assert!(
        driver < 1024,
        "driver inlines the main turn/state: {driver}"
    );
}
