use std::fmt::Write as _;

const BUILD_ID: &str = "2026-07-26-screenshot-mvp-3-capped-capture";
const DEFAULT_OUTPUT: &str = "/tmp/emulsion-screenshot.qoi";

#[link(wasm_import_module = "fullerene")]
unsafe extern "C" {
    fn screen_dimensions(width_ptr: *mut u32, height_ptr: *mut u32) -> u32;
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
    println!("Emulsion: capture begin");
    let (width, height, pixels) = capture_rgba()?;
    println!("Emulsion: capture complete {}x{}", width, height);
    let encoded = qoi::encode_to_vec(&pixels, width, height)
        .map_err(|error| format!("Cannot encode screenshot as QOI: {error}"))?;
    println!("Emulsion: qoi encode complete bytes={}", encoded.len());
    std::fs::write(output, &encoded)
        .map_err(|error| format!("Cannot save screenshot to '{output}': {error}"))?;
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

fn capture_rgba() -> Result<(u32, u32, Vec<u8>), String> {
    let mut width = 0u32;
    let mut height = 0u32;
    let dimensions_result = unsafe { screen_dimensions(&mut width, &mut height) };
    if dimensions_result != 0 {
        return Err(format!(
            "Cannot query desktop dimensions (host error {dimensions_result})."
        ));
    }
    let byte_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "The desktop dimensions are too large.".to_owned())?;
    if byte_len == 0 || byte_len > 32 * 1024 * 1024 {
        return Err("The desktop capture is outside the supported size.".to_owned());
    }
    println!("Emulsion: capture dimensions={width}x{height} bytes={byte_len}");
    println!("Emulsion: allocating capture bytes={byte_len}");
    // The host callback overwrites every byte. Avoid zero-initialising a
    // 14.7 MiB 2560x1440 buffer in the WASM interpreter before entering the
    // callback; that work can exhaust the synchronous execution budget and
    // makes the last visible line look like a capture deadlock.
    let mut pixels = Vec::with_capacity(byte_len);
    unsafe { pixels.set_len(byte_len); }
    println!("Emulsion: capture buffer allocated bytes={byte_len}");
    let result = unsafe {
        capture_screen(
            pixels.as_mut_ptr(),
            pixels.len() as u32,
            &mut width,
            &mut height,
        )
    };
    println!("Emulsion: capture host returned result={result}");
    if result != 0 {
        return Err(format!("Cannot capture the desktop (host error {result})."));
    }
    Ok((width, height, pixels))
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
