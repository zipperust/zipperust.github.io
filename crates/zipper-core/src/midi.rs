//! Minimal Standard MIDI File (SMF) parser for Zipper's `Sounds/Sho.mid`.
//!
//! Supports format 0/1, running status, tempo meta (`FF 51`), note on/off
//! (including note-on velocity 0 = off). Other events are skipped.
//! Timing is converted to seconds using the file division + tempo map.

use std::fmt;

/// One timed note event after merging all tracks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MidiEv {
    /// Absolute time from sequence start (seconds).
    pub t: f64,
    /// `true` = note on, `false` = note off.
    pub on: bool,
    pub note: u8,
}

/// Parsed MIDI sequence ready for playback.
#[derive(Debug, Clone, Default)]
pub struct MidiSequence {
    pub events: Vec<MidiEv>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiError {
    Truncated,
    BadHeader,
    BadChunk,
    UnsupportedDivision,
}

impl fmt::Display for MidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MidiError::Truncated => write!(f, "midi: truncated"),
            MidiError::BadHeader => write!(f, "midi: bad MThd"),
            MidiError::BadChunk => write!(f, "midi: bad chunk"),
            MidiError::UnsupportedDivision => write!(f, "midi: SMPTE division unsupported"),
        }
    }
}

impl std::error::Error for MidiError {}

/// Parse SMF bytes into a time-ordered note sequence.
pub fn parse_smf(data: &[u8]) -> Result<MidiSequence, MidiError> {
    if data.len() < 14 {
        return Err(MidiError::Truncated);
    }
    if &data[0..4] != b"MThd" {
        return Err(MidiError::BadHeader);
    }
    let hdr_len = read_u32(data, 4)? as usize;
    if hdr_len < 6 || data.len() < 8 + hdr_len {
        return Err(MidiError::Truncated);
    }
    let _format = read_u16(data, 8)?;
    let ntracks = read_u16(data, 10)? as usize;
    let division = read_u16(data, 12)?;
    if division & 0x8000 != 0 {
        return Err(MidiError::UnsupportedDivision);
    }
    let tpq = division as u32; // ticks per quarter note
    if tpq == 0 {
        return Err(MidiError::BadHeader);
    }

    let mut off = 8 + hdr_len;
    // Collect (tick, on, note) from all tracks, plus tempo changes (tick, us_per_qn).
    let mut raw: Vec<(u32, bool, u8)> = Vec::new();
    let mut tempos: Vec<(u32, u32)> = Vec::new(); // (tick, microseconds per quarter)

    for _ in 0..ntracks {
        if off + 8 > data.len() {
            return Err(MidiError::Truncated);
        }
        if &data[off..off + 4] != b"MTrk" {
            return Err(MidiError::BadChunk);
        }
        let tlen = read_u32(data, off + 4)? as usize;
        off += 8;
        if off + tlen > data.len() {
            return Err(MidiError::Truncated);
        }
        let track = &data[off..off + tlen];
        parse_track(track, &mut raw, &mut tempos)?;
        off += tlen;
    }

    raw.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    tempos.sort_by_key(|&(t, _)| t);
    if tempos.is_empty() {
        tempos.push((0, 500_000)); // default 120 bpm
    } else if tempos[0].0 != 0 {
        tempos.insert(0, (0, 500_000));
    }

    let events = ticks_to_seconds(&raw, &tempos, tpq);
    Ok(MidiSequence { events })
}

fn parse_track(
    track: &[u8],
    raw: &mut Vec<(u32, bool, u8)>,
    tempos: &mut Vec<(u32, u32)>,
) -> Result<(), MidiError> {
    let mut i = 0usize;
    let mut tick: u32 = 0;
    let mut running: u8 = 0;

    while i < track.len() {
        let (delta, ni) = read_vlq(track, i)?;
        i = ni;
        tick = tick.saturating_add(delta);

        if i >= track.len() {
            return Err(MidiError::Truncated);
        }
        let mut status = track[i];
        if status < 0x80 {
            if running == 0 {
                return Err(MidiError::BadChunk);
            }
            status = running;
        } else {
            i += 1;
            running = status;
        }

        if status == 0xFF {
            // Meta
            if i >= track.len() {
                return Err(MidiError::Truncated);
            }
            let meta = track[i];
            i += 1;
            let (len, ni) = read_vlq(track, i)?;
            i = ni;
            let len = len as usize;
            if i + len > track.len() {
                return Err(MidiError::Truncated);
            }
            let payload = &track[i..i + len];
            i += len;
            if meta == 0x51 && payload.len() == 3 {
                let us = ((payload[0] as u32) << 16)
                    | ((payload[1] as u32) << 8)
                    | (payload[2] as u32);
                tempos.push((tick, us));
            }
            if meta == 0x2F {
                break; // end of track
            }
        } else if status == 0xF0 || status == 0xF7 {
            let (len, ni) = read_vlq(track, i)?;
            i = ni;
            let len = len as usize;
            if i + len > track.len() {
                return Err(MidiError::Truncated);
            }
            i += len;
        } else {
            let et = status & 0xF0;
            match et {
                0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => {
                    if i + 2 > track.len() {
                        return Err(MidiError::Truncated);
                    }
                    let a = track[i];
                    let b = track[i + 1];
                    i += 2;
                    if et == 0x90 {
                        if b == 0 {
                            raw.push((tick, false, a));
                        } else {
                            raw.push((tick, true, a));
                        }
                    } else if et == 0x80 {
                        raw.push((tick, false, a));
                    }
                }
                0xC0 | 0xD0 => {
                    if i + 1 > track.len() {
                        return Err(MidiError::Truncated);
                    }
                    i += 1;
                }
                _ => {
                    // Unknown — bail rather than desync
                    return Err(MidiError::BadChunk);
                }
            }
        }
    }
    Ok(())
}

