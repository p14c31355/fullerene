use image::GenericImageView;
use std::cell::Cell;
use std::collections::VecDeque;
use std::io::{self, Cursor, Read, Seek};
use std::rc::Rc;
use std::time::{Duration, Instant};

const VIEWER_BUILD_ID: &str = "2026-07-31-native-z264-mp4-1";
const MAX_MP4_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MP4_IO_OPERATIONS: usize = 16_384;
const MAX_MP4_PARSE_TIME: Duration = Duration::from_secs(3);
const MAX_MP4_SAMPLES: u32 = 1_000_000;
const MAX_PLAYBACK_SAMPLE_BYTES: usize = 8 * 1024 * 1024;
const VIDEO_REORDER_DEPTH: usize = 16;
// Keep the event loop responsive without adding a host yield for every
// sample; native video submission is already one host transition per sample.
const PLAYBACK_YIELD_SAMPLES: usize = 2;
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SOURCE_IMAGE_WIDTH: u32 = 16_384;
const MAX_SOURCE_IMAGE_HEIGHT: u32 = 16_384;
const MAX_SOURCE_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_IMAGE_ALLOC_BYTES: u64 = 64 * 1024 * 1024;
const MAX_QOI_PIXELS: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mp4BenchMode {
    DecodeOnly,
    DecodeConvert,
    Full,
}

impl Mp4BenchMode {
    fn parse(arg: &str) -> Option<Self> {
        match arg {
            "--bench=decode" => Some(Self::DecodeOnly),
            "--bench=convert" => Some(Self::DecodeConvert),
            "--bench=full" => Some(Self::Full),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::DecodeOnly => "decode",
            Self::DecodeConvert => "decode+convert",
            Self::Full => "full",
        }
    }
}

#[link(wasm_import_module = "fullerene")]
unsafe extern "C" {
    fn show_image(width: u32, height: u32, pixels_ptr: *const u8, pixels_len: u32) -> u32;
    fn show_text(title_ptr: *const u8, title_len: u32, text_ptr: *const u8, text_len: u32) -> u32;
    fn show_error(title_ptr: *const u8, title_len: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    fn create_window(title_ptr: *const u8, title_len: u32, width: u32, height: u32) -> i32;
    fn update_window(
        window_id: i32,
        width: u32,
        height: u32,
        pixels_ptr: *const u8,
        pixels_len: u32,
    ) -> i32;
    fn video_open(config_ptr: *const u8, config_len: u32) -> u32;
    fn video_decode_sample(sample_ptr: *const u8, sample_len: u32, length_size: u32) -> u32;
    fn video_frame_info() -> u64;
    fn video_present(window_id: i32) -> u32;
    fn video_discard() -> u32;
    fn video_flush() -> u32;
    fn video_stage_timing(stage: u32) -> u64;
    fn video_close() -> u32;
    fn video_should_stop() -> u32;
    fn close_window(window_id: i32) -> u32;
    fn wait_for_ns(duration_ns: u64) -> u32;
}

fn main() {
    println!("viewer: build={}", VIEWER_BUILD_ID);
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: viewer <path>");
        std::process::exit(1);
    }

    let path = &args[1];
    let bench_mode = args.get(2).and_then(|arg| Mp4BenchMode::parse(arg));
    if is_mp4_path(path) && try_mp4_file_with_mode(path, bench_mode) {
        return;
    }

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            present_error("Viewer error", &format!("Cannot read '{}': {}", path, e));
            std::process::exit(1);
        }
    };
    println!("viewer: read complete path={} bytes={}", path, bytes.len());

    // QOI is a single-purpose image format.  Do not let a failed QOI decode
    // fall through MP4/audio/archive probes; that made one bad screenshot
    // look like an endless stream of diagnostics in Klog Live.
    if is_qoi_path(path) {
        if !qoi_header_allowed(&bytes) {
            present_error("QOI Viewer error", "Invalid or oversized QOI header.");
            return;
        }
        if !try_image(path, &bytes) {
            present_error("QOI Viewer error", "The QOI image could not be decoded.");
        }
        return;
    }

    // Keep ordinary text on the WASM viewer's text-window path and avoid
    // probing it with binary/media parsers first.
    if is_text_path(path) {
        present_text(path, &bytes);
        return;
    }

    // A named image must not be reinterpreted as MP4/audio/archive data when
    // its decoder fails.  The old fallback hid the original JPEG error and
    // made a normal image failure look like a parser hang in Klog Live.
    if is_image_path(path) {
        if !try_image(path, &bytes) {
            let title = format!("Image Viewer error: {}", path);
            present_error(&title, "The image could not be decoded.");
        }
        return;
    }

    if try_image(path, &bytes) {
        return;
    }
    if try_mp4(path, &bytes) {
        return;
    }
    if try_mp3(&bytes) {
        return;
    }
    if try_wav(&bytes) {
        return;
    }

    // Archives
    if try_zip(&bytes) {
        return;
    }
    if try_tar(&bytes) {
        return;
    }
    if try_gzip(&bytes) {
        return;
    }

    // RLE animation (Fullerene custom format)
    if try_rle(path, &bytes) {
        return;
    }

    // Text/fallback
    if std::str::from_utf8(&bytes).is_ok() {
        present_text(path, &bytes);
        return;
    }
    print_hex(path, &bytes);
}

fn is_mp4_path(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".mp4")
}

fn is_qoi_path(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".qoi")
}

fn is_image_path(path: &str) -> bool {
    matches!(
        path.to_ascii_lowercase().rsplit('.').next(),
        Some("jpg" | "jpeg" | "png" | "bmp" | "gif" | "webp" | "tif" | "tiff")
    )
}

fn qoi_header_allowed(data: &[u8]) -> bool {
    if data.len() < 14 || &data[..4] != b"qoif" {
        return false;
    }
    let width = u32::from_be_bytes(data[4..8].try_into().unwrap()) as u64;
    let height = u32::from_be_bytes(data[8..12].try_into().unwrap()) as u64;
    let channels = data[12];
    let pixels = width.saturating_mul(height);
    width != 0
        && height != 0
        && matches!(channels, 3 | 4)
        && pixels <= MAX_QOI_PIXELS
        && data.len() >= 22
}

fn is_text_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    matches!(
        lower.rsplit('.').next(),
        Some(
            "txt"
                | "md"
                | "log"
                | "toml"
                | "rs"
                | "c"
                | "h"
                | "py"
                | "js"
                | "json"
                | "xml"
                | "yml"
                | "yaml"
                | "ini"
                | "cfg"
                | "conf"
                | "sh"
                | "bat"
                | "env"
                | "lock"
        )
    )
}

