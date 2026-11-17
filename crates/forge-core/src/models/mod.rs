//! How a session's models are chosen, beyond the model the user picked:
//!
//! - [`choice`] — a subagent started without a tier or a model gets one
//!   from the session owner's `SubagentModelAdvisor`, with the reason;
//! - [`auto`] — with **Auto** on, each main-session turn's model and effort
//!   are chosen, and stepped up and down a ladder (`AutoController`);
//! - [`state`] — a session's task kind, advisors and Auto kept together
//!   (`SessionModels`), and the services its owner installs
//!   (`ModelServices`).
//!
//! Model tiers (`crate::tiers`) and the selection itself
//! (`crate::model_selection`) are their own modules.

pub(crate) mod auto;
pub(crate) mod choice;
pub(crate) mod state;

#[cfg(test)]
mod auto_tests;
