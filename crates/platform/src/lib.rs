//! OS integration that does not depend on the UI toolkit.
//!
//! * [`hotkeys`] — system-wide keyboard shortcuts
//! * [`selection`] — "what text does the user currently have selected?"
//! * [`ipc`] — single-instance handling and remote control from the CLI
//! * [`window`] — keeping the control panel above other windows
//! * [`console`] — command line output for a windowed Windows program

pub mod console;
pub mod hotkeys;
pub mod ipc;
pub mod selection;
pub mod window;
