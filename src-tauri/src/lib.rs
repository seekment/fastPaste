//! fastPaste library root: storage + index + monitor are platform-agnostic and
//! unit-testable on any OS (capture itself is platform-gated).
pub mod clipboard;
#[cfg(feature = "app")]
pub mod commands;
pub mod config;
pub mod index;
pub mod models;
pub mod monitor;
pub mod ocr;
pub mod paste;
pub mod sensitive;
pub mod store;
pub mod tokenizer;

pub use models::*;
