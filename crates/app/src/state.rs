//! Application state shared by the control panel and the settings window.
//!
//! [`Tts`] is a GPUI global. Everything that happens outside the UI thread —
//! the player changing state, a global shortcut, a request from the command
//! line, a finished selection capture — arrives as an [`Event`] on one
//! channel and is handled on the UI thread by [`handle_event`].

use bbtts_core::engine::Voice;
use bbtts_core::player::output::{AudioOutput, DeviceOutput, SilentOutput};
use bbtts_core::player::PlayerConfig;
use bbtts_core::{EngineRegistry, Player, PlayerSnapshot, Settings, Status};
use bbtts_platform::hotkeys::{Action, Hotkeys};
use bbtts_platform::ipc::Request;
use bbtts_platform::selection::{self, Captured};
use gpui_kit::{App, AsyncApp, Global};
use std::path::PathBuf;

pub enum Event {
    PlayerChanged,
    Hotkey(Action),
    Ipc(Request),
    /// A background capture of the selection / clipboard finished.
    Captured(Result<Captured, String>),
}

pub struct Tts {
    pub settings: Settings,
    pub settings_path: PathBuf,
    pub registry: EngineRegistry,
    pub player: Player,
    /// Voices of the active engine.
    pub voices: Vec<Voice>,
    /// A short message for the panel ("No text is selected.").
    pub notice: Option<String>,
    /// Problems found at start-up or when applying settings.
    pub settings_problem: Option<String>,
    pub hotkey_problems: Vec<String>,
    pub audio_problem: Option<String>,
    hotkeys: Option<Hotkeys>,
    events: flume::Sender<Event>,
    /// The control panel window.
    pub panel: Option<gpui_kit::AnyWindowHandle>,
}

impl Global for Tts {}

impl Tts {
    pub fn snapshot(&self) -> PlayerSnapshot {
        self.player.snapshot()
    }

    fn player_config(&self) -> PlayerConfig {
        PlayerConfig::from_settings(&self.settings, self.registry.resolve(&self.settings.engine))
    }

    fn reload_voices(&mut self) {
        self.voices = match self.registry.resolve(&self.settings.engine) {
            Some(engine) => engine.voices().unwrap_or_else(|e| {
                log::warn!("could not list voices: {e:#}");
                Vec::new()
            }),
            None => Vec::new(),
        };
        self.voices
            .sort_by(|a, b| a.label().to_lowercase().cmp(&b.label().to_lowercase()));
    }

    fn apply_hotkeys(&mut self) {
        let s = &self.settings.shortcuts;
        let bindings = [
            (Action::SpeakSelection, s.speak_selection.clone()),
            (Action::SpeakClipboard, s.speak_clipboard.clone()),
            (Action::PauseResume, s.pause_resume.clone()),
            (Action::Stop, s.stop.clone()),
            (Action::Previous, s.previous.clone()),
            (Action::Next, s.next.clone()),
            (Action::Slower, s.slower.clone()),
            (Action::Faster, s.faster.clone()),
        ];
        if self.hotkeys.is_none() {
            let events = self.events.clone();
            match Hotkeys::new(move |action| {
                let _ = events.send(Event::Hotkey(action));
            }) {
                Ok(h) => self.hotkeys = Some(h),
                Err(e) => {
                    self.hotkey_problems = vec![format!("{e:#}")];
                    return;
                }
            }
        }
        if let Some(hotkeys) = &mut self.hotkeys {
            self.hotkey_problems = hotkeys.apply(&bindings);
        }
    }
}

