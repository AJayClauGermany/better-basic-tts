//! Our own icons, layered over the component library's default set.
//!
//! The transport glyphs are drawn solid so they read at a glance, like the
//! classic speech controller they are modelled on.

use gpui_kit::component::Icon;
use gpui_kit::{AssetSource, Result, SharedString};
use std::borrow::Cow;

const PREFIX: &str = "bbtts/";

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $((
                concat!("bbtts/", $name, ".svg"),
                include_bytes!(concat!("../assets/icons/", $name, ".svg")),
            )),*
        ];
    };
}

icons!("play", "pause", "stop", "rewind", "forward", "turtle", "rabbit");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    Play,
    Pause,
    Stop,
    Rewind,
    Forward,
    Turtle,
    Rabbit,
}

impl Glyph {
    fn path(self) -> &'static str {
        match self {
            Glyph::Play => "bbtts/play.svg",
            Glyph::Pause => "bbtts/pause.svg",
            Glyph::Stop => "bbtts/stop.svg",
            Glyph::Rewind => "bbtts/rewind.svg",
            Glyph::Forward => "bbtts/forward.svg",
            Glyph::Turtle => "bbtts/turtle.svg",
            Glyph::Rabbit => "bbtts/rabbit.svg",
        }
    }

    pub fn icon(self) -> Icon {
        Icon::empty().path(self.path())
    }
}

/// Serves `bbtts/*.svg` from the binary and everything else from the
/// component library's bundled assets.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.starts_with(PREFIX) {
            return Ok(ICONS
                .iter()
                .find(|(name, _)| *name == path)
                .map(|(_, bytes)| Cow::Borrowed(*bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = gpui_kit::assets::Assets.list(path)?;
        all.extend(
            ICONS
                .iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
        Ok(all)
    }
}
