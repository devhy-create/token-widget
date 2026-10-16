//! Agent Monitor 数据引擎（由 Electron 版 `core/` 移植）。
//! 不依赖任何窗口 / UI，可被 Tauri 壳、命令行或测试直接调用。
pub mod antigravity;
pub mod collectors;
pub mod engine;
pub mod lines;
pub mod plans;
pub mod pricing;
pub mod sqlite;
pub mod statusline;
pub mod util;

pub use engine::{day_key, Engine, EngineOptions, ScanJob};
