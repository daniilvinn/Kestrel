//! Kestrel UI: theme tokens and the ratatui rendering of an [`AppState`](ks_core::AppState).

pub mod claude;
pub mod fileops;
pub mod format;
pub mod help;
mod hints;
pub mod icons;
pub mod images;
pub mod list;
pub mod navaids;
pub mod palette;
pub mod popup;
pub mod preview;
pub mod render;
mod scrollbar;
pub mod search;
pub mod syntax;
pub mod text;
pub mod theme;
pub mod which_key;

#[cfg(test)]
mod preview_tests;
#[cfg(test)]
mod testutil;
#[cfg(test)]
mod testutil_preview;

pub use render::{render, HINT_MIN_HEIGHT};
pub use theme::{ColorDepth, Theme, ThemeError, ThemeLoader};
