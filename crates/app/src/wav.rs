//! Minimal RIFF/WAVE reader for `--analyze` (SPEC §6.4).
//!
//! Accepts 16/24/32-bit integer PCM and 32-bit IEEE float, in plain or
//! `WAVE_FORMAT_EXTENSIBLE` headers, and downmixes every frame to mono by
//! averaging the channels (the analysis runs on one mono signal).

use std::fmt;
use std::fs;
use std::path::Path;

/// A decoded mono signal.
#[derive(Debug, Clone)]
pub struct Wav {
    pub sample_rate: u32,
    /// Channel count of the file (after downmixing, [`Wav::samples`] is mono).
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl Wav {
    /// Duration in seconds.
    pub fn duration_s(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate.max(1) as f64
    }
}

/// Why a file could not be read as mono audio.
#[derive(Debug)]
pub struct WavError(String);

impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WavError {}

fn err<T>(message: impl Into<String>) -> Result<T, WavError> {
    Err(WavError(message.into()))
}

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// Header fields of the `fmt ` chunk that the decoder needs.
struct Format {
    code: u16,
    channels: u16,
    sample_rate: u32,
    bits: u16,
}

/// Reads `path` and returns its audio downmixed to mono.
pub fn read_mono(path: &Path) -> Result<Wav, WavError> {
    let bytes = fs::read(path)
        .map_err(|e| WavError(format!("cannot read {}: {e}", path.display())))?;
    decode(&bytes).map_err(|e| WavError(format!("{}: {e}", path.display())))
}

/// Decodes an in-memory WAV file.
pub fn decode(bytes: &[u8]) -> Result<Wav, WavError> {
    if bytes.len() < 12 {
        return err("not a WAV file (too short)");
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return err("not a RIFF/WAVE file");
    }

    let mut format: Option<Format> = None;
    let mut data: Option<&[u8]> = None;
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let declared = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let body = offset + 8;
        // Truncated files are common (a recording cut short): clamp instead of failing.
        let size = (declared as usize).min(bytes.len() - body);
        match id {
            b"fmt " => format = Some(parse_format(&bytes[body..body + size])?),
            b"data" => data = Some(&bytes[body..body + size]),
            _ => {}
        }
        offset = body + size + (size & 1);
    }

    let format = match format {
        Some(f) => f,
        None => return err("no fmt chunk"),
    };
    let data = match data {
        Some(d) => d,
        None => return err("no data chunk"),
    };
    if format.channels == 0 {
        return err("fmt chunk reports 0 channels");
    }
    if format.sample_rate == 0 {
        return err("fmt chunk reports a sample rate of 0 Hz");
    }

    let sample_bytes = match (format.code, format.bits) {
        (FORMAT_PCM, 16 | 24 | 32) => format.bits as usize / 8,
        (FORMAT_FLOAT, 32) => 4,
        (FORMAT_PCM, bits) => return err(format!("unsupported {bits}-bit integer PCM")),
        (FORMAT_FLOAT, bits) => return err(format!("unsupported {bits}-bit float PCM")),
        (code, bits) => return err(format!("unsupported WAV encoding {code} with {bits} bits")),
    };
    let channels = format.channels as usize;
    let frame_bytes = sample_bytes * channels;
    let frames = data.len() / frame_bytes;
    let mut samples = Vec::with_capacity(frames);
    let scale = 1.0 / channels as f32;
    for frame in data[..frames * frame_bytes].chunks_exact(frame_bytes) {
        let mut sum = 0.0f32;
        for sample in frame.chunks_exact(sample_bytes) {
            sum += decode_sample(format.code, sample);
        }
        samples.push(sum * scale);
    }

    Ok(Wav {
        sample_rate: format.sample_rate,
        channels: format.channels,
        samples,
    })
}

fn parse_format(chunk: &[u8]) -> Result<Format, WavError> {
    if chunk.len() < 16 {
        return err("fmt chunk is too short");
    }
    let code = u16::from_le_bytes([chunk[0], chunk[1]]);
    let channels = u16::from_le_bytes([chunk[2], chunk[3]]);
    let sample_rate = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
    let bits = u16::from_le_bytes([chunk[14], chunk[15]]);
    // WAVE_FORMAT_EXTENSIBLE stores the real encoding in the first two bytes of
    // the SubFormat GUID, after wValidBitsPerSample and dwChannelMask.
    let code = if code == FORMAT_EXTENSIBLE {
        if chunk.len() < 26 {
            return err("extensible fmt chunk is too short");
        }
        u16::from_le_bytes([chunk[24], chunk[25]])
    } else {
        code
    };
    Ok(Format {
        code,
        channels,
        sample_rate,
        bits,
    })
}

