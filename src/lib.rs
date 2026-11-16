//! Nudge — reminders and follow-ups for rune-tui.
//!
//! The plugin logic is plain Rust ([`app::Nudge`] implements
//! [`rune_plugin::Plugin`]), so it is tested natively with `cargo test`;
//! `export_plugin!` adds the WebAssembly entry points when built for
//! `wasm32-unknown-unknown`. See README.md for commands and settings.

pub mod app;
pub mod fmt;
pub mod model;
pub mod recur;
pub mod settings;
pub mod when;
pub mod zone;

pub use app::Nudge;

rune_plugin::export_plugin!(Nudge);