/// Build the global state. Must run on the UI thread, inside the GPUI app.
pub fn init(settings_path: PathBuf, cx: &mut App) {
    let (settings, settings_problem) = match Settings::load(&settings_path) {
        Ok(s) => (s, None),
        Err(e) => (
            Settings::default(),
            Some(format!("{e:#}. Using defaults; the file was left untouched.")),
        ),
    };
    let registry = EngineRegistry::detect(&settings.engines);

    let (events, rx) = flume::unbounded::<Event>();

    // Fall back to a silent output so the controls still work (and the
    // problem is visible) on machines without an audio device.
    // BBTTS_AUDIO=off does the same on purpose (demos, UI tests).
    let muted = std::env::var("BBTTS_AUDIO").is_ok_and(|v| v == "off");
    let audio_problem = if muted {
        None
    } else {
        DeviceOutput::probe().err().map(|e| format!("{e:#}"))
    };
    let silent = muted || audio_problem.is_some();
    let config = PlayerConfig::from_settings(&settings, registry.resolve(&settings.engine));
    let notify = events.clone();
    let player = Player::new(
        move || -> Box<dyn AudioOutput> {
            if silent {
                Box::new(SilentOutput::new())
            } else {
                Box::new(DeviceOutput::new())
            }
        },
        config,
        settings.rate,
        move || {
            let _ = notify.send(Event::PlayerChanged);
        },
    );

    let mut tts = Tts {
        settings,
        settings_path,
        registry,
        player,
        voices: Vec::new(),
        notice: None,
        settings_problem,
        hotkey_problems: Vec::new(),
        audio_problem,
        hotkeys: None,
        events,
        panel: None,
    };
    tts.reload_voices();
    tts.apply_hotkeys();
    for problem in &tts.hotkey_problems {
        log::warn!("{problem}");
    }
    cx.set_global(tts);

    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(event) = rx.recv_async().await {
            cx.update(|cx| handle_event(event, cx));
        }
    })
    .detach();
}

/// A sender for events from other threads (IPC, tests).
pub fn event_sender(cx: &App) -> flume::Sender<Event> {
    cx.global::<Tts>().events.clone()
}

fn handle_event(event: Event, cx: &mut App) {
    match event {
        Event::PlayerChanged => {
            // The turtle / rabbit buttons change the rate inside the player;
            // remember it for next time.
            let rate = cx.global::<Tts>().snapshot().rate;
            let tts = cx.global_mut::<Tts>();
            if (tts.settings.rate - rate).abs() > 1e-4 {
                tts.settings.rate = rate;
                save(tts);
            }
            if tts.snapshot().status == Status::Playing {
                tts.notice = None;
            }
        }
        Event::Hotkey(action) => {
            perform(action, cx);
            return;
        }
        Event::Ipc(request) => {
            match request {
                Request::Show => show_panel(cx),
                Request::SpeakSelection => perform(Action::SpeakSelection, cx),
                Request::SpeakClipboard => perform(Action::SpeakClipboard, cx),
                Request::Speak(text) => speak_text(&text, "Text", cx),
                Request::PauseResume => perform(Action::PauseResume, cx),
                Request::Stop => perform(Action::Stop, cx),
                Request::Previous => perform(Action::Previous, cx),
                Request::Next => perform(Action::Next, cx),
                Request::Slower => perform(Action::Slower, cx),
                Request::Faster => perform(Action::Faster, cx),
            }
            return;
        }
        Event::Captured(result) => match result {
            Ok(captured) => {
                let title = captured.source.unwrap_or_else(|| "Selection".to_string());
                speak_text(&captured.text, &title, cx);
                return;
            }
            Err(message) => {
                cx.global_mut::<Tts>().notice = Some(message);
            }
        },
    }
    cx.refresh_windows();
}

/// Do what a button or shortcut asks for.
pub fn perform(action: Action, cx: &mut App) {
    let tts = cx.global_mut::<Tts>();
    let snapshot = tts.snapshot();
    match action {
        Action::SpeakSelection => {
            // Like the system shortcut on macOS: the same key starts and stops.
            if snapshot.is_active() {
                tts.player.stop();
            } else {
                capture(tts, selection::capture_selection);
            }
        }
        Action::SpeakClipboard => capture(tts, || {
            selection::clipboard_text().map(|text| Captured {
                text,
                source: Some("Clipboard".into()),
            })
        }),
        Action::PauseResume => {
            if snapshot.total == 0 {
                // Nothing loaded yet: the play button reads the clipboard.
                capture(tts, || {
                    selection::clipboard_text().map(|text| Captured {
                        text,
                        source: Some("Clipboard".into()),
                    })
                });
            } else {
                tts.player.toggle_pause();
            }
        }
        Action::Stop => tts.player.stop(),
        Action::Previous => tts.player.previous(),
        Action::Next => tts.player.next(),
        Action::Slower => tts.player.slower(),
        Action::Faster => tts.player.faster(),
    }
    cx.refresh_windows();
}

