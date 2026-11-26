//! OS integration that does not depend on the UI toolkit.
//!
//! * [`hotkeys`] — system-wide keyboard shortcuts
//! * [`selection`] — "what text does the user currently have selected?"
//! * [`ipc`] — single-instance handling and remote control from the CLI

pub mod hotkeys;
pub mod ipc;
pub mod selection;