fn present_text(path: &str, bytes: &[u8]) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let title = format!("Text Viewer: {}", path);
    unsafe {
        show_text(
            title.as_ptr(),
            title.len() as u32,
            text.as_ptr(),
            text.len() as u32,
        );
    }
}

fn present_error(title: &str, message: &str) {
    unsafe {
        show_error(
            title.as_ptr(),
            title.len() as u32,
            message.as_ptr(),
            message.len() as u32,
        );
    }
}

// ── Image ───────────────────────────────────────────────────────

fn try_image(path: &str, data: &[u8]) -> bool {
    const MAX_IMAGE_WIDTH: u32 = 800;
    const MAX_IMAGE_HEIGHT: u32 = 600;

    // Inspect dimensions before decoding so malformed or hostile images get
    // a useful error before the decoder allocates a full-resolution surface.
    let dimensions = image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok());
    let Some((source_w, source_h)) = dimensions else {
        println!("viewer: dimensions failed");
        return false;
    };
    println!("viewer: dimensions complete {}x{}", source_w, source_h);
    if source_w == 0 || source_h == 0 {
        let title = format!("Image Viewer: {}", path);
        let report = format!(
            "The image has invalid dimensions.\nResolution: {}x{}",
            source_w, source_h
        );
        present_error(&title, &report);
        return true;
    }
    if data.len() > MAX_IMAGE_BYTES || !source_dimensions_allowed(source_w, source_h) {
        let title = format!("Image Viewer: {}", path);
        let report = format!(
            "The image is too large to decode safely.\nResolution: {}x{}\nMaximum pixels: {}",
            source_w, source_h, MAX_SOURCE_IMAGE_PIXELS
        );
        present_error(&title, &report);
        return true;
    }

    let Ok(mut reader) = image::ImageReader::new(Cursor::new(data)).with_guessed_format() else {
        println!("viewer: decoder setup failed");
        return false;
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_IMAGE_WIDTH);
    limits.max_image_height = Some(MAX_SOURCE_IMAGE_HEIGHT);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC_BYTES);
    reader.limits(limits);
    println!("viewer: decode enter");
    let Ok(img) = reader.decode() else {
        println!("viewer: decode failed");
        return false;
    };
    println!("viewer: decode exit");

    // The compositor only displays an 800x600 client area. Downsample before
    // converting to RGB so we do not keep a second full-resolution buffer.
    println!(
        "viewer: thumbnail enter source={}x{} limit={}x{}",
        source_w, source_h, MAX_IMAGE_WIDTH, MAX_IMAGE_HEIGHT
    );
    let thumbnail = if source_w <= MAX_IMAGE_WIDTH && source_h <= MAX_IMAGE_HEIGHT {
        // image::DynamicImage::thumbnail() upscales small images to fit the
        // requested bounds. Avoid turning a 225x225 JPEG into a 600x600
        // surface in the WASM interpreter.
        img
    } else {
        img.thumbnail(MAX_IMAGE_WIDTH, MAX_IMAGE_HEIGHT)
    };
    let (w, h) = thumbnail.dimensions();
    println!("viewer: thumbnail exit {}x{}", w, h);
    println!("viewer: to_rgb8 enter");
    let pixels = thumbnail.to_rgb8().into_raw();
    println!("viewer: to_rgb8 exit bytes={}", pixels.len());
    println!(
        "viewer: show_image enter {}x{} bytes={}",
        w,
        h,
        pixels.len()
    );
    let result = unsafe { show_image(w, h, pixels.as_ptr(), pixels.len() as u32) };
    println!("viewer: show_image exit result={}", result);
    true
}

fn source_dimensions_allowed(width: u32, height: u32) -> bool {
    width <= MAX_SOURCE_IMAGE_WIDTH
        && height <= MAX_SOURCE_IMAGE_HEIGHT
        && u64::from(width)
            .checked_mul(u64::from(height))
            .is_some_and(|pixels| pixels <= MAX_SOURCE_IMAGE_PIXELS)
}

// ── MP4 video ───────────────────────────────────────────────────

/// Keep malformed metadata from turning the synchronous viewer into an
/// unbounded parser. This also makes the last successful read visible in the
/// diagnostic log instead of leaving the caller inside `read_header` forever.
struct BoundedMp4Reader<R> {
    inner: R,
    parsing_header: Rc<Cell<bool>>,
    operations: usize,
    started_at: Instant,
    position: u64,
}

impl<R> BoundedMp4Reader<R> {
    fn new(inner: R, parsing_header: Rc<Cell<bool>>) -> Self {
        Self {
            inner,
            parsing_header,
            operations: 0,
            started_at: Instant::now(),
            position: 0,
        }
    }

    fn begin_operation(&mut self) -> io::Result<()> {
        if !self.parsing_header.get() {
            self.operations = self.operations.saturating_add(1);
            return Ok(());
        }
        if self.started_at.elapsed() >= MAX_MP4_PARSE_TIME {
            println!(
                "viewer: mp4 parse time budget exhausted operations={} elapsed_ms={}",
                self.operations,
                self.started_at.elapsed().as_millis()
            );
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "MP4 metadata parse time budget exhausted",
            ));
        }
        if self.operations >= MAX_MP4_IO_OPERATIONS {
            println!(
                "viewer: mp4 io budget exhausted operations={}",
                self.operations
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MP4 metadata I/O budget exhausted",
            ));
        }
        self.operations += 1;
        Ok(())
    }
}

impl<R: Read> Read for BoundedMp4Reader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.begin_operation()?;
        let trace = self.operations <= 32 || self.operations.checked_rem(128) == Some(0);
        if trace {
            println!(
                "viewer: mp4 io read begin op={} bytes={}",
                self.operations,
                buffer.len()
            );
        }
        let result = self.inner.read(buffer);
        if let Ok(read) = result.as_ref() {
            self.position = self.position.saturating_add(*read as u64);
        }
        if trace || result.is_err() {
            println!(
                "viewer: mp4 io read exit op={} result={:?}",
                self.operations,
                result.as_ref().map(|n| *n)
            );
        }
        result
    }
}

impl<R: Seek> Seek for BoundedMp4Reader<R> {
    fn seek(&mut self, position: std::io::SeekFrom) -> io::Result<u64> {
        self.begin_operation()?;
        let trace = self.operations <= 32 || self.operations.checked_rem(128) == Some(0);
        if trace {
            println!(
                "viewer: mp4 io seek begin op={} position={:?}",
                self.operations, position
            );
        }
        let result = self.inner.seek(position);
        if let Ok(new_position) = result.as_ref() {
            self.position = *new_position;
        }
        if trace || result.is_err() {
            println!(
                "viewer: mp4 io seek exit op={} result={:?}",
                self.operations,
                result.as_ref().map(|n| *n)
            );
        }
        result
    }
}

