//! Reading the text the user has selected in whatever application is in
//! front.
//!
//! | platform | how |
//! |----------|-----|
//! | Linux (X11 / Wayland) | the *primary selection*: selected text is already available, nothing is simulated |
//! | macOS | the Accessibility API (`AXSelectedText`); if the app does not expose it, a simulated ⌘C |
//! | Windows | a simulated Ctrl+C |
//!
//! The simulated copy saves and restores the textual clipboard content.
//!
//! Capturing is split in up to three steps so that nothing slow runs on the
//! UI thread and nothing that must run on the main thread runs elsewhere:
//!
//! 1. [`capture_selection`] — any thread; may block briefly. Either yields
//!    the text or says that a copy has to be simulated.
//! 2. [`CopyRequest::press`] — **main thread** (macOS resolves key codes
//!    through an API that insists on it); returns quickly.
//! 3. [`CopyInFlight::finish`] — any thread; waits for the clipboard.

use anyhow::{anyhow, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub text: String,
    /// Name of the application the text came from, if known.
    pub source: Option<String>,
}

/// Outcome of the first capture step.
#[derive(Debug)]
pub enum Capture {
    Done(Captured),
    /// The selection can only be obtained by simulating the copy shortcut.
    NeedsCopy(CopyRequest),
}

/// A pending "press copy in the frontmost application".
#[derive(Debug)]
pub struct CopyRequest {
    source: Option<String>,
}

/// A copy that has been triggered; the clipboard has not been read yet.
#[derive(Debug)]
pub struct CopyInFlight {
    source: Option<String>,
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    saved: Option<String>,
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    marker: String,
}

fn non_empty(text: String, source: Option<String>) -> Result<Captured> {
    if text.trim().is_empty() {
        return Err(anyhow!("No text is selected."));
    }
    Ok(Captured { text, source })
}

/// Step 1: the text currently selected in the frontmost application, or a
/// request to simulate a copy. Never simulates input itself.
pub fn capture_selection() -> Result<Capture> {
    let source = frontmost_app();
    match platform::selected_text()? {
        Some(text) => non_empty(text, source).map(Capture::Done),
        None => Ok(Capture::NeedsCopy(CopyRequest { source })),
    }
}

impl CopyRequest {
    /// Step 2: press the copy shortcut. Call on the main thread.
    pub fn press(self) -> Result<CopyInFlight> {
        let (saved, marker) = platform::press_copy()?;
        Ok(CopyInFlight {
            source: self.source,
            saved,
            marker,
        })
    }
}

impl CopyInFlight {
    /// Step 3: wait for the copied text and restore the clipboard.
    pub fn finish(self) -> Result<Captured> {
        let text = platform::finish_copy(self.saved, &self.marker)?;
        non_empty(text, self.source)
    }
}

/// The text on the clipboard.
pub fn clipboard_text() -> Result<String> {
    let text = platform::clipboard_text()?;
    if text.trim().is_empty() {
        return Err(anyhow!("The clipboard has no text."));
    }
    Ok(text)
}

/// Name of the application that currently has focus.
pub fn frontmost_app() -> Option<String> {
    #[cfg(windows)]
    let name = active_win_pos_rs::get_active_window()
        .ok()
        .map(|w| w.app_name.trim().trim_end_matches(".exe").to_string());
    #[cfg(target_os = "macos")]
    let name = accessibility::focused_app_name();
    #[cfg(not(any(target_os = "macos", windows)))]
    let name: Option<String> = None;

    name.filter(|n| !n.is_empty() && !n.to_ascii_lowercase().contains("better-basic-tts"))
}

// ---------------------------------------------------------------------------
// Linux and other Unix desktops
// ---------------------------------------------------------------------------

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use anyhow::{anyhow, Context, Result};
    use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind};
    use std::process::{Command, Stdio};

    fn read(kind: LinuxClipboardKind) -> Result<String> {
        let mut clipboard = Clipboard::new().context("could not access the clipboard")?;
        match clipboard.get().clipboard(kind).text() {
            Ok(text) => Ok(text),
            Err(arboard::Error::ContentNotAvailable) => Ok(String::new()),
            Err(e) => Err(anyhow!("could not read the clipboard: {e}")),
        }
    }

    /// Fallback for Wayland compositors without the data-control protocol.
    fn wl_paste(primary: bool) -> Option<String> {
        let program = which::which("wl-paste").ok()?;
        let mut cmd = Command::new(program);
        cmd.arg("--no-newline");
        if primary {
            cmd.arg("--primary");
        }
        let out = cmd
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn wayland() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
    }

    /// The primary selection always answers; a copy is never needed.
    pub fn selected_text() -> Result<Option<String>> {
        match read(LinuxClipboardKind::Primary) {
            Ok(text) if !text.is_empty() => Ok(Some(text)),
            other => {
                if wayland() {
                    if let Some(text) = wl_paste(true) {
                        return Ok(Some(text));
                    }
                }
                other.map(Some)
            }
        }
    }

    pub fn press_copy() -> Result<(Option<String>, String)> {
        Err(anyhow!("simulated copy is not used on this platform"))
    }

    pub fn finish_copy(_saved: Option<String>, _marker: &str) -> Result<String> {
        Err(anyhow!("simulated copy is not used on this platform"))
    }

    pub fn clipboard_text() -> Result<String> {
        match read(LinuxClipboardKind::Clipboard) {
            Ok(text) if !text.is_empty() => Ok(text),
            other => {
                if wayland() {
                    if let Some(text) = wl_paste(false) {
                        return Ok(text);
                    }
                }
                other
            }
        }
    }
}

