use std::fmt::Write as _;

const BUILD_ID: &str = "2026-07-26-screenshot-mvp-1";
const DEFAULT_OUTPUT: &str = "/tmp/emulsion-screenshot.qoi";

#[link(wasm_import_module = "fullerene")]
unsafe extern "C" {
    fn capture_screen(
        pixels_ptr: *mut u8,
        pixels_len: u32,
        width_ptr: *mut u32,
        height_ptr: *mut u32,
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
    let (width, height, pixels) = capture_rgba()?;
    let encoded = encode_qoi(width, height, &pixels)?;
    std::fs::write(output, &encoded)
        .map_err(|error| format!("Cannot save screenshot to '{output}': {error}"))?;

    let mut report = String::new();
    let _ = writeln!(report, "Screenshot captured successfully.");
    let _ = writeln!(report, "Resolution: {width}x{height}");
    let _ = writeln!(report, "Format: QOI");
    let _ = writeln!(report, "Saved to: {output}");
    present_text("Emulsion", &report);
    println!("{report}");
    Ok(())
}

fn capture_rgba() -> Result<(u32, u32, Vec<u8>), String> {
    let mut width = 0u32;
    let mut height = 0u32;
    let mut pixels = vec![0u8; 32 * 1024 * 1024];
    let result = unsafe {
        capture_screen(
            pixels.as_mut_ptr(),
            pixels.len() as u32,
            &mut width,
            &mut height,
        )
    };
    if result != 0 {
        return Err(format!("Cannot capture the desktop (host error {result})."));
    }
    let byte_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "The desktop dimensions are too large.".to_owned())?;
    if byte_len > pixels.len() {
        return Err("The desktop capture did not fit in the WASM buffer.".to_owned());
    }
    pixels.truncate(byte_len);
    Ok((width, height, pixels))
}

fn encode_qoi(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "The desktop dimensions are too large.".to_owned())?;
    if rgba.len() != pixel_count.saturating_mul(4) {
        return Err("The host returned an invalid pixel buffer.".to_owned());
    }

    let mut output = Vec::with_capacity(rgba.len().saturating_add(32));
    output.extend_from_slice(b"qoif");
    output.extend_from_slice(&width.to_be_bytes());
    output.extend_from_slice(&height.to_be_bytes());
    output.push(4); // RGBA
    output.push(0); // sRGB with linear alpha

    let mut index = [[0u8; 4]; 64];
    let mut previous = [0u8, 0, 0, 255];
    let mut run = 0u8;

    for pixel in rgba.chunks_exact(4) {
        let current = [pixel[0], pixel[1], pixel[2], pixel[3]];
        if current == previous {
            run = run.saturating_add(1);
            if run == 62 {
                output.push(0xC0 | (run - 1));
                run = 0;
            }
            continue;
        }
        flush_run(&mut output, &mut run);

        let slot = qoi_index(&current);
        if index[slot] == current {
            output.push(slot as u8);
        } else {
            index[slot] = current;
            if current[3] == previous[3] {
                let dr = current[0] as i16 - previous[0] as i16;
                let dg = current[1] as i16 - previous[1] as i16;
                let db = current[2] as i16 - previous[2] as i16;
                if (-2..=1).contains(&dr) && (-2..=1).contains(&dg) && (-2..=1).contains(&db) {
                    output.push(
                        0x40 | (((dr + 2) as u8) << 4) | (((dg + 2) as u8) << 2) | (db + 2) as u8,
                    );
                } else {
                    output.push(0xFE);
                    output.extend_from_slice(&current[..3]);
                }
            } else {
                output.push(0xFF);
                output.extend_from_slice(&current);
            }
        }
        previous = current;
    }
    flush_run(&mut output, &mut run);
    output.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    Ok(output)
}

fn qoi_index(pixel: &[u8; 4]) -> usize {
    (pixel[0] as usize * 3 + pixel[1] as usize * 5 + pixel[2] as usize * 7 + pixel[3] as usize * 11)
        % 64
}

fn flush_run(output: &mut Vec<u8>, run: &mut u8) {
    if *run != 0 {
        output.push(0xC0 | (*run - 1));
        *run = 0;
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
