//! Playdate `.pda` audio decoder (`Playdate AUD`).
//!
//! Format: [cranksters pda.md](https://github.com/cranksters/playdate-reverse-engineering).
//! Zipper's `Sounds/*.pda` samples used so far are 16-bit mono PCM @ 22050 Hz
//! (`select`, `buzz`, `step`, …). ADPCM is parsed enough to reject clearly.

/// Decoded PCM ready for the host (Web Audio, etc.).
#[derive(Clone, Debug)]
pub struct PcmSample {
    pub sample_rate: u32,
    pub channels: u8,
    /// Interleaved signed 16-bit little-endian samples.
    pub samples: Vec<i16>,
}

impl PcmSample {
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            return 0;
        }
        self.samples.len() / self.channels as usize
    }

    /// Mono (or first channel) samples as f32 in [-1, 1] for Web Audio.
    pub fn to_f32_mono(&self) -> Vec<f32> {
        if self.channels <= 1 {
            return self
                .samples
                .iter()
                .map(|&s| s as f32 / 32768.0)
                .collect();
        }
        self.samples
            .chunks_exact(self.channels as usize)
            .map(|frame| frame[0] as f32 / 32768.0)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdaError {
    BadMagic,
    Truncated,
    UnsupportedFormat(u8),
    Empty,
}

impl core::fmt::Display for PdaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PdaError::BadMagic => write!(f, "pda: bad magic"),
            PdaError::Truncated => write!(f, "pda: truncated"),
            PdaError::UnsupportedFormat(v) => write!(f, "pda: unsupported format {v}"),
            PdaError::Empty => write!(f, "pda: empty"),
        }
    }
}

impl std::error::Error for PdaError {}

const PDA_MAGIC: &[u8; 12] = b"Playdate AUD";

/// `playdate.sound` format byte at offset 15.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PdaFormat {
    Pcm8Mono = 0,
    Pcm8Stereo = 1,
    Pcm16Mono = 2,
    Pcm16Stereo = 3,
    AdpcmMono = 4,
    AdpcmStereo = 5,
}

impl PdaFormat {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Pcm8Mono),
            1 => Some(Self::Pcm8Stereo),
            2 => Some(Self::Pcm16Mono),
            3 => Some(Self::Pcm16Stereo),
            4 => Some(Self::AdpcmMono),
            5 => Some(Self::AdpcmStereo),
            _ => None,
        }
    }

    fn channels(self) -> u8 {
        match self {
            Self::Pcm8Mono | Self::Pcm16Mono | Self::AdpcmMono => 1,
            Self::Pcm8Stereo | Self::Pcm16Stereo | Self::AdpcmStereo => 2,
        }
    }
}

/// Decode a `.pda` file to signed 16-bit PCM.
pub fn decode_pda(data: &[u8]) -> Result<PcmSample, PdaError> {
    if data.len() < 16 {
        return Err(PdaError::Truncated);
    }
    if &data[0..12] != PDA_MAGIC {
        return Err(PdaError::BadMagic);
    }
    let sample_rate = u32::from(data[12]) | (u32::from(data[13]) << 8) | (u32::from(data[14]) << 16);
    let fmt = PdaFormat::from_u8(data[15]).ok_or(PdaError::UnsupportedFormat(data[15]))?;
    let payload = &data[16..];
    if payload.is_empty() {
        return Err(PdaError::Empty);
    }

    let samples = match fmt {
        PdaFormat::Pcm16Mono | PdaFormat::Pcm16Stereo => decode_pcm16(payload)?,
        PdaFormat::Pcm8Mono | PdaFormat::Pcm8Stereo => decode_pcm8(payload),
        PdaFormat::AdpcmMono | PdaFormat::AdpcmStereo => {
            return Err(PdaError::UnsupportedFormat(fmt as u8));
        }
    };

    if samples.is_empty() {
        return Err(PdaError::Empty);
    }

    Ok(PcmSample {
        sample_rate,
        channels: fmt.channels(),
        samples,
    })
}

fn decode_pcm16(payload: &[u8]) -> Result<Vec<i16>, PdaError> {
    if payload.len() % 2 != 0 {
        return Err(PdaError::Truncated);
    }
    let mut out = Vec::with_capacity(payload.len() / 2);
    for chunk in payload.chunks_exact(2) {
        out.push(i16::from_le_bytes([chunk[0], chunk[1]]));
    }
    Ok(out)
}

fn decode_pcm8(payload: &[u8]) -> Vec<i16> {
    payload
        .iter()
        .map(|&s| {
            let signed = s as i16 - 128;
            signed << 8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_select_pda_if_present() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ref/Zipper.pdx/Sounds/select.pda"
        );
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        let pcm = decode_pda(&data).expect("select.pda");
        assert_eq!(pcm.sample_rate, 22050);
        assert_eq!(pcm.channels, 1);
        // 212 payload bytes → 106 samples (~4.8 ms at 22050)
        assert_eq!(pcm.samples.len(), 106);
        let f32s = pcm.to_f32_mono();
        assert_eq!(f32s.len(), 106);
        assert!(f32s.iter().any(|&s| s != 0.0));
    }
}