/// Run a (blocking) capture on a background thread.
fn capture(tts: &mut Tts, f: impl FnOnce() -> anyhow::Result<Captured> + Send + 'static) {
    tts.notice = None;
    let events = tts.events.clone();
    std::thread::spawn(move || {
        let result = f().map_err(|e| format!("{e:#}"));
        let _ = events.send(Event::Captured(result));
    });
}

/// Interpret `text` with the current settings and start reading it.
pub fn speak_text(text: &str, title: &str, cx: &mut App) {
    let tts = cx.global_mut::<Tts>();
    let utterances = tts.settings.prepare(text);
    if utterances.is_empty() {
        tts.notice = Some("There is no text to read.".into());
    } else {
        tts.notice = None;
        tts.player.speak(utterances, title);
    }
    cx.refresh_windows();
}

/// Bring the control panel to the front (explicit request only: reading
/// never activates the panel, so focus stays in the app being read).
pub fn show_panel(cx: &mut App) {
    if let Some(panel) = cx.global::<Tts>().panel {
        let _ = panel.update(cx, |_, window, _| window.activate_window());
    }
}

/// Change settings, persist them and apply them to the running player.
pub fn update_settings(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    let tts = cx.global_mut::<Tts>();
    let before = tts.settings.clone();
    change(&mut tts.settings);
    tts.settings.normalize();
    if tts.settings == before {
        return;
    }
    if tts.settings.engine != before.engine {
        tts.reload_voices();
        // A voice id from another engine means nothing here.
        if !tts.voices.iter().any(|v| v.id == tts.settings.voice) {
            tts.settings.voice.clear();
        }
        for role in bbtts_core::Role::ALL {
            let style = tts.settings.roles.get_mut(role);
            if !tts.voices.iter().any(|v| v.id == style.voice) {
                style.voice.clear();
            }
        }
    }
    if tts.settings.shortcuts != before.shortcuts {
        tts.apply_hotkeys();
    }
    if (tts.settings.rate - before.rate).abs() > 1e-4 {
        tts.player.set_rate(tts.settings.rate);
    }
    let voice_changed = tts.settings.engine != before.engine
        || tts.settings.voice != before.voice
        || tts.settings.volume != before.volume
        || tts.settings.sentence_pause_ms != before.sentence_pause_ms
        || tts.settings.roles != before.roles;
    if voice_changed {
        tts.player.configure(tts.player_config());
    }
    save(tts);
    cx.refresh_windows();
}

fn save(tts: &mut Tts) {
    // Never overwrite a file we could not parse; the user may want to fix it.
    if tts.settings_problem.is_some() {
        return;
    }
    if let Err(e) = tts.settings.save(&tts.settings_path) {
        log::error!("could not save settings: {e:#}");
        tts.notice = Some(format!("Could not save settings: {e:#}"));
    }
}

/// Read a short sample with the current voice.
pub fn preview_voice(cx: &mut App) {
    let name = {
        let tts = cx.global::<Tts>();
        tts.voices
            .iter()
            .find(|v| v.id == tts.settings.voice)
            .map(|v| v.name.clone())
    };
    let sample = match name {
        Some(name) => format!("Hello, this is {name}. This is how I sound at the current speed."),
        None => "Hello. This is how the default voice sounds at the current speed.".to_string(),
    };
    speak_text(&sample, "Voice preview", cx);
}
