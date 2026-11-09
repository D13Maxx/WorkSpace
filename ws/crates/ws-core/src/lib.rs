//! Turn orchestration: the layer that turns a user message into a persisted
//! reply (master spec s4).

pub mod agents;
pub mod edits;
pub mod error;
pub mod events;
pub mod profiles;
pub mod skills;
pub mod tools;
pub mod turn;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use agents::{
    Agents, PLAN_FILE, coordinator_instructions, coordinator_message, load_worker_fallbacks,
    load_worker_model, save_worker_fallbacks, save_worker_model,
};
pub use error::{CoreError, Result};
pub use events::TurnEvent;
pub use profiles::{Profile, ProfileTable};
pub use skills::{Skill, Skills};
pub use tools::{Isolated, ToolHost, ToolOutput, ToolSet};
pub use turn::{EditView, ModelListResult, Orchestrator, RECONNECT_WAITS};
