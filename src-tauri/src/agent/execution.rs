//! Shared turn driver. Profiles own capabilities and handoff contracts; the driver
//! owns iteration. State is moved between turns so no two turns share mutable state.
use std::future::Future;
use std::ops::ControlFlow;

pub(crate) async fn run<S, B, E, F, T>(mut state: S, mut turn: F) -> Result<B, E>
where
    F: FnMut(S) -> T,
    T: Future<Output = Result<ControlFlow<B, S>, E>>,
{
    loop {
        // Keep the turn future out of the driver's inline async state. In debug
        // builds, moving a large turn/state through nested poll frames exhausted
        // the desktop runtime's 2 MiB worker stack before the first model call.
        match Box::pin(turn(state)).await? {
            ControlFlow::Continue(next) => state = next,
            ControlFlow::Break(result) => return Ok(result),
        }
    }
}

pub(crate) mod actions;
pub(crate) mod runtime;

pub(crate) mod protocol;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod stack_tests;