fn try_mp4(path: &str, data: &[u8]) -> bool {
    println!("viewer: mp4 probe enter bytes={}", data.len());
    if data.len() as u64 > MAX_MP4_BYTES {
        println!("viewer: mp4 rejected size>{} bytes", MAX_MP4_BYTES);
        return false;
    }

    println!("MP4 FILE size OK viewer_build={}", VIEWER_BUILD_ID);
    println!("viewer: mp4 header enter");
    let parsing_header = Rc::new(Cell::new(true));
    let Ok(reader) = mp4::Mp4Reader::read_header(
        BoundedMp4Reader::new(Cursor::new(data), Rc::clone(&parsing_header)),
        data.len() as u64,
    ) else {
        println!("viewer: mp4 header failed");
        return false;
    };
    parsing_header.set(false);
    println!("viewer: mp4 header exit");

    try_mp4_reader(path, data.len() as u64, reader)
}

/// Open an MP4 through a seekable WASI file instead of reading the whole
/// media file into a WASM Vec.  The MP4 metadata is at the beginning of the
/// sample file, and the reader only seeks to the first sample it needs.
#[cfg(test)]
fn try_mp4_file(path: &str) -> bool {
    try_mp4_file_with_mode(path, None)
}

fn try_mp4_file_with_mode(path: &str, bench_mode: Option<Mp4BenchMode>) -> bool {
    println!("viewer: mp4 file stat enter path={path}");
    let size = match std::fs::metadata(path).map(|metadata| metadata.len()) {
        Ok(size) => size,
        Err(error) => {
            present_error(
                "MP4 Viewer error",
                &format!("Cannot stat '{}': {}", path, error),
            );
            return true;
        }
    };
    println!("viewer: mp4 file stat exit size={size}");
    println!("viewer: mp4 file mode size={}", size);
    if size > MAX_MP4_BYTES {
        present_error(
            "MP4 Viewer error",
            &format!("MP4 is too large to preview safely: {} bytes", size),
        );
        return true;
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            present_error(
                "MP4 Viewer error",
                &format!("Cannot open '{}': {}", path, error),
            );
            return true;
        }
    };
    println!("viewer: mp4 file open exit");
    println!("MP4 FILE size OK viewer_build={}", VIEWER_BUILD_ID);
    println!("viewer: mp4 header enter");
    let parsing_header = Rc::new(Cell::new(true));
    // Wrap the bounded reader in a large BufReader so that the mp4 crate's
    // many small header reads (8-byte atom size+type probes) are coalesced
    // into far fewer real file I/O operations.  Without this, a normal MP4
    // moov atom exhausts the 16 384-operation I/O budget before parsing
    // completes, even though the file itself is well-formed.
    let bounded = BoundedMp4Reader::new(file, Rc::clone(&parsing_header));
    let buffered = std::io::BufReader::with_capacity(64 * 1024, bounded);
    let reader = match mp4::Mp4Reader::read_header(buffered, size) {
        Ok(reader) => reader,
        Err(error) => {
            println!("viewer: mp4 header failed");
            println!("viewer: mp4 header error={:?}", error);
            present_error(
                "MP4 Viewer error",
                "MP4 header is invalid or unsupported.\n\nThe file was not opened to avoid a parser hang.",
            );
            return true;
        }
    };
    parsing_header.set(false);
    println!("viewer: mp4 header exit");
    try_mp4_reader_with_mode(path, size, reader, bench_mode)
}

fn try_mp4_reader<R: Read + Seek>(path: &str, size: u64, reader: mp4::Mp4Reader<R>) -> bool {
    try_mp4_reader_with_mode(path, size, reader, None)
}

