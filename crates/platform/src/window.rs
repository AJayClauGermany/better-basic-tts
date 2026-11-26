//! Keeping a window above the others.
//!
//! The UI toolkit has no portable "always on top" switch, so this talks to
//! the window system directly:
//!
//! * Windows — `SetWindowPos(HWND_TOPMOST)`
//! * X11 — the `_NET_WM_STATE_ABOVE` window manager hint
//! * macOS — handled by the toolkit (a floating panel), nothing to do here
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
        _ => {
            let _ = on_top;
            Ok(false)
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