/// One sample of `bytes` (`2`, `3` or `4` bytes) scaled to `[-1, 1)`.
fn decode_sample(code: u16, bytes: &[u8]) -> f32 {
    match (code, bytes.len()) {
        (FORMAT_FLOAT, 4) => f32::from_le_bytes(bytes.try_into().unwrap()),
        (_, 2) => i16::from_le_bytes(bytes.try_into().unwrap()) as f32 / 32_768.0,
        (_, 3) => {
            // Sign-extend 24 bits into an i32.
            let raw = i32::from_le_bytes([0, bytes[0], bytes[1], bytes[2]]) >> 8;
            raw as f32 / 8_388_608.0
        }
        (_, 4) => i32::from_le_bytes(bytes.try_into().unwrap()) as f32 / 2_147_483_648.0,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a mono/stereo PCM16 or float32 WAV in memory.
    fn wav(
        code: u16,
        channels: u16,
        rate: u32,
        bits: u16,
        frames: &[Vec<f32>],
    ) -> Vec<u8> {
        let sample_bytes = bits as usize / 8;
        let mut data = Vec::new();
        for frame in frames {
            for &v in frame {
                match (code, sample_bytes) {
                    (FORMAT_FLOAT, 4) => data.extend_from_slice(&v.to_le_bytes()),
                    (_, 2) => {
                        data.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
                    }
                    (_, 3) => {
                        let raw = (v * 8_388_607.0) as i32;
                        data.extend_from_slice(&raw.to_le_bytes()[0..3]);
                    }
                    (_, _) => {
                        let raw = (v * 2_147_483_647.0) as i32;
                        data.extend_from_slice(&raw.to_le_bytes());
                    }
                }
            }
        }
        let block_align = channels as usize * sample_bytes;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&code.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * block_align as u32).to_le_bytes());
        out.extend_from_slice(&(block_align as u16).to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn pcm16_stereo_is_averaged() {
        let frames = vec![vec![0.5, 0.0], vec![-0.5, 0.0]];
        let w = decode(&wav(FORMAT_PCM, 2, 48_000, 16, &frames)).unwrap();
        assert_eq!((w.sample_rate, w.channels), (48_000, 2));
        assert_eq!(w.samples.len(), 2);
        assert!((w.samples[0] - 0.25).abs() < 1e-3, "{:?}", w.samples);
        assert!((w.samples[1] + 0.25).abs() < 1e-3, "{:?}", w.samples);
    }

    #[test]
    fn pcm24_and_pcm32() {
        let frames = vec![vec![0.5], vec![-0.25]];
        for bits in [24u16, 32] {
            let w = decode(&wav(FORMAT_PCM, 1, 44_100, bits, &frames)).unwrap();
            assert!((w.samples[0] - 0.5).abs() < 1e-3, "{bits} bit: {:?}", w.samples);
            assert!(
                (w.samples[1] + 0.25).abs() < 1e-3,
                "{bits} bit: {:?}",
                w.samples
            );
            assert_eq!(w.sample_rate, 44_100);
        }
    }

    #[test]
    fn float32() {
        let frames = vec![vec![0.5], vec![-0.25]];
        let w = decode(&wav(FORMAT_FLOAT, 1, 48_000, 32, &frames)).unwrap();
        assert_eq!(w.samples, vec![0.5, -0.25]);
    }

    #[test]
    fn extensible_header_uses_subformat() {
        let frames = vec![vec![0.5]];
        let base = wav(FORMAT_PCM, 1, 48_000, 16, &frames);
        // Rewrite the fmt chunk as extensible with a PCM subformat GUID.
        let mut out = Vec::new();
        out.extend_from_slice(&base[0..16]); // RIFF size WAVE "fmt "
        out.extend_from_slice(&40u32.to_le_bytes()); // fmt size
        out.extend_from_slice(&FORMAT_EXTENSIBLE.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&48_000u32.to_le_bytes());
        out.extend_from_slice(&96_000u32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        out.extend_from_slice(&16u16.to_le_bytes()); // valid bits
        out.extend_from_slice(&4u32.to_le_bytes()); // channel mask
        out.extend_from_slice(&FORMAT_PCM.to_le_bytes()); // SubFormat GUID
        out.extend_from_slice(&[0u8; 14]);
        out.extend_from_slice(&base[36..]); // data chunk
        let w = decode(&out).unwrap();
        assert!((w.samples[0] - 0.5).abs() < 1e-3, "{:?}", w.samples);
    }

    #[test]
    fn rejects_non_wav() {
        assert!(decode(b"not a wav file at all").is_err());
        assert!(decode(b"RIFF\x00\x00\x00\x00WAVE").is_err(), "no chunks");
        let mut damaged = wav(FORMAT_PCM, 1, 48_000, 16, &[vec![0.0]]);
        damaged[0] = b'X';
        assert!(decode(&damaged).is_err());
    }

    #[test]
    fn truncated_data_chunk_is_clamped() {
        let mut w = wav(FORMAT_PCM, 1, 48_000, 16, &[vec![0.5], vec![0.5]]);
        w.truncate(w.len() - 2); // lose the tail of the last sample
        assert_eq!(decode(&w).unwrap().samples.len(), 1);
    }

    #[test]
    fn unsupported_encoding_is_an_error() {
        let mut w = wav(FORMAT_PCM, 1, 48_000, 16, &[vec![0.5]]);
        // Replace the format tag (offset 20) with an unhandled code.
        w[20] = 7;
        let e = decode(&w).unwrap_err().to_string();
        assert!(e.contains("unsupported"), "{e}");
    }
}
