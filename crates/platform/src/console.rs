//! Console output on Windows.
//!
//! Release builds use the "windows" subsystem so that starting the app from
//! the Start menu does not flash a console window. The price is that such a
//! program is not connected to the terminal it was started from, so
//! `better-basic-tts voices` would print nothing. Attaching to the parent's
//! console, if there is one, fixes that.

/// Connect stdout / stderr to the launching terminal, if any. No-op on other
/// platforms and when output is already redirected.
pub fn attach_to_parent() {
    #[cfg(windows)]
    {
        const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
        #[link(name = "kernel32")]
        extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
        }
        // Fails harmlessly when there is no parent console (started from the
        // shell) or we already have one (debug builds).
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}
