//! AgentSOP 内核：SOP 定义、状态机、事件存储、上下文渲染。
//!
//! 该 crate 不依赖 Tauri / HTTP，可在无界面环境下单测与回放。

pub mod context;
pub mod error;
pub mod kernel;
pub mod model;
pub mod sessions;
pub mod skills;
pub mod sop;
pub mod store;

pub use error::{AppError, Result};
pub use kernel::{Kernel, HookOutcome};
pub use model::*;
pub use sop::{DEFAULT_SOP_NAME, DEFAULT_SOP_YAML};
pub use store::Paths;
