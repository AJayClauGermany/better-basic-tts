//! Core of Better Basic TTS.
//!
//! The crate is split into three layers that only talk to each other through
//! plain data:
//!
//! 1. [`text`] turns arbitrary input text into a list of [`text::Utterance`]s:
//!    an *interpreter* (plain text, Markdown, ...) assigns a [`text::Role`] to
//!    each block, the *speakable* pass makes symbols such as `<` and `>` safe
//!    and audible, and the *segmenter* cuts blocks into sentences.
//! 2. [`engine`] turns one utterance into PCM audio using a local speech
//!    engine. Nothing ever leaves the machine.
//! 3. [`player`] owns the transport: play, pause, stop, previous / next
//!    sentence, slower / faster.
//!
//! [`settings`] ties the three together and is what the UI edits.

pub mod engine;
pub mod player;
pub mod settings;
pub mod text;

pub use engine::{Audio, Engine, EngineRegistry, SynthRequest, Voice};
pub use player::{Player, PlayerSnapshot, Status};
pub use settings::Settings;
pub use text::{Role, Utterance};
