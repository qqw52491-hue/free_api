//! Host-owned task material assembly. No model calls and no authorization here.
//! Runtime grants remain authoritative; renderers only describe them.
mod budget;
mod dispatch;
mod review;
pub(crate) mod visibility;
pub(crate) use budget::section;
pub(crate) use dispatch::ordinary;
pub(crate) use review::{review_entry, review_goal, review_system, stage};
#[cfg(test)]
mod tests;