fn try_mp4_reader_with_mode<R: Read + Seek>(
    path: &str,
    size: u64,
    mut reader: mp4::Mp4Reader<R>,
    bench_mode: Option<Mp4BenchMode>,
) -> bool {
    let duration = reader.duration().as_secs_f64();
    println!("viewer: mp4 duration exit seconds={:.3}", duration);
    let total = duration as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    let dur = if h > 0 {
        format!("{}h{:02}m{:02}s", h, m, s)
    } else {
        format!("{:02}m{:02}s", m, s)
    };

    let size_mb = size as f64 / (1024.0 * 1024.0);
    println!("File: {}", path);
    println!("Type: MP4 video");
    println!("Size: {:.1} MB", size_mb);
    println!("Duration: {}", dur);

    println!("viewer: mp4 video track scan enter");
    let Some((track_id, width, height, avcc, timescale)) = reader
        .tracks()
        .values()
        .find(|t| t.track_type().ok() == Some(mp4::TrackType::Video))
        .and_then(|track| {
            let avc1 = track.trak.mdia.minf.stbl.stsd.avc1.as_ref()?;
            Some((
                track.track_id(),
                track.width() as u32,
                track.height() as u32,
                avc1.avcc.clone(),
                track.timescale(),
            ))
        })
    else {
        println!("viewer: mp4 video track unavailable");
        return present_mp4_failure(path, size_mb, dur, 0, 0, "video track unavailable");
    };
    println!("viewer: mp4 video track scan exit id={}", track_id);
    println!("Resolution: {}x{}", width, height);
    if width == 0 || height == 0 || width > 1920 || height > 1080 {
        println!("viewer: mp4 rejected resolution");
        return present_mp4_failure(path, size_mb, dur, width, height, "unsupported resolution");
    }
    let sample_count = reader.sample_count(track_id).ok().unwrap_or(0);
    println!("viewer: mp4 sample count={}", sample_count);
    if sample_count == 0 {
        return present_mp4_failure(path, size_mb, dur, width, height, "video has no samples");
    }
    if sample_count > MAX_MP4_SAMPLES {
        println!(
            "viewer: mp4 rejected sample count={} limit={}",
            sample_count, MAX_MP4_SAMPLES
        );
        return present_mp4_failure(
            path,
            size_mb,
            dur,
            width,
            height,
            "video sample table is too large",
        );
    }

    let length_size = usize::from(avcc.length_size_minus_one) + 1;
    println!(
        "viewer: mp4 avcc length_size={} sps={} pps={}",
        length_size,
        avcc.sequence_parameter_sets.len(),
        avcc.picture_parameter_sets.len()
    );
    if !matches!(length_size, 1 | 2 | 4)
        || avcc.sequence_parameter_sets.is_empty()
        || avcc.picture_parameter_sets.is_empty()
    {
        return present_mp4_failure(
            path,
            size_mb,
            dur,
            width,
            height,
            "invalid AVC configuration",
        );
    }

    // MP4 stores SPS/PPS in avcC, outside the sample. Build the compact
    // Annex B configuration once and hand it to native z264. The WASM side
    // remains responsible for demuxing and pacing, but no longer performs
    // per-NAL parsing, H.264 reconstruction, or YUV conversion.
    let mut config_stream = Vec::new();
    for nal in avcc
        .sequence_parameter_sets
        .iter()
        .chain(avcc.picture_parameter_sets.iter())
    {
        config_stream.extend_from_slice(&[0, 0, 0, 1]);
        config_stream.extend_from_slice(&nal.bytes);
    }
    println!(
        "viewer: mp4 decoder config enter bytes={}",
        config_stream.len()
    );
    let config_code = unsafe { video_open(config_stream.as_ptr(), config_stream.len() as u32) };
    if config_code != 0 {
        println!(
            "viewer: mp4 native decoder config failed code={}",
            config_code
        );
        return present_mp4_failure(
            path,
            size_mb,
            dur,
            width,
            height,
            "decoder configuration failed",
        );
    }
    println!("viewer: mp4 decoder config exit");

    let title = format!("Video: {}", path);
    let mut window_id = -1;
    let mut stopped_by_escape = false;
    // Clear a request that was generated before this viewer became active.
    unsafe {
        let _ = video_should_stop();
    }
    let playback_start = Instant::now();
    let mut read_time = Duration::ZERO;
    let mut decode_time = Duration::ZERO;
    let mut convert_time = Duration::ZERO;
    let mut present_time = Duration::ZERO;
    let mut wait_time = Duration::ZERO;
    let mut decoded_frames = 0u32;
    let mut presented_frames = 0u32;
    let mut samples = 0u32;
    let mut nals_decoded = 0u64;
    let mut rgb_bytes = 0usize;
    // H.264 decodes pictures in decode order while MP4 composition offsets
    // describe presentation order. Keep the small reorder window here so a
    // decoded frame is paced against the earliest presentation timestamp
    // that has entered the decoder, not against the packet that released it.
    let mut pending_presentation_times = VecDeque::new();
    let mut failure = None;
    let mut samples_since_yield = PLAYBACK_YIELD_SAMPLES;
    println!("viewer: mp4 playback enter samples={}", sample_count);
    // mp4 crate sample IDs are one-based. Decode every video sample in order,
    // present each decoded frame, and pace it against the track timestamps.
    'playback: for sample_id in 1..=sample_count {
        if unsafe { video_should_stop() != 0 } {
            stopped_by_escape = true;
            break 'playback;
        }
        let trace_sample = sample_id <= 4 || sample_id.checked_rem(128) == Some(0);
        if trace_sample {
            println!("viewer: mp4 sample begin id={}", sample_id);
        }
        // Keep the event loop responsive without crossing the WASM/host
        // boundary twice for the common one-NAL-per-sample case.  The first
        // sample yields immediately; later samples yield at a bounded NAL
        // interval, including samples with no decoded output.
        if samples_since_yield >= PLAYBACK_YIELD_SAMPLES {
            unsafe {
                if trace_sample {
                    println!("viewer: mp4 sample wait begin id={}", sample_id);
                }
                let wait_start = Instant::now();
                wait_for_ns(0);
                wait_time += wait_start.elapsed();
                if trace_sample {
                    println!("viewer: mp4 sample wait exit id={}", sample_id);
                }
            }
            samples_since_yield = 0;
        }
        if unsafe { video_should_stop() != 0 } {
            stopped_by_escape = true;
            break 'playback;
        }
        if trace_sample {
            println!("viewer: mp4 sample read begin id={}", sample_id);
        }
        let read_start = Instant::now();
        let sample = match reader.read_sample(track_id, sample_id) {
            Ok(Some(sample)) => sample,
            Ok(None) => {
                println!("viewer: mp4 sample unavailable id={}", sample_id);
                failure = Some("video sample unavailable");
                break 'playback;
            }
            Err(error) => {
                println!(
                    "viewer: mp4 sample read failed id={} error={:?}",
                    sample_id, error
                );
                failure = Some("video sample read failed");
                break 'playback;
            }
        };
        read_time += read_start.elapsed();
        samples = samples.saturating_add(1);
        if trace_sample {
            println!(
                "viewer: mp4 sample read exit id={} bytes={}",
                sample_id,
                sample.bytes.len()
            );
        }
        if sample.bytes.len() > MAX_PLAYBACK_SAMPLE_BYTES {
            println!(
                "viewer: mp4 sample rejected id={} size>{} bytes",
                sample_id, MAX_PLAYBACK_SAMPLE_BYTES
            );
            failure = Some("video sample is too large");
            break 'playback;
        }
        let target_ns =
            sample_presentation_time_ns(sample.start_time, sample.rendering_offset, timescale);
        push_pending_presentation_time(&mut pending_presentation_times, target_ns);
        let decode_start = Instant::now();
        let frame_count = unsafe {
            video_decode_sample(
                sample.bytes.as_ptr(),
                sample.bytes.len() as u32,
                length_size as u32,
            )
        };
        decode_time += decode_start.elapsed();
        samples_since_yield = samples_since_yield.saturating_add(1);
        if frame_count == u32::MAX {
            failure = Some("native decoder rejected a video sample");
            break 'playback;
        }
        nals_decoded = nals_decoded.saturating_add(u64::from(frame_count));
        decoded_frames = decoded_frames.saturating_add(frame_count);
        for _ in 0..frame_count {
            let frame_target_ns =
                take_earliest_presentation_time(&mut pending_presentation_times, target_ns);
            let wait_start = Instant::now();
            wait_for_video_time(playback_start, frame_target_ns);
            wait_time += wait_start.elapsed();
            if unsafe { video_should_stop() != 0 } {
                stopped_by_escape = true;
                break 'playback;
            }
            let frame_info = unsafe { video_frame_info() };
            let frame_width = (frame_info >> 32) as u32;
            let frame_height = frame_info as u32;
            if frame_width == 0 || frame_height == 0 {
                failure = Some("native decoder returned invalid frame dimensions");
                break 'playback;
            }
            rgb_bytes = frame_width as usize * frame_height as usize * 3;
            let present_start = Instant::now();
            let present_code = match bench_mode {
                Some(Mp4BenchMode::DecodeOnly) => unsafe { video_discard() },
                Some(Mp4BenchMode::DecodeConvert) => unsafe { video_present(-1) },
                _ => {
                    if window_id < 0 {
                        window_id = unsafe {
                            create_window(
                                title.as_ptr(),
                                title.len() as u32,
                                frame_width,
                                frame_height,
                            )
                        };
                    }
                    if window_id < 0 {
                        u32::MAX
                    } else {
                        unsafe { video_present(window_id) }
                    }
                }
            };
            if bench_mode == Some(Mp4BenchMode::DecodeConvert) {
                convert_time += present_start.elapsed();
            } else if bench_mode != Some(Mp4BenchMode::DecodeOnly) {
                present_time += present_start.elapsed();
            }
            if present_code != 0 {
                failure = Some("native frame presentation failed");
                break 'playback;
            }
            if bench_mode != Some(Mp4BenchMode::DecodeOnly) {
                presented_frames = presented_frames.saturating_add(1);
                if presented_frames == 1 || presented_frames.checked_rem(30) == Some(0) {
                    println!(
                        "viewer: mp4 playback frame={} sample={} pts_ns={}",
                        presented_frames, sample_id, frame_target_ns
                    );
                }
            }
        }
        if trace_sample {
            println!("viewer: mp4 sample exit id={}", sample_id);
        }
    }
    let mut flush_count = if stopped_by_escape || failure.is_some() {
        0
    } else {
        unsafe { video_flush() }
    };
    if flush_count == u32::MAX {
        failure = Some("native decoder flush failed");
        flush_count = 0;
    }
    decoded_frames = decoded_frames.saturating_add(flush_count);
    for _ in 0..flush_count {
        let frame_target_ns = take_earliest_presentation_time(
            &mut pending_presentation_times,
            playback_start
                .elapsed()
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64,
        );
        let wait_start = Instant::now();
        wait_for_video_time(playback_start, frame_target_ns);
        wait_time += wait_start.elapsed();
        if unsafe { video_should_stop() != 0 } {
            stopped_by_escape = true;
            break;
        }
        let frame_info = unsafe { video_frame_info() };
        let frame_width = (frame_info >> 32) as u32;
        let frame_height = frame_info as u32;
        if frame_width == 0 || frame_height == 0 {
            failure = Some("native decoder returned invalid frame dimensions");
            break;
        }
        rgb_bytes = frame_width as usize * frame_height as usize * 3;
        let present_start = Instant::now();
        let present_code = match bench_mode {
            Some(Mp4BenchMode::DecodeOnly) => unsafe { video_discard() },
            Some(Mp4BenchMode::DecodeConvert) => unsafe { video_present(-1) },
            _ if window_id >= 0 => unsafe { video_present(window_id) },
            _ => u32::MAX,
        };
        if bench_mode == Some(Mp4BenchMode::DecodeConvert) {
            convert_time += present_start.elapsed();
        } else if bench_mode != Some(Mp4BenchMode::DecodeOnly) {
            present_time += present_start.elapsed();
        }
        if present_code != 0 {
            failure = Some("native frame flush presentation failed");
            break;
        }
        if bench_mode != Some(Mp4BenchMode::DecodeOnly) {
            presented_frames = presented_frames.saturating_add(1);
        }
    }
    let yuv_to_rgb_ns = unsafe { video_stage_timing(0) };
    let scale_ns = unsafe { video_stage_timing(1) };
    let window_buffer_copy_ns = unsafe { video_stage_timing(2) };
    let composite_ns = unsafe { video_stage_timing(3) };
    let framebuffer_flush_ns = unsafe { video_stage_timing(4) };
    println!(
        "MP4-STAGE frames={} read_demux_ms={:.3} decode_ms={:.3} yuv_to_rgb_ms={:.3} scale_ms={:.3} window_buffer_copy_ms={:.3} composite_ms={:.3} framebuffer_flush_ms={:.3} wait_ms={:.3}",
        presented_frames,
        ms(read_time),
        ms(decode_time),
        ns_ms(yuv_to_rgb_ns),
        ns_ms(scale_ns),
        ns_ms(window_buffer_copy_ns),
        ns_ms(composite_ns),
        ns_ms(framebuffer_flush_ns),
        ms(wait_time),
    );
    unsafe {
        video_close();
        if window_id >= 0 {
            let _ = close_window(window_id);
        }
    }
    if let Some(reason) = failure {
        return present_mp4_failure(path, size_mb, dur, width, height, reason);
    }
    println!(
        "viewer: mp4 playback exit frames={} stopped_by_escape={}",
        presented_frames, stopped_by_escape
    );
    if let Some(mode) = bench_mode {
        println!(
            "MP4-BENCH mode={} playback_ms={:.3} read_ms={:.3} decode_ms={:.3} convert_ms={:.3} present_ms={:.3} samples={} nals_with_frames={} decoded_frames={} presented_frames={} rgb_bytes={}",
            mode.label(),
            ms(playback_start.elapsed()),
            ms(read_time),
            ms(decode_time),
            ms(convert_time),
            ms(present_time),
            samples,
            nals_decoded,
            decoded_frames,
            presented_frames,
            rgb_bytes,
        );
    }
    if decoded_frames == 0 {
        return present_mp4_failure(
            path,
            size_mb,
            dur,
            width,
            height,
            "first frame could not be decoded safely",
        );
    }
    true
}

