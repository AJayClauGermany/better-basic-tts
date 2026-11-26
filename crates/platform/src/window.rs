//! Keeping a window above the others.
//!
//! The UI toolkit has no portable "always on top" switch, so this talks to
//! the window system directly:
//!
//! * Windows — `SetWindowPos(HWND_TOPMOST)`
//! * X11 — the `_NET_WM_STATE_ABOVE` window manager hint
//! * macOS — the window's level and Spaces behaviour, set on the `NSWindow`
//! * Wayland — no protocol exists; use the compositor's own "keep above"

use anyhow::Result;
use raw_window_handle::RawWindowHandle;

/// Ask the window system to keep `window` above (or no longer above) normal
/// windows. Returns `Ok(false)` where that is not possible.
pub fn set_always_on_top(window: RawWindowHandle, on_top: bool) -> Result<bool> {
    match window {
        #[cfg(windows)]
        RawWindowHandle::Win32(handle) => {
            windows_impl::set_topmost(handle.hwnd.get(), on_top)?;
            Ok(true)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        RawWindowHandle::Xcb(handle) => {
            x11::set_above(handle.window.get(), on_top)?;
            Ok(true)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        RawWindowHandle::Xlib(handle) => {
            x11::set_above(handle.window as u32, on_top)?;
            Ok(true)
        }
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => {
            macos::set_floating(handle.ns_view.as_ptr(), on_top);
            Ok(true)
        }
        _ => {
            let _ = on_top;
            Ok(false)
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use std::ffi::c_void;

    /// `NSFloatingWindowLevel`: above the normal windows of every app.
    const FLOATING_LEVEL: isize = 3;
    const NORMAL_LEVEL: isize = 0;
    /// `NSWindowCollectionBehaviorCanJoinAllSpaces`: follow the user from
    /// Space to Space instead of staying behind on the one it was opened on.
    const CAN_JOIN_ALL_SPACES: usize = 1 << 0;
    /// `NSWindowCollectionBehaviorFullScreenAuxiliary`: may sit on top of
    /// another app's full-screen window.
    const FULL_SCREEN_AUXILIARY: usize = 1 << 8;

    /// Must be called on the main thread, with a live `NSView`.
    pub fn set_floating(ns_view: *mut c_void, on_top: bool) {
        // SAFETY: the pointer comes from the toolkit's window handle for a
        // window that is alive for the duration of this call, and we are on
        // the main thread (the toolkit only hands out handles there). The
        // selectors and argument types are those of NSView / NSWindow.
        unsafe {
            let view = ns_view as *mut AnyObject;
            if view.is_null() {
                return;
            }
            let window: *mut AnyObject = msg_send![view, window];
            if window.is_null() {
                return;
            }
            let level = if on_top { FLOATING_LEVEL } else { NORMAL_LEVEL };
            let _: () = msg_send![window, setLevel: level];
            // A window that hides when another app comes forward would
            // defeat the purpose.
            let _: () = msg_send![window, setHidesOnDeactivate: false];
            let behavior = if on_top {
                CAN_JOIN_ALL_SPACES | FULL_SCREEN_AUXILIARY
            } else {
                0
            };
            let _: () = msg_send![window, setCollectionBehavior: behavior];
        }
    }
}

#[cfg(windows)]
mod windows_impl {
    use anyhow::{bail, Result};

    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const HWND_TOPMOST: isize = -1;
    const HWND_NOTOPMOST: isize = -2;

    #[link(name = "user32")]
    extern "system" {
        fn SetWindowPos(
            hwnd: isize,
            insert_after: isize,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
    }

    pub fn set_topmost(hwnd: isize, on_top: bool) -> Result<()> {
        let after = if on_top { HWND_TOPMOST } else { HWND_NOTOPMOST };
        let ok = unsafe {
            SetWindowPos(
                hwnd,
                after,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        };
        if ok == 0 {
            bail!("SetWindowPos failed");
        }
        Ok(())
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod x11 {
    use anyhow::{Context, Result};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, PropMode,
    };
    use x11rb::wrapper::ConnectionExt as _;

    pub fn set_above(window: u32, on_top: bool) -> Result<()> {
        let (conn, screen_num) = x11rb::connect(None).context("could not connect to X11")?;
        let root = conn.setup().roots[screen_num].root;
        let atom = |name: &str| -> Result<u32> {
            Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
        };
        let net_wm_state = atom("_NET_WM_STATE")?;
        let above = atom("_NET_WM_STATE_ABOVE")?;

        // For a window that is not mapped yet the property is read when it
        // is; for a mapped window the window manager wants a client message.
        // Doing both covers either order.
        if on_top {
            conn.change_property32(
                PropMode::APPEND,
                window,
                net_wm_state,
                AtomEnum::ATOM,
                &[above],
            )?;
        }
        const REMOVE: u32 = 0;
        const ADD: u32 = 1;
        const SOURCE_APPLICATION: u32 = 1;
        let event = ClientMessageEvent::new(
            32,
            window,
            net_wm_state,
            [
                if on_top { ADD } else { REMOVE },
                above,
                0,
                SOURCE_APPLICATION,
                0,
            ],
        );
        conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )?;
        conn.flush()?;
        // Round-trip so the requests are processed before we disconnect.
        conn.get_input_focus()?.reply()?;
        Ok(())
    }
}
