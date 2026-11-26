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
//! All functions here block for up to a few hundred milliseconds; call them
//! from a background thread.

use anyhow::{anyhow, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub text: String,
    /// Name of the application the text came from, if known.
    pub source: Option<String>,
}

/// The text currently selected in the frontmost application.
pub fn capture_selection() -> Result<Captured> {
    let source = frontmost_app();
    let text = platform::selected_text()?;
    if text.trim().is_empty() {
        return Err(anyhow!("No text is selected."));
    }
    Ok(Captured { text, source })
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
        let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn wayland() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
    }

    pub fn selected_text() -> Result<String> {
        match read(LinuxClipboardKind::Primary) {
            Ok(text) if !text.is_empty() => Ok(text),
            other => {
                if wayland() {
                    if let Some(text) = wl_paste(true) {
                        return Ok(text);
                    }
                }
                other
            }
        }
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

    pub fn selected_text() -> Result<String> {
        #[cfg(target_os = "macos")]
        if let Some(text) = super::accessibility::selected_text() {
            if !text.trim().is_empty() {
                return Ok(text);
            }
        }
        copy_selection()
    }

    /// Press the platform's copy shortcut in the frontmost application and
    /// read what lands on the clipboard, then put the old text back.
    fn copy_selection() -> Result<String> {
        let mut clipboard = Clipboard::new().context("could not access the clipboard")?;
        let saved = clipboard.get_text().ok();

        // A marker lets us tell "nothing was selected" from "the selection
        // equals what was on the clipboard already".
        let marker = format!("\u{200b}bbtts-{}\u{200b}", std::process::id());
        clipboard
            .set_text(marker.clone())
            .context("could not prepare the clipboard")?;

        let pressed = press_copy();

        let mut copied = None;
        if pressed.is_ok() {
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
        }

        // Restore what was there before.
        match saved {
            Some(text) => {
                let _ = clipboard.set_text(text);
            }
            None => {
                let _ = clipboard.clear();
            }
        }

        pressed?;
        Ok(copied.unwrap_or_default())
    }

    fn press_copy() -> Result<()> {
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
