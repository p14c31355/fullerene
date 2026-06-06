use lattice::compositor::Compositor;
use lattice::desktop::Desktop;
use lattice::renderer::VecFramebuffer;
use std::fs;

fn main() {
    // ── Create a virtual 640×480 framebuffer ────────────────
    let mut target = VecFramebuffer::new(640, 480);

    // ── Create desktop with a dark wallpaper ─────────────────
    let mut desktop = Desktop::new(0x202020); // dark gray

    // Window 1: blue (0x4466FF) at (30, 30), 300×200
    desktop.create_window(30, 30, 300, 200, 0x4466FFu32);

    // Window 2: red (0xFF6644) at (180, 100), 300×200 (overlaps blue)
    desktop.create_window(180, 100, 300, 200, 0xFF6644u32);

    // Window 3: green (0x44BB44) at (350, 280), 250×150
    desktop.create_window(350, 280, 250, 150, 0x44BB44u32);

    // ── Frame 1: Initial layout (no cursor) ────────────────
    let scene = lattice::scene::Scene::new(desktop.wm.windows(), None, 0x202020);
    Compositor::render(&scene, &mut target);
    fs::write("/tmp/lattice_frame1.ppm", target.to_ppm_bytes())
        .expect("Failed to write frame1 PPM");
    println!("Wrote /tmp/lattice_frame1.ppm");

    // Verify frame 1
    let px = &target.pixels;
    assert_eq!(px[50 * 640 + 50], 0x4466FF, "blue region");
    assert_eq!(px[120 * 640 + 200], 0xFF6644, "red region (top)");
    assert_eq!(px[300 * 640 + 400], 0x44BB44, "green region (topmost)");
    assert_eq!(px[0], 0x202020, "background");

    // ── Frame 2: Raise red to top & drag it ────────────────
    desktop.set_cursor(200, 120);
    desktop.mouse_down(640, 480); // raises red to top, begin drag
    desktop.mouse_move(300, 150);
    desktop.mouse_up();

    let scene = desktop.scene();
    Compositor::render(&scene, &mut target);
    fs::write("/tmp/lattice_frame2.ppm", target.to_ppm_bytes())
        .expect("Failed to write frame2 PPM");
    println!("Wrote /tmp/lattice_frame2.ppm");

    // Verify frame 2
    let px = &target.pixels;
    // Old red position → blue behind
    assert_eq!(px[120 * 640 + 200], 0x4466FF, "old red → blue behind");
    // New red position (280, 130)
    assert_eq!(px[135 * 640 + 285], 0xFF6644, "new red position");

    // ── Frame 3: Move cursor ────────────────────────────────
    desktop.set_cursor(100, 50);

    let scene = desktop.scene();
    Compositor::render(&scene, &mut target);
    fs::write("/tmp/lattice_frame3.ppm", target.to_ppm_bytes())
        .expect("Failed to write frame3 PPM");
    println!("Wrote /tmp/lattice_frame3.ppm");

    // Cursor visible
    let px = &target.pixels;
    assert_ne!(px[55 * 640 + 105], 0x202020, "cursor body not background");
    assert_ne!(px[55 * 640 + 105], 0x4466FF, "cursor body ≠ blue");

    println!("All pixel assertions passed!");
    println!("Done! View with: eog /tmp/lattice_frame1.ppm");
}
