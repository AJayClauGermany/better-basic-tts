//! System-wide keyboard shortcuts.

use anyhow::{anyhow, Result};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// What a shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Read the selection, or stop if something is being read.
    SpeakSelection,
    SpeakClipboard,
    PauseResume,
    Stop,
    Previous,
    Next,
    Slower,
    Faster,
}

impl Action {
    pub const ALL: [Action; 8] = [
        Action::SpeakSelection,
        Action::SpeakClipboard,
        Action::PauseResume,
        Action::Stop,
        Action::Previous,
        Action::Next,
        Action::Slower,
        Action::Faster,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::SpeakSelection => "Speak selection / stop",
            Action::SpeakClipboard => "Speak clipboard",
            Action::PauseResume => "Pause / resume",
            Action::Stop => "Stop",
            Action::Previous => "Previous sentence",
            Action::Next => "Next sentence",
            Action::Slower => "Slower",
            Action::Faster => "Faster",
        }
    }
}

/// Parse a shortcut such as `Ctrl+Alt+S`, `Cmd+Shift+S` or `Alt+Esc`.
pub fn parse(shortcut: &str) -> Result<HotKey> {
    let normalized = shortcut
        .split('+')
        .map(|token| {
            let token = token.trim();
            match token.to_ascii_uppercase().as_str() {
                "ESC" => "Escape".to_string(),
                "WIN" | "WINDOWS" => "Super".to_string(),
                "OPT" => "Option".to_string(),
                "RETURN" => "Enter".to_string(),
                "SPACEBAR" => "Space".to_string(),
                "DEL" => "Delete".to_string(),
                "PLUS" => "=".to_string(),
                _ => token.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("+");
    normalized
        .parse::<HotKey>()
        .map_err(|e| anyhow!("\"{shortcut}\" is not a valid shortcut: {e}"))
}

/// Why global shortcuts cannot work in this session, if they cannot.
pub fn unsupported_reason() -> Option<&'static str> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return Some(
                "Wayland does not let apps register global shortcuts. Bind \
                 `better-basic-tts speak-selection` to a key in your desktop's keyboard settings instead.",
            );
        }
        if std::env::var_os("DISPLAY").is_none() {
            return Some("No display found, global shortcuts are off.");
        }
    }
    None
}

type Bindings = Arc<Mutex<HashMap<u32, Action>>>;

/// Owns the registered shortcuts. Create it on the main thread and keep it
/// alive for as long as the shortcuts should work.
pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    registered: Vec<HotKey>,
    bindings: Bindings,
}

impl Hotkeys {
    /// `on_action` is called from an arbitrary thread when a shortcut fires.
    pub fn new(on_action: impl Fn(Action) + Send + Sync + 'static) -> Result<Self> {
        if let Some(reason) = unsupported_reason() {
            return Err(anyhow!(reason));
        }
        let manager = GlobalHotKeyManager::new()
            .map_err(|e| anyhow!("could not set up global shortcuts: {e}"))?;
        let bindings: Bindings = Arc::default();
        let lookup = bindings.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.state() != HotKeyState::Pressed {
                return;
            }
            let action = lookup.lock().unwrap().get(&event.id()).copied();
            if let Some(action) = action {
                on_action(action);
            }
        }));
        Ok(Self {
            manager,
            registered: Vec::new(),
            bindings,
        })
    }

    /// Replace all shortcuts. Returns one message per shortcut that could
    /// not be registered (invalid, or taken by another application).
    pub fn apply(&mut self, shortcuts: &[(Action, String)]) -> Vec<String> {
        for hotkey in self.registered.drain(..) {
            let _ = self.manager.unregister(hotkey);
        }
        let mut bindings = self.bindings.lock().unwrap();
        bindings.clear();
        let mut problems = Vec::new();
        for (action, text) in shortcuts {
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let hotkey = match parse(text) {
                Ok(h) => h,
                Err(e) => {
                    problems.push(format!("{}: {e}", action.label()));
                    continue;
                }
            };
            if bindings.contains_key(&hotkey.id()) {
                problems.push(format!(
                    "{}: \"{text}\" is already used by another action",
                    action.label()
                ));
                continue;
            }
            match self.manager.register(hotkey) {
                Ok(()) => {
                    bindings.insert(hotkey.id(), *action);
                    self.registered.push(hotkey);
                }
                Err(e) => problems.push(format!(
                    "{}: could not register \"{text}\" ({e})",
                    action.label()
                )),
            }
        }
        problems
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        for hotkey in self.registered.drain(..) {
            let _ = self.manager.unregister(hotkey);
        }
        GlobalHotKeyEvent::set_event_handler::<fn(GlobalHotKeyEvent)>(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_friendly_spellings() {
        assert!(parse("Ctrl+Alt+S").is_ok());
        assert!(parse("cmd+shift+s").is_ok());
        assert!(parse("CmdOrCtrl+Shift+S").is_ok());
        assert!(parse("Alt+Esc").is_ok());
        assert!(parse("Ctrl + Alt + P").is_ok());
        assert!(parse("F9").is_ok());
        assert_eq!(
            parse("Ctrl+Alt+S").unwrap().id(),
            parse("alt+control+KeyS").unwrap().id()
        );
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse("Ctrl+Alt+").is_err());
        assert!(parse("Ctrl+S+A").is_err());
        assert!(parse("Banana").is_err());
    }
}