fn present_mp4_failure(
    path: &str,
    size_mb: f64,
    duration: String,
    width: u32,
    height: u32,
    reason: &str,
) -> bool {
    let title = format!("Video: {}", path);
    let report = format!(
        "File: {}\nType: MP4 video\nSize: {:.1} MB\nDuration: {}\nResolution: {}x{}\n\n{}.",
        path, size_mb, duration, width, height, reason
    );
    unsafe {
        show_text(
            title.as_ptr(),
            title.len() as u32,
            report.as_ptr(),
            report.len() as u32,
        );
    }
    true
}

fn wait_for_video_time(start: Instant, target_ns: u64) {
    let target = Duration::from_nanos(target_ns);
    let remaining = target.checked_sub(start.elapsed()).unwrap_or_default();
    if remaining.is_zero() {
        // The decoder is already behind. update_window() performs a host
        // event-loop tick after presenting the frame, so another zero-time
        // wait here would only add an unnecessary WASM/host transition.
        return;
    }
    let remaining_ns = remaining.as_nanos().min(u128::from(u64::MAX)) as u64;
    unsafe {
        wait_for_ns(remaining_ns);
    }
}

/// Convert an MP4 decode timestamp plus composition-time offset into the
/// presentation clock used by the viewer. The offset is signed because
/// B-frames can be displayed after their decode-order timestamp.
fn sample_presentation_time_ns(start_time: u64, rendering_offset: i32, timescale: u32) -> u64 {
    if timescale == 0 {
        return 0;
    }
    let timestamp = i128::from(start_time)
        .saturating_add(i128::from(rendering_offset))
        .saturating_mul(1_000_000_000)
        / i128::from(timescale);
    timestamp.clamp(0, i128::from(u64::MAX)) as u64
}

