//! Benchmark the actual WASI viewer on the native wasmi engine.
//!
//! This bypasses the Fullerene kernel, VFS, Klog, and event loop. The host
//! callbacks use a local byte buffer and a small RGB blit loop, while the
//! viewer itself is the same `viewer.wasm` shipped in the system image.

use genome::FsError;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use wasi_runtime::runtime;
use wasi_runtime::wasi::WasiHost;

static MEDIA: OnceLock<Vec<u8>> = OnceLock::new();
static CLOCK_START: OnceLock<Instant> = OnceLock::new();
static FRAME_COUNT: AtomicU64 = AtomicU64::new(0);
static BLIT_CHECKSUM: AtomicU64 = AtomicU64::new(0);
static READ_CALLS: AtomicU64 = AtomicU64::new(0);
static READ_BYTES: AtomicU64 = AtomicU64::new(0);
static READ_TIME_NS: AtomicU64 = AtomicU64::new(0);
static UPDATE_CALLS: AtomicU64 = AtomicU64::new(0);
static UPDATE_TIME_NS: AtomicU64 = AtomicU64::new(0);
static OUTPUT: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
static READ_GRANULARITY: AtomicU64 = AtomicU64::new(64 * 1024);

fn main() {
    let mut args = std::env::args().skip(1);
    let viewer_path = args
        .next()
        .expect("usage: cargo run --release --example bench_wasmi -- <viewer.wasm> <video.mp4>");
    let media_path = args
        .next()
        .expect("usage: cargo run --release --example bench_wasmi -- <viewer.wasm> <video.mp4>");
    let viewer_mode = args.next().unwrap_or_else(|| "--bench=full".to_string());
    let read_mode = args.next().unwrap_or_else(|| "--read=64k".to_string());
    READ_GRANULARITY.store(
        if read_mode == "--read=4k" {
            4 * 1024
        } else {
            64 * 1024
        },
        Ordering::Relaxed,
    );
    OUTPUT
        .set(Mutex::new(Vec::new()))
        .expect("benchmark output initialized twice");
    let viewer = std::fs::read(&viewer_path).expect("viewer.wasm read failed");
    let media = std::fs::read(&media_path).expect("video read failed");
    MEDIA.set(media).expect("benchmark media initialized twice");
    CLOCK_START
        .set(Instant::now())
        .expect("benchmark clock initialized twice");

    let start = Instant::now();
    let code = runtime::run(
        &viewer,
        &[
            "/apps/viewer.wasm",
            "/bench/video.mp4",
            viewer_mode.as_str(),
        ],
        host(),
    );
    let elapsed = start.elapsed();
    let frames = FRAME_COUNT.load(Ordering::Relaxed);
    let checksum = BLIT_CHECKSUM.load(Ordering::Relaxed);

    println!("viewer_bytes={}", viewer.len());
    println!("media_bytes={}", MEDIA.get().map_or(0, Vec::len));
    println!(
        "exit_code={code} elapsed_ms={:.3}",
        elapsed.as_secs_f64() * 1000.0
    );
    println!(
        "frames={frames} effective_fps={:.2}",
        frames as f64 / elapsed.as_secs_f64()
    );
    let read_calls = READ_CALLS.load(Ordering::Relaxed);
    let read_bytes = READ_BYTES.load(Ordering::Relaxed);
    let read_time = ns_to_ms(READ_TIME_NS.load(Ordering::Relaxed));
    let update_calls = UPDATE_CALLS.load(Ordering::Relaxed);
    let update_time = ns_to_ms(UPDATE_TIME_NS.load(Ordering::Relaxed));
    println!(
        "read_mode={} viewer_mode={} read_calls={} read_bytes={} host_read_ms={:.3} update_calls={} host_update_ms={:.3}",
        read_mode, viewer_mode, read_calls, read_bytes, read_time, update_calls, update_time
    );
    println!("blit_checksum={checksum}");
    if let Some(output) = OUTPUT.get().and_then(|output| output.lock().ok()) {
        let mut found_bench = false;
        for line in output.split(|&byte| byte == b'\n') {
            if line.starts_with(b"MP4-BENCH ") {
                found_bench = true;
                println!("{}", String::from_utf8_lossy(line));
            }
        }
        if !found_bench {
            let start = output.len().saturating_sub(16 * 1024);
            println!(
                "diagnostic_tail={}",
                String::from_utf8_lossy(&output[start..])
            );
        }
    }
}

