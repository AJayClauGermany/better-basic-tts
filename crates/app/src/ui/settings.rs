//! The settings window. Everything that is not needed at a glance lives here.

use crate::state::{self, Tts};
use bbtts_core::settings::{RoleStyle, Settings as TtsSettings, MAX_RATE, MIN_RATE, RATE_STEP};
use bbtts_core::text::{AngleBrackets, InterpreterKind, UrlMode};
use bbtts_core::Role;
use bbtts_platform::hotkeys::Action as Do;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::component::{v_flex, ActiveTheme as _, Icon, Sizable as _, TitleBar};
use gpui_kit::*;

/// Handle of the open settings window, if any.
#[derive(Default)]
struct SettingsWindow(Option<AnyWindowHandle>);

impl Global for SettingsWindow {}

/// Open the settings window, or bring it forward if it is already open.
pub fn open(cx: &mut App) {
    if let Some(handle) = cx.try_global::<SettingsWindow>().and_then(|w| w.0) {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(780.), px(620.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(520.), px(380.))),
        window_decorations: Some(WindowDecorations::Client),
        app_id: Some(crate::APP_ID.into()),
        ..TitleBar::window_options()
    };
    match open_window(options, cx, |window, cx| {
        window.set_window_title("Better Basic TTS Settings");
        cx.new(|_| SettingsView)
    }) {
        Ok((handle, _)) => cx.set_global(SettingsWindow(Some(handle))),
        Err(e) => log::error!("could not open the settings window: {e:#}"),
    }
}

struct SettingsView;

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                TitleBar::new().child(
                    div()
                        .flex_1()
                        .text_center()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child("Settings"),
                ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    Settings::new("settings")
                        .sidebar_width(px(190.))
                        .pages(pages(cx)),
                ),
            )
    }
}

// ---------------------------------------------------------------------------
// Getters and setters over the global settings
// ---------------------------------------------------------------------------

fn get<T: 'static>(f: impl Fn(&TtsSettings) -> T + 'static) -> impl Fn(&App) -> T + 'static {
    move |cx| f(&cx.global::<Tts>().settings)
}

fn set<T: 'static>(f: impl Fn(&mut TtsSettings, T) + 'static) -> impl Fn(T, &mut App) + 'static {
    move |value, cx| state::update_settings(cx, |s| f(s, value))
}

fn options<T: Copy>(
    all: &[T],
    key: impl Fn(T) -> &'static str,
    label: impl Fn(T) -> &'static str,
) -> Vec<(SharedString, SharedString)> {
    all.iter()
        .map(|&v| (key(v).into(), label(v).into()))
        .collect()
}

fn interpreter_key(v: InterpreterKind) -> &'static str {
    match v {
        InterpreterKind::Auto => "auto",
        InterpreterKind::Plain => "plain",
        InterpreterKind::Markdown => "markdown",
    }
}

fn angle_key(v: AngleBrackets) -> &'static str {
    match v {
        AngleBrackets::Smart => "smart",
        AngleBrackets::Literal => "literal",
        AngleBrackets::StripTags => "strip_tags",
        AngleBrackets::Silent => "silent",
    }
}

fn url_key(v: UrlMode) -> &'static str {
    match v {
        UrlMode::Domain => "domain",
        UrlMode::Full => "full",
        UrlMode::Skip => "skip",
    }
}

fn from_key<T: Copy>(all: &[T], key: impl Fn(T) -> &'static str, wanted: &str) -> Option<T> {
    all.iter().copied().find(|&v| key(v) == wanted)
}

/// A text field that applies its value on Enter or when it loses focus —
/// not on every keystroke. Shortcuts must not be registered half-typed.
fn committed_input(
    key: impl Into<SharedString>,
    placeholder: &'static str,
    value: impl Fn(&App) -> SharedString + 'static,
    set_value: impl Fn(SharedString, &mut App) + 'static,
) -> SettingField<SharedString> {
    struct State {
        input: Entity<InputState>,
        committed: SharedString,
        _subscription: Subscription,
    }

    let key: SharedString = key.into();
    let set_value = std::rc::Rc::new(set_value);
    SettingField::render(move |options, window, cx| {
        let current = value(cx);
        let state = window.use_keyed_state(key.clone(), cx, {
            let current = current.clone();
            let set_value = set_value.clone();
            move |window, cx| {
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(current.clone())
                        .placeholder(placeholder)
                });
                let _subscription =
                    cx.subscribe(&input, move |state: &mut State, input, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                            let text: SharedString = input.read(cx).value().trim().to_string().into();
                            if text != state.committed {
                                state.committed = text.clone();
                                set_value(text, cx);
                            }
                        }
                    });
                State {
                    input,
                    committed: current,
                    _subscription,
                }
            }
        });
        // Follow changes made elsewhere (reset, hand-edited file).
        state.update(cx, |state, cx| {
            if state.committed != current {
                state.committed = current.clone();
                state.input.update(cx, |input, cx| {
                    input.set_value(current.clone(), window, cx);
                });
            }
        });
        let input = state.read(cx).input.clone();
        Input::new(&input).with_size(options.size()).w_64()
    })
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

