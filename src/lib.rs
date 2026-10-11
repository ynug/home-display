//! Home Display — always-on home dashboard (Rust + Slint).
//!
//! Layering: `services` (network, parsing) → `repository` (latest good data +
//! status) → `view` (pure display models) → `ui_bridge` (Slint types) → `app`
//! (UI-thread controller, rotation via `screen::ScreenManager`).

pub mod app;
pub mod clock;
pub mod config;
pub mod diag;
pub mod fixtures;
pub mod model;
pub mod repository;
pub mod screen;
pub mod services;
pub mod status;
pub mod ui_bridge;
pub mod view;

#[allow(clippy::all)]
pub mod ui {
    slint::include_modules!();
}
