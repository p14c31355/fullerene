use image::{GenericImageView, ImageReader};
use std::io::Cursor;
use std::time::Instant;

const ITERATIONS: usize = 8;
const MAX_WIDTH: u32 = 800;
const MAX_HEIGHT: u32 = 600;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: cargo run --release --example bench_image -- <image>");
    let bytes = std::fs::read(&path).expect("image read failed");
    let start = Instant::now();
    let mut output = (0, 0, 0usize);

    for _ in 0..ITERATIONS {
        let image = ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .expect("image format detection failed")
            .decode()
            .expect("image decode failed");
        let image = if image.width() <= MAX_WIDTH && image.height() <= MAX_HEIGHT {
            image
        } else {
            image.thumbnail(MAX_WIDTH, MAX_HEIGHT)
        };
        let (width, height) = image.dimensions();
        let pixels = image.to_rgb8().into_raw();
        output = (width, height, pixels.len());
        std::hint::black_box(&output);
    }

    let elapsed = start.elapsed();
    println!(
        "{} iterations: {}x{} {} RGB bytes, total={:?}, per-frame={:?}",
        ITERATIONS,
        output.0,
        output.1,
        output.2,
        elapsed,
        elapsed / ITERATIONS as u32,
    );
}