// ---------------------------------------------------------------------------
// macOS and Windows: simulated copy (plus Accessibility on macOS)
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "macos", windows))]
mod platform {
    use anyhow::{anyhow, Context, Result};
    use arboard::Clipboard;
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    use std::time::{Duration, Instant};

    pub fn clipboard_text() -> Result<String> {
        let mut clipboard = Clipboard::new().context("could not access the clipboard")?;
        match clipboard.get_text() {
            Ok(text) => Ok(text),
            Err(arboard::Error::ContentNotAvailable) => Ok(String::new()),
            Err(e) => Err(anyhow!("could not read the clipboard: {e}")),
        }
    }

    /// `Some(text)` if the selection could be read without side effects.
    pub fn selected_text() -> Result<Option<String>> {
        #[cfg(target_os = "macos")]
        if let Some(text) = super::accessibility::selected_text() {
            if !text.trim().is_empty() {
                return Ok(Some(text));
            }
        }
        Ok(None)
    }

    /// Remember the clipboard, mark it, and press the platform's copy
    /// shortcut in the frontmost application.
    pub fn press_copy() -> Result<(Option<String>, String)> {
        let mut clipboard = Clipboard::new().context("could not access the clipboard")?;
        let saved = clipboard.get_text().ok();

        // A marker lets us tell "nothing was selected" from "the selection
        // equals what was on the clipboard already".
        let marker = format!("\u{200b}bbtts-{}\u{200b}", std::process::id());
        clipboard
            .set_text(marker.clone())
            .context("could not prepare the clipboard")?;

        if let Err(e) = send_copy_keys() {
            restore(&mut clipboard, saved);
            return Err(e);
        }
        Ok((saved, marker))
    }

    /// Wait for the copy to land, then put the old clipboard text back.
    pub fn finish_copy(saved: Option<String>, marker: &str) -> Result<String> {
        let mut clipboard = Clipboard::new().context("could not access the clipboard")?;
        let mut copied = None;
        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            match clipboard.get_text() {
                Ok(text) if text != marker => {
                    copied = Some(text);
                    break;
                }
                _ => {}
            }
        }
        restore(&mut clipboard, saved);
        Ok(copied.unwrap_or_default())
    }

    fn restore(clipboard: &mut Clipboard, saved: Option<String>) {
        match saved {
            Some(text) => {
                let _ = clipboard.set_text(text);
            }
            None => {
                let _ = clipboard.clear();
            }
        }
    }

    fn send_copy_keys() -> Result<()> {
        let mut enigo = Enigo::new(&Settings::default()).map_err(|e| {
            if cfg!(target_os = "macos") {
                anyhow!(
                    "Reading the selection needs the Accessibility permission \
                     (System Settings → Privacy & Security → Accessibility). ({e})"
                )
            } else {
                anyhow!("could not simulate the copy shortcut: {e}")
            }
        })?;
        let command = if cfg!(target_os = "macos") {
            Key::Meta
        } else {
            Key::Control
        };
        // The user is most likely still holding the modifiers of the global
        // shortcut; let go of them so the application sees a clean copy.
        for key in [Key::Alt, Key::Shift, Key::Control, Key::Meta] {
            let _ = enigo.key(key, Direction::Release);
        }
        std::thread::sleep(Duration::from_millis(30));
        let result = enigo
            .key(command, Direction::Press)
            .and_then(|_| enigo.key(Key::Unicode('c'), Direction::Click));
        let _ = enigo.key(command, Direction::Release);
        result.map_err(|e| anyhow!("could not simulate the copy shortcut: {e}"))
    }
}

#[cfg(target_os = "macos")]
mod accessibility {
    //! `AXSelectedText` of the focused UI element. Works in native text views
    //! (TextEdit, Safari, Notes, Xcode, ...) without touching the clipboard.

    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringRef};
    use std::ffi::c_void;
    use std::ptr;

    type AXUIElementRef = *const c_void;
    type AXError = i32;
    const AX_SUCCESS: AXError = 0;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
        fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
    }

    /// Copy an attribute of an accessibility element. The caller owns the
    /// returned object (wrapped so it is released on drop).
    unsafe fn copy_attribute(element: AXUIElementRef, name: &'static str) -> Option<CFType> {
        let attribute = CFString::from_static_string(name);
        let mut value: CFTypeRef = ptr::null();
        let err =
            AXUIElementCopyAttributeValue(element, attribute.as_concrete_TypeRef(), &mut value);
        if err != AX_SUCCESS || value.is_null() {
            return None;
        }
        Some(CFType::wrap_under_create_rule(value))
    }

    unsafe fn system_wide() -> Option<CFType> {
        if AXIsProcessTrusted() == 0 {
            return None;
        }
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return None;
        }
        Some(CFType::wrap_under_create_rule(system as CFTypeRef))
    }

    pub fn selected_text() -> Option<String> {
        unsafe {
            let system = system_wide()?;
            let focused = copy_attribute(system.as_CFTypeRef(), "AXFocusedUIElement")?;
            let value = copy_attribute(focused.as_CFTypeRef(), "AXSelectedText")?;
            value.downcast::<CFString>().map(|s| s.to_string())
        }
    }

    pub fn focused_app_name() -> Option<String> {
        unsafe {
            let system = system_wide()?;
            let app = copy_attribute(system.as_CFTypeRef(), "AXFocusedApplication")?;
            let title = copy_attribute(app.as_CFTypeRef(), "AXTitle")?;
            title.downcast::<CFString>().map(|s| s.to_string())
        }
    }
}
