//! A small, forgiving WAV reader / writer.
//!
//! Speech tools that stream to stdout (espeak-ng, Piper) write placeholder
//! chunk sizes because they cannot seek back, so a strict parser rejects
//! their output. This one walks the chunks and takes whatever data is there.

use super::Audio;
use anyhow::{bail, Result};

pub fn parse(bytes: &[u8]) -> Result<Audio> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("not a WAV file ({} bytes)", bytes.len());
    }
    let mut pos = 12;
    let mut format: Option<(u16, u16, u32, u16)> = None; // tag, channels, rate, bits
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let declared = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;
        let available = bytes.len() - body;
        let size = declared.min(available);
        match id {
            b"fmt " => {
                if size < 16 {
                    bail!("WAV fmt chunk too short");
                }
                let f = &bytes[body..body + size];
                let mut tag = u16::from_le_bytes([f[0], f[1]]);
                let channels = u16::from_le_bytes([f[2], f[3]]);
                let rate = u32::from_le_bytes([f[4], f[5], f[6], f[7]]);
                let bits = u16::from_le_bytes([f[14], f[15]]);
                // WAVE_FORMAT_EXTENSIBLE: the real tag is the start of the GUID.
                if tag == 0xFFFE && size >= 26 {
                    tag = u16::from_le_bytes([f[24], f[25]]);
                }
                format = Some((tag, channels, rate, bits));
            }
            b"data" => {
                let Some((tag, channels, rate, bits)) = format else {
                    bail!("WAV data chunk before fmt chunk");
                };
                if channels == 0 || rate == 0 {
                    bail!("WAV with zero channels or sample rate");
                }
                let samples = decode(&bytes[body..body + size], tag, bits)?;
                return Ok(Audio {
                    sample_rate: rate,
                    samples: downmix(samples, channels as usize),
                });
            }
            _ => {}
        }
        // Chunks are word aligned.
        pos = body + size + (size & 1);
    }
    bail!("WAV file has no data chunk")
}

fn decode(data: &[u8], tag: u16, bits: u16) -> Result<Vec<f32>> {
    Ok(match (tag, bits) {
        (1, 8) => data.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect(),
        (1, 16) => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect(),
        (1, 24) => data
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| {
                let v = i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8;
                v as f32 / 8_388_608.0
            })
            .collect(),
        (1, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0)
            .collect(),
        (3, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        (3, 64) => data
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| f64::from_le_bytes(*c) as f32)
            .collect(),
        _ => bail!("unsupported WAV encoding (format {tag}, {bits} bits)"),
    })
}

fn downmix(samples: Vec<f32>, channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples;
    }
    samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Interpret raw little-endian 16-bit mono PCM.
pub fn from_raw_s16le(bytes: &[u8], sample_rate: u32) -> Audio {
    Audio {
        sample_rate,
        samples: bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect(),
    }
}

/// Encode as 16-bit mono PCM WAV.
pub fn encode(audio: &Audio) -> Vec<u8> {
    let data_len = (audio.samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&audio.sample_rate.to_le_bytes());
    out.extend_from_slice(&(audio.sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in &audio.samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let audio = Audio {
            sample_rate: 22050,
            samples: vec![0.0, 0.5, -0.5, 0.25],
        };
        let parsed = parse(&encode(&audio)).unwrap();
        assert_eq!(parsed.sample_rate, 22050);
        assert_eq!(parsed.samples.len(), 4);
        assert!((parsed.samples[1] - 0.5).abs() < 0.001);
    }

    #[test]
    fn tolerates_streamed_sizes_and_extra_chunks() {
        let audio = Audio {
            sample_rate: 16000,
            samples: vec![0.1; 10],
        };
        let mut bytes = encode(&audio);
        // Bogus sizes like a streaming writer would leave.
        bytes[4..8].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
        bytes[40..44].copy_from_slice(&0x7fff_f000u32.to_le_bytes());
        // Insert a LIST chunk before data.
        let mut with_list = bytes[..36].to_vec();
        with_list.extend_from_slice(b"LIST");
        with_list.extend_from_slice(&3u32.to_le_bytes());
        with_list.extend_from_slice(b"abc\0");
        with_list.extend_from_slice(&bytes[36..]);
        let parsed = parse(&with_list).unwrap();
        assert_eq!(parsed.samples.len(), 10);
    }

    #[test]
    fn stereo_is_downmixed() {
        let mut bytes = encode(&Audio {
            sample_rate: 8000,
            samples: vec![1.0, 0.0, 0.5, 0.5],
        });
        bytes[22..24].copy_from_slice(&2u16.to_le_bytes());
        let parsed = parse(&bytes).unwrap();
        assert_eq!(parsed.samples.len(), 2);
        assert!((parsed.samples[0] - 0.5).abs() < 0.001);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(b"hello").is_err());
        assert!(parse(b"RIFF\0\0\0\0WAVE").is_err());
    }
}