fn pages(cx: &App) -> Vec<SettingPage> {
    vec![
        voice_page(cx),
        text_page(),
        roles_page(cx),
        shortcuts_page(cx),
        general_page(cx),
    ]
}

fn voice_options(cx: &App, default_label: &'static str) -> Vec<(SharedString, SharedString)> {
    let mut list: Vec<(SharedString, SharedString)> = vec![("".into(), default_label.into())];
    list.extend(
        cx.global::<Tts>()
            .voices
            .iter()
            .map(|v| (v.id.clone().into(), v.label().into())),
    );
    list
}

fn voice_page(cx: &App) -> SettingPage {
    let tts = cx.global::<Tts>();
    let mut engines: Vec<(SharedString, SharedString)> = vec![(
        "auto".into(),
        match tts.registry.engines().first() {
            Some(first) => format!("Automatic ({})", first.name()).into(),
            None => "Automatic".into(),
        },
    )];
    engines.extend(
        tts.registry
            .engines()
            .iter()
            .map(|e| (e.id().to_string().into(), e.name().to_string().into())),
    );
    let engine_note: SharedString = if tts.registry.is_empty() {
        bbtts_core::EngineRegistry::missing_engine_help().into()
    } else {
        "Speech is generated on this computer. Nothing is sent over the network.".into()
    };

    SettingPage::new("Voice")
        .icon(Icon::new(IconName::AudioLines))
        .default_open(true)
        .group(
            SettingGroup::new()
                .title("Voice")
                .description(engine_note)
                .item(
                    SettingItem::new(
                        "Engine",
                        SettingField::dropdown(
                            engines,
                            get(|s| s.engine.clone().into()),
                            set(|s, v: SharedString| s.engine = v.to_string()),
                        )
                        .default_value("auto"),
                    )
                    .description("Which local speech engine produces the audio."),
                )
                .item(SettingItem::new(
                    "Voice",
                    SettingField::scrollable_dropdown(
                        voice_options(cx, "Engine default"),
                        get(|s| s.voice.clone().into()),
                        set(|s, v: SharedString| s.voice = v.to_string()),
                    )
                    .default_value(""),
                ))
                .item(
                    SettingItem::new(
                        "Speed",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: MIN_RATE as f64,
                                max: MAX_RATE as f64,
                                step: RATE_STEP as f64,
                            },
                            get(|s| (s.rate as f64 * 10.0).round() / 10.0),
                            set(|s, v: f64| s.rate = v as f32),
                        )
                        .default_value(1.0),
                    )
                    .description("1.0 is the voice's normal speed. Same as the turtle and the rabbit."),
                )
                .item(SettingItem::new(
                    "Volume (%)",
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: 0.0,
                            max: 100.0,
                            step: 5.0,
                        },
                        get(|s| (s.volume as f64 * 100.0).round()),
                        set(|s, v: f64| s.volume = (v / 100.0) as f32),
                    )
                    .default_value(100.0),
                ))
                .item(
                    SettingItem::new(
                        "Preview",
                        SettingField::render(|options, _, _| {
                            Button::new("preview-voice")
                                .outline()
                                .with_size(options.size())
                                .icon(Icon::new(IconName::Play))
                                .label("Play sample")
                                .on_click(|_, _, cx| state::preview_voice(cx))
                        }),
                    )
                    .description("Reads a short sentence with these settings."),
                ),
        )
}

