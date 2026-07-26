//! Small, repeatable host benchmark for the software compositor.
//!
//! Run with `cargo run -p lattice --release --example bench_render`.

use lattice::compositor::Compositor;
use lattice::renderer::VecFramebuffer;
use lattice::scene::{DirtyRect, Scene};
use lattice::window::{Window, WindowId};
use std::time::{Duration, Instant};

const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;
const ITERATIONS: usize = 120;

fn measure(scene: &Scene<'_>, target: &mut VecFramebuffer) -> Duration {
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        Compositor::render(scene, target);
    }
    start.elapsed()
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

    Compositor::render(&full, &mut target);
    let full_time = measure(&full, &mut target);
    let dirty_time = measure(&dirty, &mut target);

    println!(
        "{} iterations at {}x{}: full={:?} ({:?}/frame), disjoint-dirty={:?} ({:?}/frame)",
        ITERATIONS,
        WIDTH,
        HEIGHT,
        full_time,
        full_time / ITERATIONS as u32,
        dirty_time,
        dirty_time / ITERATIONS as u32,
    );
}