fn take_earliest_presentation_time(times: &mut VecDeque<u64>, fallback: u64) -> u64 {
    let Some((index, _)) = times.iter().enumerate().min_by_key(|(_, time)| **time) else {
        return fallback;
    };
    times.remove(index).unwrap_or(fallback)
}

fn push_pending_presentation_time(times: &mut VecDeque<u64>, timestamp: u64) {
    times.push_back(timestamp);
    // OrderedDecoder retains at most this many frames while it waits for
    // display order. A sample can produce no frame, so timestamps can become
    // stale; dropping the oldest entries bounds memory and limits the pacing
    // drift to one reorder window when that happens.
    while times.len() > VIDEO_REORDER_DEPTH {
        let _ = times.pop_front();
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn ns_ms(nanoseconds: u64) -> f64 {
    nanoseconds as f64 / 1_000_000.0
}

#[cfg(test)]
fn fit_dimensions(
    source_width: usize,
    source_height: usize,
    max_width: usize,
    max_height: usize,
) -> Option<(usize, usize)> {
    if source_width == 0 || source_height == 0 || max_width == 0 || max_height == 0 {
        return None;
    }
    if source_width <= max_width && source_height <= max_height {
        return Some((source_width, source_height));
    }
    if source_width as u64 * max_height as u64 <= source_height as u64 * max_width as u64 {
        let width = source_width
            .checked_mul(max_height)?
            .checked_div(source_height)?
            .max(1);
        Some((width.min(max_width), max_height))
    } else {
        let height = source_height
            .checked_mul(max_width)?
            .checked_div(source_width)?
            .max(1);
        Some((max_width, height.min(max_height)))
    }
}

// ── MP3 audio ───────────────────────────────────────────────────

fn try_mp3(data: &[u8]) -> bool {
    if data.len() < 10 || (&data[..3] != b"ID3" && data[0] != 0xff) {
        return false;
    }
    let id3_size = if data.len() >= 10 && &data[..3] == b"ID3" {
        (((data[6] as usize) << 21)
            | ((data[7] as usize) << 14)
            | ((data[8] as usize) << 7)
            | data[9] as usize)
            + 10
    } else {
        0
    };

    let (mut frames, mut samples, mut sr, mut ch, mut br, mut off) =
        (0u64, 0u64, 0u32, 0u32, 0u32, id3_size);
    while let Some((matched, b, s, c, spf, frame_len)) = mp3_frame(data, off) {
        if frames == 0 {
            br = b;
            sr = s;
            ch = c;
        }
        frames += 1;
        samples += spf as u64;
        off = matched.saturating_add(frame_len as usize);
        if frames > 10000 {
            break;
        }
    }
    let dur = if sr > 0 {
        samples as f64 / sr as f64
    } else {
        0.0
    };
    println!(
        "Type: MP3 audio\nBitrate: {} kbps\nSample rate: {} Hz\nChannels: {}",
        br,
        sr,
        if ch == 1 { "mono" } else { "stereo" }
    );
    println!("Duration: {:.1} s\nFrames: {}", dur, frames);
    true
}

fn mp3_frame(data: &[u8], start: usize) -> Option<(usize, u32, u32, u32, u32, u32)> {
    const BR_MPEG1: [u32; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    const BR_MPEG2: [u32; 16] = [
        0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
    ];
    const SR: [u32; 4] = [44100, 48000, 32000, 0];
    let last = data.len().checked_sub(4)?;
    if start > last {
        return None;
    }
    for off in start..=last {
        let h = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        if h & 0xffe0_0000 != 0xffe0_0000 {
            continue;
        }
        let ver = (h >> 19) & 3;
        let layer = (h >> 17) & 3;
        let bi = ((h >> 12) & 0xf) as usize;
        let si = ((h >> 10) & 3) as usize;
        if ver == 1 || layer != 1 || bi == 0 || bi == 15 || si == 3 {
            continue;
        }
        let b = if ver == 3 { BR_MPEG1[bi] } else { BR_MPEG2[bi] };
        let s = match ver {
            3 => SR[si],
            2 => SR[si] / 2,
            _ => SR[si] / 4,
        };
        let c = if (h >> 6) & 3 == 3 { 1 } else { 2 };
        // MPEG-1 Layer III uses 144 * bitrate / sample_rate; MPEG-2/2.5
        // Layer III uses 72. Include the padding slot or the next scan starts
        // at the wrong byte and can consume the complete WASM fuel budget.
        let coefficient = if ver == 3 { 144_000 } else { 72_000 };
        let padding = if (h >> 9) & 1 == 0 { 0 } else { 1 };
        let frame_len = (coefficient * b / s).saturating_add(padding).max(1);
        return Some((off, b, s, c, if ver == 3 { 1152 } else { 576 }, frame_len));
    }
    None
}

// ── WAV audio ───────────────────────────────────────────────────

fn try_wav(data: &[u8]) -> bool {
    if data.len() < 36 || &data[..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return false;
    }
    let ch = u16::from_le_bytes([data[22], data[23]]);
    let sr = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    let bits = u16::from_le_bytes([data[34], data[35]]);
    let (mut ds, mut off) = (0u32, 36usize);
    while let Some(header_end) = off.checked_add(8) {
        if header_end > data.len() {
            return false;
        }
        let cs = u32::from_le_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);
        if &data[off..off + 4] == b"data" {
            if usize::try_from(cs)
                .ok()
                .and_then(|n| header_end.checked_add(n))
                .is_none_or(|end| end > data.len())
            {
                return false;
            }
            ds = cs;
            break;
        }
        let Some(next) = header_end.checked_add(cs as usize) else {
            return false;
        };
        if next > data.len() {
            return false;
        }
        off = next;
    }
    let dur = if sr > 0 && ch > 0 && bits > 0 {
        ds as f64 / (ch as f64 * (bits as f64 / 8.0) * sr as f64)
    } else {
        0.0
    };
    println!(
        "Type: WAV audio\nChannels: {}\nSample rate: {} Hz\nBits: {}-bit\nData: {} bytes\nDuration: {:.1} s",
        ch, sr, bits, ds, dur
    );
    true
}

// ── Archives ────────────────────────────────────────────────────

fn try_zip(data: &[u8]) -> bool {
    if data.len() < 22 || (!data.starts_with(b"PK\x03\x04") && !data.starts_with(b"PK\x05\x06")) {
        return false;
    }
    println!("Archive: ZIP");
    let mut count = 0u32;
    let mut off = 0usize;
    while off.checked_add(46).is_some_and(|end| end <= data.len()) {
        let Some(pos) = data[off..].windows(4).position(|w| w == b"PK\x01\x02") else {
            break;
        };
        let Some(signature) = off.checked_add(pos) else {
            break;
        };
        let Some(record_end) = signature.checked_add(46) else {
            break;
        };
        if record_end > data.len() {
            break;
        }
        off = signature;
        let name_len = u16::from_le_bytes([data[off + 28], data[off + 29]]) as usize;
        let extra_len = u16::from_le_bytes([data[off + 30], data[off + 31]]) as usize;
        let comment_len = u16::from_le_bytes([data[off + 32], data[off + 33]]) as usize;
        let Some(end) = record_end
            .checked_add(name_len)
            .and_then(|end| end.checked_add(extra_len))
            .and_then(|end| end.checked_add(comment_len))
        else {
            break;
        };
        if end > data.len() {
            break;
        }
        let name = std::str::from_utf8(&data[off + 46..off + 46 + name_len]).unwrap_or("(invalid)");
        if !name.ends_with('/') {
            count += 1;
        }
        println!("  {} (offset {})", name, off);
        off = end;
    }
    println!("Total entries: {}", count);
    true
}

fn try_tar(data: &[u8]) -> bool {
    // TAR detection: ustar magic at offset 257
    if data.len() < 512 || &data[257..262] != b"ustar" {
        return false;
    }
    println!("Archive: TAR");
    let count = print_tar_entries(data, true);
    println!("Total entries: {}", count);
    true
}

fn try_gzip(data: &[u8]) -> bool {
    if data.len() < 18 || !data.starts_with(b"\x1f\x8b") || data[2] != 8 {
        return false;
    }
    println!("Archive: GZIP");
    // Try to decompress and show the first entries if it's a .tar.gz
    use std::io::Read;
    const MAX_GZIP_PREVIEW: usize = 16 * 1024 * 1024;
    let decoder = flate2::read::GzDecoder::new(data);
    let mut decompressed = Vec::new();
    let read_ok = decoder
        .take((MAX_GZIP_PREVIEW as u64) + 1)
        .read_to_end(&mut decompressed)
        .is_ok();
    let truncated = decompressed.len() > MAX_GZIP_PREVIEW;
    if truncated {
        decompressed.truncate(MAX_GZIP_PREVIEW);
    }
    if read_ok && !truncated && decompressed.len() >= 512 && &decompressed[257..262] == b"ustar" {
        println!("  (contains TAR archive)");
        print_tar_entries(&decompressed, false);
    } else {
        if truncated {
            println!(
                "  Uncompressed size: >{} bytes (preview truncated)",
                MAX_GZIP_PREVIEW
            );
        } else {
            println!(
                "  Uncompressed size: {} bytes (preview suppressed)",
                decompressed.len()
            );
        }
    }
    true
}

fn print_tar_entries(data: &[u8], show_links: bool) -> u32 {
    let mut off = 0usize;
    let mut count = 0;
    while off + 512 <= data.len() {
        if data[off] == 0 {
            break;
        }
        let name_end = data[off..off + 100]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(100);
        let name = std::str::from_utf8(&data[off..off + name_end]).unwrap_or("(invalid)");
        let Some(size) = parse_tar_octal(&data[off + 124..off + 136]) else {
            break;
        };
        let kind = match data.get(off + 156) {
            Some(&b'5') => "dir",
            Some(&b'2') if show_links => "link",
            _ => "file",
        };
        println!("  {} {:>12}  {}", kind, size, name);
        if show_links && !name.ends_with('/') {
            count += 1;
        }
        let Some(blocks) = size
            .checked_add(511)
            .and_then(|value| value.checked_div(512))
            .and_then(|value| value.checked_mul(512))
        else {
            break;
        };
        let Some(next) = usize::try_from(blocks)
            .ok()
            .and_then(|blocks| off.checked_add(512)?.checked_add(blocks))
        else {
            break;
        };
        if next > data.len() {
            break;
        }
        off = next;
    }
    count
}

fn parse_tar_octal(field: &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for &byte in field {
        if byte == 0 || byte == b' ' {
            continue;
        }
        if !(b'0'..=b'7').contains(&byte) {
            return None;
        }
        value = value.checked_mul(8)?.checked_add(u64::from(byte - b'0'))?;
    }
    Some(value)
}

// ── RLE animation (Fullerene custom format) ─────────────────────

fn try_rle(path: &str, data: &[u8]) -> bool {
    if data.len() < 16
        || &data[..4] != b"BARL"
        || u32::from_le_bytes([data[4], data[5], data[6], data[7]]) != 1
    {
        return false;
    }
    let frame_count = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let frame_width = u16::from_le_bytes([data[12], data[13]]) as u32;
    let frame_height = u16::from_le_bytes([data[14], data[15]]) as u32;
    println!("Animation: RLE");
    println!("Frames: {}", frame_count);
    println!("Resolution: {}x{}", frame_width, frame_height);

    if frame_width == 0
        || frame_height == 0
        || frame_width > 1920
        || frame_height > 1080
        || frame_count == 0
    {
        return true;
    }

    // Decode and display the first frame
    let pix_count = frame_width as usize * frame_height as usize;
    let mut pixels = vec![0u8; pix_count * 3];
    let Some(offset_table_bytes) = usize::try_from(frame_count)
        .ok()
        .and_then(|count| count.checked_mul(2))
    else {
        return true;
    };
    let Some(mut off) = 16usize.checked_add(offset_table_bytes) else {
        return true;
    };
    if off > data.len() {
        return true;
    }

    // Parse each frame's data (simple RLE: run of colors)
    let mut pi = 0usize;
    while off < data.len() && pi < pix_count && pi < 1920 * 1080 {
        let b = data[off];
        off += 1;
        if b & 0x80 != 0 {
            // Run of identical pixels
            let run = (b & 0x7f) as usize + 1;
            if off + 3 > data.len() {
                break;
            }
            let r = data[off];
            let g = data[off + 1];
            let b2 = data[off + 2];
            off += 3;
            for _ in 0..run {
                if pi < pix_count {
                    pixels[pi * 3] = r;
                    pixels[pi * 3 + 1] = g;
                    pixels[pi * 3 + 2] = b2;
                    pi += 1;
                }
            }
        } else {
            // Raw pixel
            let run = (b & 0x7f) as usize + 1;
            for _ in 0..run {
                if off + 3 > data.len() || pi >= pix_count {
                    break;
                }
                pixels[pi * 3] = data[off];
                pixels[pi * 3 + 1] = data[off + 1];
                pixels[pi * 3 + 2] = data[off + 2];
                off += 3;
                pi += 1;
            }
        }
    }

    let title = format!("Animation: {}", path);
    unsafe {
        let wid = create_window(
            title.as_ptr(),
            title.len() as u32,
            frame_width,
            frame_height,
        );
        if wid >= 0 {
            update_window(
                wid,
                frame_width,
                frame_height,
                pixels.as_ptr(),
                pixels.len() as u32,
            );
        }
    }
    println!("First frame decoded ({}x{})", frame_width, frame_height);
    true
}

// ── Hex fallback ───────────────────────────────────────────────

fn print_hex(path: &str, data: &[u8]) {
    println!("File: {}\nSize: {} bytes\nBinary data", path, data.len());
    let preview = data.len().min(256);
    for (i, chunk) in data[..preview].chunks(16).enumerate() {
        print!("{:08x}: ", i * 16);
        for b in chunk {
            print!("{:02x} ", b);
        }
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        VIDEO_REORDER_DEPTH, fit_dimensions, mp3_frame, push_pending_presentation_time,
        qoi_header_allowed, sample_presentation_time_ns, source_dimensions_allowed,
        take_earliest_presentation_time, try_image,
    };
    use image::GenericImageView;
    use image::ImageReader;
    use std::collections::VecDeque;
    use std::io::Cursor;

    #[unsafe(no_mangle)]
    extern "C" fn create_window(
        _title_ptr: *const u8,
        _title_len: u32,
        _width: u32,
        _height: u32,
    ) -> i32 {
        1
    }

    #[unsafe(no_mangle)]
    extern "C" fn update_window(
        _window_id: i32,
        _width: u32,
        _height: u32,
        _pixels_ptr: *const u8,
        _pixels_len: u32,
    ) -> i32 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn video_open(_config_ptr: *const u8, _config_len: u32) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn video_decode_sample(
        _sample_ptr: *const u8,
        _sample_len: u32,
        _length_size: u32,
    ) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn video_frame_info() -> u64 {
        (480u64 << 32) | 360
    }

    #[unsafe(no_mangle)]
    extern "C" fn video_present(_window_id: i32) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn video_discard() -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn video_flush() -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn video_stage_timing(_stage: u32) -> u64 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn video_close() -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn video_should_stop() -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn close_window(_window_id: i32) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn show_text(
        _title_ptr: *const u8,
        _title_len: u32,
        _text_ptr: *const u8,
        _text_len: u32,
    ) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn show_error(
        _title_ptr: *const u8,
        _title_len: u32,
        _msg_ptr: *const u8,
        _msg_len: u32,
    ) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn show_image(
        _width: u32,
        _height: u32,
        _pixels_ptr: *const u8,
        _pixels_len: u32,
    ) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn wait_for_ns(_duration_ns: u64) -> u32 {
        0
    }

    #[test]
    fn rejects_images_that_would_allocate_too_many_pixels() {
        assert!(!source_dimensions_allowed(4096, 4097));
        assert!(!source_dimensions_allowed(16_385, 1));
        assert!(source_dimensions_allowed(4096, 4096));
    }

    #[test]
    fn accepts_streamed_qoi_rgba_layout() {
        let pixels = [[255u8, 0, 0, 255], [0, 255, 0, 255]];
        let mut qoi = Vec::from(*b"qoif");
        qoi.extend_from_slice(&2u32.to_be_bytes());
        qoi.extend_from_slice(&1u32.to_be_bytes());
        qoi.extend_from_slice(&[4, 0]);
        for pixel in pixels {
            qoi.push(0xFF);
            qoi.extend_from_slice(&pixel);
        }
        qoi.extend_from_slice(&[0; 7]);
        qoi.push(1);

        let decoded = ImageReader::new(Cursor::new(qoi))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(decoded.dimensions(), (2, 1));
    }

    #[test]
    fn rejects_qoi_before_falling_through_other_probes() {
        let mut header = Vec::from(*b"qoif");
        header.extend_from_slice(&4096u32.to_be_bytes());
        header.extend_from_slice(&4096u32.to_be_bytes());
        header.extend_from_slice(&[4, 0]);
        header.extend_from_slice(&[0; 8]);
        assert!(!qoi_header_allowed(&header));
    }

    #[test]
    fn decodes_jpeg_through_the_bounded_image_path() {
        let source = image::RgbImage::from_pixel(2, 1, image::Rgb([32, 96, 160]));
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(source)
            .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();

        assert!(try_image("sample.jpg", &jpeg));
    }

    #[test]
    fn advances_mp3_by_the_encoded_frame_length() {
        let header = [0xff, 0xfb, 0x90, 0x64];
        let frame = mp3_frame(&header, 0).unwrap();
        assert_eq!(frame, (0, 128, 44_100, 2, 1152, 417));
    }

    #[test]
    fn uses_mpeg2_bitrate_table_and_frame_coefficient() {
        let header = [0xff, 0xf3, 0x80, 0x64];
        let frame = mp3_frame(&header, 0).unwrap();
        assert_eq!(frame, (0, 64, 22_050, 2, 576, 208));
    }

    #[test]
    fn fits_video_dimensions_without_distorting_aspect_ratio() {
        assert_eq!(fit_dimensions(1920, 1080, 800, 600), Some((800, 450)));
        assert_eq!(fit_dimensions(1080, 1920, 800, 600), Some((337, 600)));
        assert_eq!(fit_dimensions(320, 240, 800, 600), Some((320, 240)));
    }

    #[test]
    fn applies_mp4_composition_offset_to_presentation_time() {
        assert_eq!(
            sample_presentation_time_ns(3_000, 500, 1_000),
            3_500_000_000
        );
        assert_eq!(
            sample_presentation_time_ns(3_000, -500, 1_000),
            2_500_000_000
        );
        assert_eq!(sample_presentation_time_ns(0, -1, 1_000), 0);
    }

    #[test]
    fn reorders_decode_timestamps_for_b_frames() {
        let mut times = VecDeque::from([0, 66_666_666, 33_333_333]);
        assert_eq!(take_earliest_presentation_time(&mut times, 99), 0);
        assert_eq!(take_earliest_presentation_time(&mut times, 99), 33_333_333);
        assert_eq!(take_earliest_presentation_time(&mut times, 99), 66_666_666);
        assert_eq!(take_earliest_presentation_time(&mut times, 99), 99);
    }

    #[test]
    fn bounds_pending_presentation_timestamps_to_reorder_depth() {
        let mut times = VecDeque::new();
        for timestamp in 0..(VIDEO_REORDER_DEPTH + 4) as u64 {
            push_pending_presentation_time(&mut times, timestamp);
        }
        assert_eq!(times.len(), VIDEO_REORDER_DEPTH);
        assert_eq!(times.front(), Some(&4));
        assert_eq!(times.back(), Some(&(VIDEO_REORDER_DEPTH as u64 + 3)));
    }

    #[test]
    fn plays_optional_local_mp4_to_completion() {
        let Ok(path) = std::env::var("FULLERENE_MP4_TEST") else {
            return;
        };
        assert!(super::try_mp4_file(&path));
    }
}
