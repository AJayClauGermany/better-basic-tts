//! Starting the graphical application.

use crate::state::{self, Event, Tts};
use crate::ui::{self, panel};
use anyhow::Result;
use bbtts_platform::ipc::{self, Request};
use gpui_kit::component::TitleBar;
use gpui_kit::*;
use std::path::PathBuf;

/// Run the control panel until it is closed. `initial` is performed once the
/// panel is up (used by `speak` & co. when no instance was running).
pub fn run(settings_path: PathBuf, initial: Option<Request>) -> Result<()> {
    gpui_kit::application()
        .with_assets(crate::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            panel::bind_keys(cx);
            state::init(settings_path, cx);

            // Become the single instance and accept remote control.
            let events = state::event_sender(cx);
            let server = ipc::serve({
                let events = events.clone();
                move |request| {
                    let _ = events.send(Event::Ipc(request));
                }
            });
            let mut server = match server {
                Ok(server) => Some(server),
                Err(e) => {
                    log::warn!("remote control is unavailable: {e:#}");
                    None
                }
            };

            let always_on_top = cx.global::<Tts>().settings.always_on_top;
            let bounds = Bounds::centered(
                None,
                size(px(panel::PANEL_WIDTH), px(panel::PANEL_HEIGHT)),
                cx,
            );
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                is_resizable: false,
                // On macOS a floating panel stays above other windows.
                kind: if always_on_top && cfg!(target_os = "macos") {
                    WindowKind::Floating
                } else {
                    WindowKind::Normal
                },
                window_decorations: Some(WindowDecorations::Client),
                app_id: Some(crate::APP_ID.into()),
                ..TitleBar::window_options()
            };
            let opened = open_window(options, cx, |window, cx| {
                window.set_window_title("Better Basic TTS");
                cx.new(|cx| ui::panel::Panel::new(window, cx))
            });
            let handle = match opened {
                Ok((handle, _)) => handle,
                Err(e) => {
                    eprintln!("error: could not open the control panel: {e:#}");
                    cx.quit();
                    return;
                }
            };
            cx.global_mut::<Tts>().panel = Some(handle);
            state::apply_always_on_top(cx);

            // The panel is the application: closing it quits.
            cx.on_window_closed(move |cx, closed| {
                if closed == handle.window_id() {
                    cx.quit();
                }
            })
            .detach();
            // Keep the socket for the lifetime of the app.
            cx.on_app_quit(move |_| {
                let server = server.take();
                async move { drop(server) }
            })
            .detach();

            if let Some(request) = initial {
                let _ = events.send(Event::Ipc(request));
            }
            cx.activate(true);
        });
    Ok(())
}
