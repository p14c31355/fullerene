//! Small, repeatable host benchmark for the software compositor.
//!
//! Run with `cargo run -p lattice --release --example bench_render`.
//!
//! Reports full-frame render time, disjoint dirty-region render time, per-frame
//! cost, pixels/second throughput, and the dirty-region speedup ratio across a
//! growing number of disjoint dirty rects. The dirty-region path is the main
//! runtime optimization (see `docs/ARCHITECTURE.md` section 7/8 and
//! `docs/BUG_JOURNAL.md` Entry 002): because the RAM back buffer persists
//! untouched pixels, rendering only the clipped dirty regions should stay
//! near-constant per region rather than scaling with screen area.

use lattice::compositor::Compositor;
use lattice::renderer::VecFramebuffer;
use lattice::scene::{DirtyRect, Scene};
use lattice::window::{Window, WindowId};
use std::time::{Duration, Instant};

const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;
const ITERATIONS: usize = 120;
const RECT_SIZE: u32 = 32;

fn measure(scene: &Scene<'_>, target: &mut VecFramebuffer) -> Duration {
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        Compositor::render(scene, target);
    }
    start.elapsed()
}

/// Build `count` disjoint 32x32 dirty rects spread across the framebuffer.
fn disjoint_rects(count: usize) -> Vec<DirtyRect> {
    let stride = (WIDTH / RECT_SIZE) as usize;
    (0..count)
        .map(|i| {
            let col = (i % stride) as u32;
            let row = (i / stride) as u32;
            DirtyRect::new(
                col * RECT_SIZE,
                row * RECT_SIZE,
                RECT_SIZE,
                RECT_SIZE,
            )
        })
        .collect()
}

fn main() {
    let windows = [
        Window::new(WindowId(1), 24, 32, 640, 420, 0x4466FF),
        Window::new(WindowId(2), 260, 180, 560, 400, 0xFF6644),
    ];
    let full = Scene::new(&windows, None, 0x202020);
    let dirty_rects = [
        DirtyRect::new(0, 0, 32, 32),
        DirtyRect::new(960, 700, 32, 32),
    ];
    let dirty = Scene::with_dirty_rects(&windows, None, 0x202020, &dirty_rects);
    let mut target = VecFramebuffer::new(WIDTH, HEIGHT);
    let frame_pixels = u64::from(WIDTH) * u64::from(HEIGHT);

    // Warm up once so the first measurement is not dominated by one-time setup.
    Compositor::render(&full, &mut target);

    let full_time = measure(&full, &mut target);
    let dirty_time = measure(&dirty, &mut target);
    let full_per_frame = full_time / ITERATIONS as u32;
    let dirty_per_frame = dirty_time / ITERATIONS as u32;

    println!(
        "{} iterations at {}x{}: full={:?} ({:?}/frame), disjoint-dirty={:?} ({:?}/frame)",
        ITERATIONS,
        WIDTH,
        HEIGHT,
        full_time,
        full_per_frame,
        dirty_time,
        dirty_per_frame,
    );
    println!(
        "throughput: full={:.2} Mpix/s, dirty speedup={:.2}x (per-frame cost ratio)",
        frame_pixels as f64 / full_per_frame.as_secs_f64() / 1.0e6,
        full_per_frame.as_secs_f64() / dirty_per_frame.as_secs_f64(),
    );

    println!("dirty-region scaling (rects -> per-frame, speedup vs full):");
    for &count in &[1usize, 2, 4, 8, 16, 32] {
        let rects = disjoint_rects(count);
        let scene = Scene::with_dirty_rects(&windows, None, 0x202020, &rects);
        let t = measure(&scene, &mut target) / ITERATIONS as u32;
        println!(
            "  {:>2} rects: {:?}/frame ({:.2}x)",
            count,
            t,
            full_per_frame.as_secs_f64() / t.as_secs_f64(),
        );
    }
}
