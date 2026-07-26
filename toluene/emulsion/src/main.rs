use std::fmt::Write as _;

const BUILD_ID: &str = "2026-07-26-screenshot-mvp-3-capped-capture";
const DEFAULT_OUTPUT: &str = "/tmp/emulsion-screenshot.qoi";

#[link(wasm_import_module = "fullerene")]
unsafe extern "C" {
    fn screen_dimensions(width_ptr: *mut u32, height_ptr: *mut u32) -> u32;
    fn capture_screen_chunk(
        offset: u32,
        pixels_ptr: *mut u8,
        pixels_len: u32,
        width_ptr: *mut u32,
        height_ptr: *mut u32,
    ) -> u32;
    fn write_file_chunk(
        path_ptr: *const u8,
        path_len: u32,
        offset: u64,
        data_ptr: *const u8,
        data_len: u32,
        replace: u32,
    ) -> u32;
    fn show_text(title_ptr: *const u8, title_len: u32, text_ptr: *const u8, text_len: u32) -> u32;
    fn show_error(title_ptr: *const u8, title_len: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
}

fn main() {
    println!("Emulsion: build={BUILD_ID}");
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return;
    }

    let mode = args.get(1).map(String::as_str).unwrap_or("capture");
    match mode {
        "capture" | "screenshot" => {
            let output = args.get(2).map(String::as_str).unwrap_or(DEFAULT_OUTPUT);
            if let Err(message) = capture(output) {
                present_error(&message);
                std::process::exit(1);
            }
        }
        "record" | "screen-record" => {
            let message = "Screen recording is not available yet.\n\n".to_owned()
                + "The capture host API is ready for the next Emulsion milestone.";
            present_error(&message);
            std::process::exit(2);
        }
        path if !path.starts_with('-') => {
            if let Err(message) = capture(path) {
                present_error(&message);
                std::process::exit(1);
            }
        }
        _ => {
            present_error("Unknown Emulsion command. Use --help for usage.");
            std::process::exit(2);
        }
    }
}

fn print_usage() {
    println!("Usage:");
    println!("  emulsion                         Capture the full screen");
    println!("  emulsion capture [OUTPUT.qoi]    Capture the full screen");
    println!("  emulsion record [OUTPUT.frec]    Reserved for screen recording");
    println!();
    println!("The default output is {DEFAULT_OUTPUT}.");
}

fn capture(output: &str) -> Result<(), String> {
    println!("Emulsion: capture begin");
    let mut width = 0u32;
    let mut height = 0u32;
    let dimensions_result = unsafe { screen_dimensions(&mut width, &mut height) };
    if dimensions_result != 0 || width == 0 || height == 0 {
        return Err(format!(
            "Cannot query desktop dimensions (host error {dimensions_result})."
        ));
    }
    let total_bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "The desktop dimensions are too large.".to_owned())?;
    println!("Emulsion: capture stream dimensions={width}x{height} bytes={total_bytes}");

    let mut header = Vec::with_capacity(14);
    header.extend_from_slice(b"qoif");
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[4, 0]);
    write_chunk(output, 0, &header, true)?;

    // Keep both the WASM guest allocation and each host callback bounded.
    // The simple QOI RGBA opcode is valid and lets us stream without holding
    // the full image or encoded output in memory.
    const CHUNK_BYTES: usize = 256 * 1024;
    let mut pixels = vec![0u8; CHUNK_BYTES];
    let mut encoded = Vec::with_capacity(CHUNK_BYTES + CHUNK_BYTES / 4);
    let mut offset = 0usize;
    while offset < total_bytes {
        let chunk_len = (total_bytes - offset).min(pixels.len()) & !3;
        if chunk_len == 0 {
            return Err("Capture chunk alignment error.".to_owned());
        }
        let mut chunk_width = 0u32;
        let mut chunk_height = 0u32;
        let result = unsafe {
            capture_screen_chunk(
                offset as u32,
                pixels.as_mut_ptr(),
                chunk_len as u32,
                &mut chunk_width,
                &mut chunk_height,
            )
        };
        if result != 0 || (chunk_width, chunk_height) != (width, height) {
            return Err(format!(
                "Cannot capture desktop chunk at {offset} (host error {result})."
            ));
        }
        encoded.clear();
        for rgba in pixels[..chunk_len].chunks_exact(4) {
            encoded.push(0xFF);
            encoded.extend_from_slice(rgba);
        }
        write_chunk(output, 14 + encoded_offset(offset), &encoded, false)?;
        println!(
            "Emulsion: capture chunk complete offset={} bytes={}",
            offset, chunk_len
        );
        offset += chunk_len;
    }
    // Keep the end marker in one contiguous write. Some mounted filesystems
    // finalize/flush metadata per write, so splitting the marker can leave a
    // structurally complete-looking file that decoders reject.
    const QOI_END_MARKER: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 1];
    write_chunk(
        output,
        14 + encoded_offset(total_bytes),
        &QOI_END_MARKER,
        false,
    )?;
    println!("Emulsion: qoi stream complete");
    println!("Emulsion: file write complete path={output}");

    let mut report = String::new();
    let _ = writeln!(report, "Screenshot captured successfully.");
    let _ = writeln!(report, "Resolution: {width}x{height}");
    let _ = writeln!(report, "Format: QOI");
    let _ = writeln!(report, "Saved to: {output}");
    present_text("Emulsion", &report);
    println!("{report}");
    Ok(())
}

fn encoded_offset(rgba_offset: usize) -> u64 {
    (rgba_offset / 4).saturating_mul(5) as u64
}

fn write_chunk(path: &str, offset: u64, data: &[u8], replace: bool) -> Result<(), String> {
    let result = unsafe {
        write_file_chunk(
            path.as_ptr(),
            path.len() as u32,
            offset,
            data.as_ptr(),
            data.len() as u32,
            replace as u32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(format!(
            "Cannot write screenshot chunk at offset {offset} (host error {result})."
        ))
    }
}

fn present_text(title: &str, message: &str) {
    unsafe {
        show_text(
            title.as_ptr(),
            title.len() as u32,
            message.as_ptr(),
            message.len() as u32,
        );
    }
}

fn present_error(message: &str) {
    unsafe {
        show_error(
            b"Emulsion error".as_ptr(),
            b"Emulsion error".len() as u32,
            message.as_ptr(),
            message.len() as u32,
        );
    }
}