fn host() -> WasiHost {
    WasiHost {
        write_stdout: sink,
        write_stderr: sink,
        read_stdin: no_stdin,
        yield_now: no_op,
        wait_for_ns: no_wait,
        file_size,
        read_file_range,
        read_directory,
        write_file,
        write_file_chunk,
        get_monotonic_ns,
        screen_dimensions,
        capture_screen,
        capture_screen_chunk,
        show_image,
        show_text,
        show_error,
        create_window,
        update_window,
        close_window,
        play_pcm,
    }
}

fn sink(bytes: &[u8]) {
    if let Some(output) = OUTPUT.get()
        && let Ok(mut output) = output.lock()
        && output.len() < 512 * 1024
    {
        output.extend_from_slice(bytes);
    }
}

fn no_stdin() -> Option<u8> {
    None
}

fn no_op() {}

fn no_wait(_: u64) {}

fn file_size(_: &str) -> Result<u64, FsError> {
    MEDIA.get().map(|data| data.len() as u64).ok_or(FsError::Io)
}

fn read_file_range(_: &str, offset: u64, limit: usize) -> Result<Vec<u8>, FsError> {
    let start_time = Instant::now();
    let data = MEDIA.get().ok_or(FsError::Io)?;
    let start = usize::try_from(offset).map_err(|_| FsError::InvalidSeek)?;
    if start > data.len() {
        return Err(FsError::InvalidSeek);
    }
    let granularity = READ_GRANULARITY.load(Ordering::Relaxed) as usize;
    let end = start.saturating_add(limit.min(granularity)).min(data.len());
    let result = data[start..end].to_vec();
    READ_CALLS.fetch_add(1, Ordering::Relaxed);
    READ_BYTES.fetch_add(result.len() as u64, Ordering::Relaxed);
    READ_TIME_NS.fetch_add(start_time.elapsed().as_nanos() as u64, Ordering::Relaxed);
    Ok(result)
}

fn read_directory(_: &str) -> Result<Vec<(String, u8)>, FsError> {
    Ok(Vec::new())
}

fn write_file(_: &str, _: &[u8]) -> Result<(), FsError> {
    Err(FsError::NotSupported)
}

fn write_file_chunk(_: &str, _: u64, _: &[u8], _: bool) -> Result<(), FsError> {
    Err(FsError::NotSupported)
}

fn get_monotonic_ns() -> u64 {
    CLOCK_START.get().map_or(0, |start| {
        start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
    })
}

fn screen_dimensions() -> (u32, u32) {
    (800, 600)
}

fn capture_screen() -> Option<(u32, u32, Vec<u8>)> {
    None
}

fn capture_screen_chunk(_: u32, _: &mut [u8]) -> Option<(u32, u32)> {
    None
}

fn show_image(_: u32, _: u32, _: &[u8]) -> i32 {
    0
}

fn show_text(_: &str, _: &str) -> i32 {
    0
}

fn show_error(_: &str, _: &str) -> i32 {
    0
}

fn create_window(_: &str, _: u32, _: u32) -> i32 {
    1
}

fn update_window(_: i32, _: u32, _: u32, pixels: &[u8]) -> i32 {
    let start_time = Instant::now();
    let mut checksum = 0u64;
    for rgb in pixels.chunks_exact(3) {
        let color = (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]);
        checksum = checksum.wrapping_add(u64::from(color));
    }
    FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
    BLIT_CHECKSUM.fetch_add(checksum, Ordering::Relaxed);
    UPDATE_CALLS.fetch_add(1, Ordering::Relaxed);
    UPDATE_TIME_NS.fetch_add(start_time.elapsed().as_nanos() as u64, Ordering::Relaxed);
    0
}

fn close_window(_: i32) -> i32 {
    0
}

fn play_pcm(_: u32, _: u8, _: u8, _: &[u8]) -> i32 {
    0
}

fn ns_to_ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}