fn text_page() -> SettingPage {
    SettingPage::new("Text")
        .icon(Icon::new(IconName::FileText))
        .group(
            SettingGroup::new()
                .title("Interpretation")
                .description(
                    "Text is always treated as text: nothing in it can change the voice, \
                     the speed or run markup.",
                )
                .item(
                    SettingItem::new(
                        "Read text as",
                        SettingField::dropdown(
                            options(&InterpreterKind::ALL, interpreter_key, InterpreterKind::label),
                            get(|s| interpreter_key(s.interpreter).into()),
                            set(|s, v: SharedString| {
                                if let Some(k) = from_key(&InterpreterKind::ALL, interpreter_key, &v) {
                                    s.interpreter = k;
                                }
                            }),
                        )
                        .default_value("auto"),
                    )
                    .description(
                        "Markdown gives headings, quotes, code and lists their own role, \
                         which can have their own voice.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Angle brackets < >",
                        SettingField::dropdown(
                            options(&AngleBrackets::ALL, angle_key, AngleBrackets::label),
                            get(|s| angle_key(s.angle_brackets).into()),
                            set(|s, v: SharedString| {
                                if let Some(k) = from_key(&AngleBrackets::ALL, angle_key, &v) {
                                    s.angle_brackets = k;
                                }
                            }),
                        )
                        .default_value("smart"),
                    )
                    .description(
                        "Smart reads \"a < b\" as \"a less than b\", \"<div>\" as \"div tag\" \
                         and \"Vec<String>\" as \"Vec of String\".",
                    ),
                )
                .item(SettingItem::new(
                    "Links",
                    SettingField::dropdown(
                        options(&UrlMode::ALL, url_key, UrlMode::label),
                        get(|s| url_key(s.urls).into()),
                        set(|s, v: SharedString| {
                            if let Some(k) = from_key(&UrlMode::ALL, url_key, &v) {
                                s.urls = k;
                            }
                        }),
                    )
                    .default_value("domain"),
                )),
        )
        .group(
            SettingGroup::new().title("Pacing").item(
                SettingItem::new(
                    "Pause between sentences (ms)",
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: 0.0,
                            max: 2000.0,
                            step: 10.0,
                        },
                        get(|s| s.sentence_pause_ms as f64),
                        set(|s, v: f64| s.sentence_pause_ms = v.max(0.0) as u32),
                    )
                    .default_value(90.0),
                )
                .description("Pauses after headings and paragraphs are set per role."),
            ),
        )
}

fn role_group(role: Role, cx: &App) -> SettingGroup {
    let defaults = RoleStyle::default();
    let default_pause = bbtts_core::settings::RoleStyles::default()
        .get(role)
        .pause_after_ms as f64;
    SettingGroup::new()
        .title(role.label())
        .item(SettingItem::new(
            "Voice",
            SettingField::scrollable_dropdown(
                voice_options(cx, "Same as main voice"),
                get(move |s| s.roles.get(role).voice.clone().into()),
                set(move |s, v: SharedString| s.roles.get_mut(role).voice = v.to_string()),
            )
            .default_value(""),
        ))
        .item(
            SettingItem::new(
                "Speed",
                SettingField::number_input(
                    NumberFieldOptions {
                        min: 0.5,
                        max: 2.0,
                        step: 0.05,
                    },
                    get(move |s| (s.roles.get(role).rate as f64 * 100.0).round() / 100.0),
                    set(move |s, v: f64| s.roles.get_mut(role).rate = v as f32),
                )
                .default_value(defaults.rate as f64),
            )
            .description("Relative to the main speed."),
        )
        .item(
            SettingItem::new(
                "Pitch",
                SettingField::number_input(
                    NumberFieldOptions {
                        min: 0.5,
                        max: 2.0,
                        step: 0.05,
                    },
                    get(move |s| (s.roles.get(role).pitch as f64 * 100.0).round() / 100.0),
                    set(move |s, v: f64| s.roles.get_mut(role).pitch = v as f32),
                )
                .default_value(defaults.pitch as f64),
            )
            .description("1.0 is the voice's normal pitch. Not every engine supports it."),
        )
        .item(
            SettingItem::new(
                "Announce with",
                committed_input(
                    format!("announce-{}", role.key()),
                    "nothing",
                    get(move |s| s.roles.get(role).announce.clone().into()),
                    set(move |s, v: SharedString| s.roles.get_mut(role).announce = v.to_string()),
                ),
            )
            .description("Words spoken before each one, for example \"Quote\"."),
        )
        .item(SettingItem::new(
            "Pause after (ms)",
            SettingField::number_input(
                NumberFieldOptions {
                    min: 0.0,
                    max: 5000.0,
                    step: 20.0,
                },
                get(move |s| s.roles.get(role).pause_after_ms as f64),
                set(move |s, v: f64| s.roles.get_mut(role).pause_after_ms = v.max(0.0) as u32),
            )
            .default_value(default_pause),
        ))
        .item(SettingItem::new(
            "Skip",
            SettingField::switch(
                get(move |s| s.roles.get(role).skip),
                set(move |s, v: bool| s.roles.get_mut(role).skip = v),
            )
            .default_value(false),
        ))
}

