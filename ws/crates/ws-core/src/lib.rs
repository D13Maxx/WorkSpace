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

pub use agents::Agents;
pub use error::{CoreError, Result};
pub use events::TurnEvent;
pub use profiles::{Profile, ProfileTable};
pub use skills::{Skill, Skills};
pub use tools::{ToolHost, ToolOutput, ToolSet};
pub use turn::{EditView, ModelListResult, Orchestrator};
