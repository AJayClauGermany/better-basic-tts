//! Windows system voices through `Windows.Media.SpeechSynthesis`.
//!
//! `SynthesizeTextToStreamAsync` takes *plain text* (SSML has its own, separate
//! entry point which we never call) and returns a WAV stream in memory.

use super::{wav, Audio, Engine, SynthRequest, Voice};
use anyhow::{anyhow, Context, Result};
use windows::core::HSTRING;
use windows::Media::SpeechSynthesis::SpeechSynthesizer;
use windows::Storage::Streams::DataReader;

pub struct WinRtEngine;

impl WinRtEngine {
    pub fn detect() -> Option<Self> {
        match SpeechSynthesizer::AllVoices().and_then(|v| v.Size()) {
            Ok(n) if n > 0 => Some(Self),
            Ok(_) => None,
            Err(err) => {
                log::warn!("Windows speech synthesis unavailable: {err}");
                None
            }
        }
    }
}

impl Engine for WinRtEngine {
    fn id(&self) -> &str {
        "winrt"
    }

    fn name(&self) -> &str {
        "Windows system voices"
    }

    fn voices(&self) -> Result<Vec<Voice>> {
        let all = SpeechSynthesizer::AllVoices()?;
        let mut voices = Vec::new();
        for v in &all {
            voices.push(Voice {
                id: v.Id()?.to_string(),
                name: v.DisplayName()?.to_string(),
                language: v.Language()?.to_string(),
            });
        }
        Ok(voices)
    }

    fn synthesize(&self, req: &SynthRequest) -> Result<Audio> {
        let synth = SpeechSynthesizer::new().context("could not create speech synthesizer")?;
        if let Some(id) = req.voice.as_deref().filter(|v| !v.is_empty()) {
            let all = SpeechSynthesizer::AllVoices()?;
            for v in &all {
                if v.Id()?.to_string() == id {
                    synth.SetVoice(&v)?;
                    break;
                }
            }
        }
        let options = synth.Options()?;
        // Not every Windows build supports these; ignore failures.
        let _ = options.SetSpeakingRate(req.rate.clamp(0.5, 6.0) as f64);
        let _ = options.SetAudioPitch(req.pitch.clamp(0.0, 2.0) as f64);
        let _ = options.SetAudioVolume(req.volume.clamp(0.0, 1.0) as f64);
        let _ = options.SetIncludeSentenceBoundaryMetadata(false);
        let _ = options.SetIncludeWordBoundaryMetadata(false);

        let stream = synth
            .SynthesizeTextToStreamAsync(&HSTRING::from(req.text.as_str()))?
            .join()
            .map_err(|e| anyhow!("speech synthesis failed: {e}"))?;
        let size = stream.Size()? as u32;
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
        reader.LoadAsync(size)?.join()?;
        let mut bytes = vec![0u8; size as usize];
        reader.ReadBytes(&mut bytes)?;
        wav::parse(&bytes).context("Windows produced unreadable audio")
    }
}
