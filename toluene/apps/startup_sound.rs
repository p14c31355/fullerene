//! Fullerene's boot sound player.
//!
//! The application deliberately owns the file read and WAV parsing. The
//! kernel only exposes a small PCM playback import, keeping boot audio on the
//! same WASI/WASM path as other user applications.

use std::fs;

const SOUND_PATH: &str = "/usr/share/sounds/fullerene/fullerene_startup_sound.wav";

#[link(wasm_import_module = "fullerene")]
unsafe extern "C" {
    fn play_pcm(
        sample_rate: u32,
        channels: u32,
        bits_per_sample: u32,
        data_ptr: *const u8,
        data_len: u32,
    ) -> u32;
}

fn read_le_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset + 1)?,
    ]))
}

fn read_le_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset + 1)?,
        *bytes.get(offset + 2)?,
        *bytes.get(offset + 3)?,
    ]))
}

fn parse_wav(bytes: &[u8]) -> Result<(u32, u32, u32, &[u8]), &'static str> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file");
    }

    let mut offset = 12usize;
    let mut format = None;
    let mut data = None;
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let chunk_size = read_le_u32(bytes, offset + 4).ok_or("truncated WAV chunk")? as usize;
        let chunk_start = offset + 8;
        let chunk_end = chunk_start
            .checked_add(chunk_size)
            .ok_or("WAV chunk size overflow")?;
        if chunk_end > bytes.len() {
            return Err("WAV chunk exceeds file");
        }

        match &bytes[offset..offset + 4] {
            b"fmt " if chunk_size >= 16 => {
                let audio_format = read_le_u16(bytes, chunk_start).ok_or("bad WAV format")?;
                let channels = read_le_u16(bytes, chunk_start + 2).ok_or("bad WAV channels")?;
                let sample_rate = read_le_u32(bytes, chunk_start + 4).ok_or("bad WAV rate")?;
                let bits = read_le_u16(bytes, chunk_start + 14).ok_or("bad WAV depth")?;
                format = Some((audio_format, channels, sample_rate, bits));
            }
            b"data" => data = Some(&bytes[chunk_start..chunk_end]),
            _ => {}
        }

        // RIFF chunks are word aligned, even when their declared payload is
        // odd-sized.
        offset = chunk_end.saturating_add(chunk_size & 1);
    }

    let (audio_format, channels, sample_rate, bits) = format.ok_or("WAV fmt chunk missing")?;
    if audio_format != 1 {
        return Err("WAV is not uncompressed PCM");
    }
    let data = data.ok_or("WAV data chunk missing")?;
    if channels == 0 || bits == 0 || data.is_empty() || data.len() % 2 != 0 {
        return Err("WAV PCM payload is invalid");
    }
    Ok((sample_rate, channels as u32, bits as u32, data))
}

fn main() {
    let bytes = match fs::read(SOUND_PATH) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("startup sound: cannot read {}: {}", SOUND_PATH, error);
            std::process::exit(1);
        }
    };
    let (sample_rate, channels, bits, pcm) = match parse_wav(&bytes) {
        Ok(format) => format,
        Err(error) => {
            eprintln!("startup sound: {}", error);
            std::process::exit(1);
        }
    };
    println!(
        "startup sound: {} Hz, {} channel(s), {} bit, {} PCM bytes",
        sample_rate,
        channels,
        bits,
        pcm.len()
    );

    let code = unsafe { play_pcm(sample_rate, channels, bits, pcm.as_ptr(), pcm.len() as u32) };
    if code != 0 {
        eprintln!("startup sound: playback failed (code {})", code);
        std::process::exit(1);
    }
}