fn ticks_to_seconds(raw: &[(u32, bool, u8)], tempos: &[(u32, u32)], tpq: u32) -> Vec<MidiEv> {
    let mut out = Vec::with_capacity(raw.len());
    let mut tempo_i = 0usize;
    let mut cur_us = tempos[0].1;
    let mut last_tick: u32 = 0;
    let mut elapsed_sec = 0.0f64;

    for &(tick, on, note) in raw {
        // Advance through tempo changes up to `tick`.
        while tempo_i + 1 < tempos.len() && tempos[tempo_i + 1].0 <= tick {
            let next_tick = tempos[tempo_i + 1].0;
            let dticks = next_tick.saturating_sub(last_tick);
            elapsed_sec += ticks_to_sec(dticks, cur_us, tpq);
            last_tick = next_tick;
            tempo_i += 1;
            cur_us = tempos[tempo_i].1;
        }
        let dticks = tick.saturating_sub(last_tick);
        elapsed_sec += ticks_to_sec(dticks, cur_us, tpq);
        last_tick = tick;
        out.push(MidiEv {
            t: elapsed_sec,
            on,
            note,
        });
    }
    out
}

fn ticks_to_sec(ticks: u32, us_per_qn: u32, tpq: u32) -> f64 {
    (ticks as f64) * (us_per_qn as f64) / (tpq as f64) / 1_000_000.0
}

fn read_u16(data: &[u8], off: usize) -> Result<u16, MidiError> {
    if off + 2 > data.len() {
        return Err(MidiError::Truncated);
    }
    Ok(u16::from_be_bytes([data[off], data[off + 1]]))
}

fn read_u32(data: &[u8], off: usize) -> Result<u32, MidiError> {
    if off + 4 > data.len() {
        return Err(MidiError::Truncated);
    }
    Ok(u32::from_be_bytes([
        data[off],
        data[off + 1],
        data[off + 2],
        data[off + 3],
    ]))
}

fn read_vlq(data: &[u8], mut i: usize) -> Result<(u32, usize), MidiError> {
    let mut val: u32 = 0;
    for _ in 0..4 {
        if i >= data.len() {
            return Err(MidiError::Truncated);
        }
        let b = data[i];
        i += 1;
        val = (val << 7) | u32::from(b & 0x7F);
        if b < 0x80 {
            return Ok((val, i));
        }
    }
    Err(MidiError::BadChunk)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal format-1 SMF: one tempo + two notes.
    /// Track 0: tempo 500000 @0, end.
    /// Track 1: note 60 on @0, off @480 (quarter at 120bpm = 0.5s), end.
    fn tiny_smf() -> Vec<u8> {
        let mut out = Vec::new();
        // MThd
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // format 1
        out.extend_from_slice(&2u16.to_be_bytes()); // 2 tracks
        out.extend_from_slice(&480u16.to_be_bytes()); // division

        // Track 0 — tempo
        let mut t0 = Vec::new();
        t0.push(0x00); // delta 0
        t0.extend_from_slice(&[0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]); // 500000
        t0.push(0x00);
        t0.extend_from_slice(&[0xFF, 0x2F, 0x00]);
        out.extend_from_slice(b"MTrk");
        out.extend_from_slice(&(t0.len() as u32).to_be_bytes());
        out.extend_from_slice(&t0);

        // Track 1 — note
        let mut t1 = Vec::new();
        t1.push(0x00);
        t1.extend_from_slice(&[0x90, 60, 0x64]); // note on
        // delta 480 = 0x83 0x60
        t1.push(0x83);
        t1.push(0x60);
        t1.extend_from_slice(&[0x80, 60, 0x40]); // note off
        t1.push(0x00);
        t1.extend_from_slice(&[0xFF, 0x2F, 0x00]);
        out.extend_from_slice(b"MTrk");
        out.extend_from_slice(&(t1.len() as u32).to_be_bytes());
        out.extend_from_slice(&t1);

        out
    }

    #[test]
    fn parse_tiny_fixture() {
        let seq = parse_smf(&tiny_smf()).expect("parse");
        assert_eq!(seq.events.len(), 2);
        assert!(seq.events[0].on);
        assert_eq!(seq.events[0].note, 60);
        assert!((seq.events[0].t - 0.0).abs() < 1e-9);
        assert!(!seq.events[1].on);
        assert!((seq.events[1].t - 0.5).abs() < 1e-6);
    }

    #[test]
    fn parse_sho_mid_when_present() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../ref/Zipper.pdx/Sounds/Sho.mid");
        let Ok(data) = std::fs::read(&path) else {
            return;
        };
        let seq = parse_smf(&data).expect("parse Sho.mid");
        let ons = seq.events.iter().filter(|e| e.on).count();
        let offs = seq.events.iter().filter(|e| !e.on).count();
        assert_eq!(ons, 240, "Sho.mid note-ons");
        assert_eq!(offs, 240, "Sho.mid note-offs");
        assert!(seq.events[0].on);
        // First note shortly after start; last note ~241s.
        assert!(seq.events[0].t < 1.0);
        let last_t = seq.events.last().unwrap().t;
        assert!(
            (240.0..245.0).contains(&last_t),
            "last event ~241s, got {last_t}"
        );
    }
}
