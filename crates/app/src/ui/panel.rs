//! The control panel: six buttons and nothing to learn.
//!
//! ```text
//! ┌────────────────────────────────────────────────┐
//! │ ✕            TextEdit               1.2×   ⚙   │
//! │ [🐢]  [⏪]  [⏯]  [⏹]  [⏩]  [🐇]                │
//! │ The sentence being read right now…      3 / 42 │
//! └────────────────────────────────────────────────┘
//! ```

use crate::assets::Glyph;
use crate::state::{self, Tts};
use bbtts_core::Status;
use bbtts_platform::hotkeys::Action as Do;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Icon, Sizable as _, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub const PANEL_WIDTH: f32 = 412.0;
pub const PANEL_HEIGHT: f32 = 142.0;

actions!(
    panel,
    [
        PlayPause,
        Stop,
        Previous,
        Next,
        Slower,
        Faster,
        SpeakClipboard,
        OpenSettings
    ]
);

const CONTEXT: &str = "ControlPanel";

/// Keys that work while the panel has focus.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("space", PlayPause, Some(CONTEXT)),
        KeyBinding::new("escape", Stop, Some(CONTEXT)),
        KeyBinding::new("left", Previous, Some(CONTEXT)),
        KeyBinding::new("right", Next, Some(CONTEXT)),
        KeyBinding::new("down", Slower, Some(CONTEXT)),
        KeyBinding::new("up", Faster, Some(CONTEXT)),
        KeyBinding::new("-", Slower, Some(CONTEXT)),
        KeyBinding::new("=", Faster, Some(CONTEXT)),
    ]);
    // Paste-to-speak and the conventional settings shortcut.
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-v", SpeakClipboard, Some(CONTEXT)),
        KeyBinding::new("cmd-,", OpenSettings, Some(CONTEXT)),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-v", SpeakClipboard, Some(CONTEXT)),
        KeyBinding::new("ctrl-,", OpenSettings, Some(CONTEXT)),
    ]);
}

pub struct Panel {
    focus: FocusHandle,
}

impl Panel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self { focus }
    }
}

fn format_rate(rate: f32) -> String {
    format!("{rate:.1}×")
}

/// One of the six big buttons.
fn transport(
    id: &'static str,
    glyph: Glyph,
    tooltip: &'static str,
    action: Do,
    enabled: bool,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id(id)
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .h(px(54.))
        .rounded(px(9.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .text_color(theme.foreground)
        .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
        .map(|this| {
            if enabled {
                this.cursor_pointer()
                    .hover(|s| s.bg(theme.secondary_hover))
                    .active(|s| s.bg(theme.secondary_active))
                    .on_click(move |_, _, cx| state::perform(action, cx))
            } else {
                this.opacity(0.38)
            }
        })
        .child(glyph.icon().size(px(30.)))
}

impl Render for Panel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tts = cx.global::<Tts>();
        let snap = tts.snapshot();
        let theme = cx.theme();

        let active = snap.is_active();
        let loaded = snap.total > 0;
        let title: SharedString = if loaded && !snap.title.is_empty() {
            snap.title.clone().into()
        } else {
            "Better Basic TTS".into()
        };

        // What the line under the buttons says, most urgent first.
        let problem = snap
            .error
            .clone()
            .or_else(|| tts.notice.clone())
            .or_else(|| tts.settings_problem.clone())
            .or_else(|| tts.audio_problem.clone());
        let (status_text, status_is_problem): (SharedString, bool) = match (&problem, &snap.current)
        {
            (Some(problem), _) => (problem.clone().into(), true),
            (None, Some(sentence)) if loaded => (sentence.replace('\n', " ").into(), false),
            _ => {
                let shortcut = tts.settings.shortcuts.speak_selection.trim();
                let hint = if shortcut.is_empty() || !tts.hotkey_problems.is_empty() {
                    "Press play to read the clipboard.".to_string()
                } else {
                    format!("Select text, then press {shortcut}.")
                };
                (hint.into(), false)
            }
        };
        let counter: Option<SharedString> =
            loaded.then(|| format!("{} / {}", snap.index + 1, snap.total).into());

        let (play_icon, play_tip) = match snap.status {
            Status::Playing | Status::Loading => (Glyph::Pause, "Pause (Space)"),
            Status::Paused => (Glyph::Play, "Resume (Space)"),
            Status::Idle if loaded => (Glyph::Play, "Play from the start (Space)"),
            Status::Idle => (Glyph::Play, "Read the clipboard (Space)"),
        };
        let can_slow = snap.rate > bbtts_core::settings::MIN_RATE + 1e-4;
        let can_fast = snap.rate < bbtts_core::settings::MAX_RATE - 1e-4;

        v_flex()
            .id("control-panel")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(|_: &PlayPause, _, cx| state::perform(Do::PauseResume, cx))
            .on_action(|_: &Stop, _, cx| state::perform(Do::Stop, cx))
            .on_action(|_: &Previous, _, cx| state::perform(Do::Previous, cx))
            .on_action(|_: &Next, _, cx| state::perform(Do::Next, cx))
            .on_action(|_: &Slower, _, cx| state::perform(Do::Slower, cx))
            .on_action(|_: &Faster, _, cx| state::perform(Do::Faster, cx))
            .on_action(|_: &SpeakClipboard, _, cx| state::perform(Do::SpeakClipboard, cx))
            .on_action(|_: &OpenSettings, _, cx| super::settings::open(cx))
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                TitleBar::new().child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_center()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .child(title),
                        )
                        .child(
                            div()
                                .id("rate")
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .tooltip(|window, cx| {
                                    Tooltip::new("Speed. Change it with the turtle and the rabbit.")
                                        .build(window, cx)
                                })
                                .child(format_rate(snap.rate)),
                        )
                        .child(
                            Button::new("settings")
                                .ghost()
                                .small()
                                .icon(Icon::new(IconName::Settings))
                                .tooltip("Settings")
                                .on_click(|_, _, cx| super::settings::open(cx)),
                        ),
                ),
            )
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .gap_2()
                    .child(transport(
                        "slower",
                        Glyph::Turtle,
                        "Slower (↓)",
                        Do::Slower,
                        can_slow,
                        cx,
                    ))
                    .child(transport(
                        "previous",
                        Glyph::Rewind,
                        "Previous sentence (←)",
                        Do::Previous,
                        active,
                        cx,
                    ))
                    .child(transport(
                        "play-pause",
                        play_icon,
                        play_tip,
                        Do::PauseResume,
                        true,
                        cx,
                    ))
                    .child(transport(
                        "stop",
                        Glyph::Stop,
                        "Stop (Esc)",
                        Do::Stop,
                        active,
                        cx,
                    ))
                    .child(transport(
                        "next",
                        Glyph::Forward,
                        "Next sentence (→)",
                        Do::Next,
                        active,
                        cx,
                    ))
                    .child(transport(
                        "faster",
                        Glyph::Rabbit,
                        "Faster (↑)",
                        Do::Faster,
                        can_fast,
                        cx,
                    )),
            )
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_2()
                    .gap_3()
                    .items_center()
                    .text_xs()
                    .child(
                        div()
                            .id("status")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(if status_is_problem {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            })
                            .tooltip({
                                let full = status_text.clone();
                                move |window, cx| Tooltip::new(full.clone()).build(window, cx)
                            })
                            .child(status_text),
                    )
                    .when_some(counter, |this, counter| {
                        this.child(
                            div()
                                .flex_shrink_0()
                                .text_color(theme.muted_foreground)
                                .child(counter),
                        )
                    }),
            )
    }
}