fn roles_page(cx: &App) -> SettingPage {
    SettingPage::new("Voices by role")
        .icon(Icon::new(IconName::Users))
        .description(
            "Give headings, quotes, code and list items their own voice, speed or pause. \
             Roles come from the interpreter chosen under Text (Markdown has all of them; \
             plain text is all paragraphs).",
        )
        .groups(
            [
                Role::Heading,
                Role::Paragraph,
                Role::Quote,
                Role::Code,
                Role::ListItem,
            ]
            .map(|role| role_group(role, cx)),
        )
}

fn shortcuts_page(cx: &App) -> SettingPage {
    let tts = cx.global::<Tts>();
    let mut note = String::from(
        "These work in every application. Write them like Ctrl+Alt+S, Cmd+Shift+S or Alt+F9; \
         leave a field empty for no shortcut. Press Enter to apply.",
    );
    for problem in &tts.hotkey_problems {
        note.push_str("\n⚠ ");
        note.push_str(problem);
    }

    fn field(action: Do) -> SettingField<SharedString> {
        fn slot(s: &mut bbtts_core::settings::Shortcuts, action: Do) -> &mut String {
            match action {
                Do::SpeakSelection => &mut s.speak_selection,
                Do::SpeakClipboard => &mut s.speak_clipboard,
                Do::PauseResume => &mut s.pause_resume,
                Do::Stop => &mut s.stop,
                Do::Previous => &mut s.previous,
                Do::Next => &mut s.next,
                Do::Slower => &mut s.slower,
                Do::Faster => &mut s.faster,
            }
        }
        committed_input(
            format!("shortcut-{action:?}"),
            "none",
            move |cx| {
                let mut shortcuts = cx.global::<Tts>().settings.shortcuts.clone();
                slot(&mut shortcuts, action).clone().into()
            },
            set(move |s, v: SharedString| *slot(&mut s.shortcuts, action) = v.to_string()),
        )
    }

    SettingPage::new("Shortcuts")
        .icon(Icon::new(IconName::Keyboard))
        .group(
            SettingGroup::new()
                .title("Global shortcuts")
                .description(note)
                .items(Do::ALL.map(|action| {
                    let item = SettingItem::new(action.label(), field(action));
                    match action {
                        Do::SpeakSelection => item.description(
                            "Reads the selected text. Pressing it again while reading stops.",
                        ),
                        _ => item,
                    }
                })),
        )
}

fn general_page(cx: &App) -> SettingPage {
    let path = cx.global::<Tts>().settings_path.clone();
    let path_label: SharedString = path.display().to_string().into();
    SettingPage::new("General")
        .icon(Icon::new(IconName::Settings))
        .group(
            SettingGroup::new()
                .title("Window")
                .item(
                    SettingItem::new(
                        "Keep on top",
                        SettingField::switch(
                            get(|s| s.always_on_top),
                            set(|s, v: bool| s.always_on_top = v),
                        )
                        .default_value(true),
                    )
                    .description(
                        "Keep the controls above other windows. Applies the next time the app starts.",
                    ),
                )

        )
        .group(
            SettingGroup::new()
                .title("Advanced")
                .description(
                    "Pronunciation rules (find and replace) and additional local engines such as \
                     Piper, KittenTTS or NeuTTS are configured in the settings file.",
                )
                .item(
                    SettingItem::new(
                        "Settings file",
                        SettingField::render(move |options, _, _| {
                            let path = path.clone();
                            Button::new("open-settings-file")
                                .outline()
                                .with_size(options.size())
                                .label("Open file")
                                .on_click(move |_, _, cx| {
                                    // Make sure there is a file to open.
                                    if !path.exists() {
                                        let _ = cx.global::<Tts>().settings.save(&path);
                                    }
                                    cx.open_with_system(&path);
                                })
                        }),
                    )
                    .description(path_label),
                ),
        )
}
