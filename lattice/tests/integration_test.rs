use lattice::compositor::Compositor;
use lattice::cursor::Cursor;
use lattice::surface::Surface;
use lattice::wm::WindowManager;

/// Full‑pipeline integration test:
///
/// 1. Create a 320×200 framebuffer
/// 2. Create two overlapping windows (red + blue) via WM
/// 3. Composite → verify red and blue pixels are present
/// 4. Verify z‑order: overlapping region should be blue (top)
/// 5. Drag the red window on top
/// 6. Composite again → overlapping region should now be red
/// 7. Verify cursor drawing
#[test]
fn test_full_pipeline() {
    const W: u32 = 320;
    const H: u32 = 200;
    let mut fb = vec![0u32; (W * H) as usize];

    let mut wm = WindowManager::new();

    // Create a red window at (10, 10), 100×100
    let red_id = wm.create_window(10, 10, 100, 100, 0x0000FF); // red
    // Create a blue window at (60, 60), 100×100 (overlapping)
    let _blue_id = wm.create_window(60, 60, 100, 100, 0xFF0000); // blue (BGRA? No — we treat 0xRRGGBB)

    // ── Step 1: Composite without cursor ────────────────────
    Compositor::composite(&mut fb, W, H, wm.windows(), 0x808080, None);

    // Background pixel (should be gray)
    assert_eq!(fb[0], 0x808080, "top-left should be background");

    // Red-only region: (15, 15) — inside red window, outside blue
    let red_only_idx = (15 * W + 15) as usize;
    assert_eq!(
        fb[red_only_idx], 0x0000FF,
        "non‑overlapping red region should be red"
    );

    // Overlapping region (80, 80) — inside both; blue is on top
    let overlap_idx = (80 * W + 80) as usize;
    assert_eq!(
        fb[overlap_idx], 0xFF0000,
        "overlapping region should show blue (top window)"
    );

    // ── Step 2: Raise red to top and re‑composite ────────────
    wm.raise_to_top(red_id);
    Compositor::composite(&mut fb, W, H, wm.windows(), 0x808080, None);

    // Overlapping region should now be red
    assert_eq!(
        fb[overlap_idx], 0x0000FF,
        "after raise, overlapping region should be red"
    );

    // ── Step 3: Drag red window ──────────────────────────────
    wm.on_mouse_down(15, 15); // grab red at (15,15) → offset (5,5)
    wm.on_mouse_move(200, 100); // move to (200,100) → red top‑left at (195,95)

    Compositor::composite(&mut fb, W, H, wm.windows(), 0x808080, None);

    // Red's old position (15, 15) — neither window covers it (blue is at (60,60)),
    // so it should be background gray.
    let old_red_idx = (15 * W + 15) as usize;
    assert_eq!(
        fb[old_red_idx], 0x808080,
        "old red position should be background (neither window covers it)"
    );

    // Red's new position: top‑left at (195, 95)
    let new_red_idx = (100 * W + 200) as usize;
    assert_eq!(
        fb[new_red_idx], 0x0000FF,
        "new red position should show red"
    );

    wm.on_mouse_up();

    // ── Step 4: Composite with cursor ────────────────────────
    let cursor = Cursor::new(150, 100);
    Compositor::composite(&mut fb, W, H, wm.windows(), 0x808080, Some(&cursor));

    // Cursor pixel at hotspot (150, 100) should be non‑background
    let cursor_idx = (100 * W + 150) as usize;
    assert_ne!(
        fb[cursor_idx], 0x808080,
        "cursor hotspot should not be background"
    );
}

#[test]
fn test_surface_blit() {
    // Unit‑style test for Surface blit correctness
    let mut dst = Surface::new(10, 10, 0x000000);
    let mut src = Surface::new(4, 4, 0xFFFFFF);

    // Draw a pattern on src
    src.fill_rect(1, 1, 2, 2, 0xFF0000);

    // Blit src onto dst at (3, 3)
    dst.blit_at(&src, 3, 3);

    // Pixel (4, 4) in dst should be red (blitted from src(1,1))
    assert_eq!(dst.get_pixel(4, 4), Some(0xFF0000));

    // Pixel (3, 3) should be white (from src(0,0))
    assert_eq!(dst.get_pixel(3, 3), Some(0xFFFFFF));

    // Pixel (0, 0) should still be black (uncovered)
    assert_eq!(dst.get_pixel(0, 0), Some(0x000000));
}

#[test]
fn test_remove_window() {
    let mut wm = WindowManager::new();
    let id = wm.create_window(0, 0, 100, 100, 0xFF0000);
    assert_eq!(wm.focused(), Some(id));
    assert!(wm.remove_window(id));
    assert_eq!(wm.focused(), None);
    assert!(wm.windows().is_empty());
    assert!(!wm.remove_window(id)); // already removed
}