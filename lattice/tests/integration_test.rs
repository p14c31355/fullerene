use lattice::compositor::{Compositor, RenderTarget};
use lattice::cursor::Cursor;
use lattice::scene::Scene;
use lattice::surface::Surface;
use lattice::wm::WindowManager;

struct TestTarget {
    pixels: Vec<u32>,
    w: u32,
    h: u32,
}

impl RenderTarget for TestTarget {
    fn buffer(&mut self) -> &mut [u32] { &mut self.pixels }
    fn dimensions(&self) -> (u32, u32) { (self.w, self.h) }
}

impl TestTarget {
    fn new(w: u32, h: u32) -> Self { Self { pixels: vec![0u32; (w * h) as usize], w, h } }
}

/// Full‑pipeline integration test.
#[test]
fn test_full_pipeline() {
    const W: u32 = 320;
    const H: u32 = 200;
    let mut target = TestTarget::new(W, H);

    let mut wm = WindowManager::new();
    let _red_id = wm.create_window(10, 10, 100, 100, 0x0000FF);   // id=1
    let _blue_id = wm.create_window(60, 60, 100, 100, 0xFF0000); // id=2 (top)

    // ── Step 1: Composite without cursor ────────────────────
    let scene = Scene::new(wm.windows(), None, 0x808080);
    Compositor::render(&scene, &mut target);

    assert_eq!(target.pixels[0], 0x808080, "top‑left background");

    let red_only_idx = (15 * W + 15) as usize;
    assert_eq!(target.pixels[red_only_idx], 0x0000FF, "red region");

    let overlap_idx = (80 * W + 80) as usize;
    assert_eq!(target.pixels[overlap_idx], 0xFF0000, "overlap → blue (top)");

    // ── Step 2: Raise red to top ──────────────────────────
    wm.raise_to_top(lattice::window::WindowId(1));
    let scene = Scene::new(wm.windows(), None, 0x808080);
    Compositor::render(&scene, &mut target);

    assert_eq!(target.pixels[overlap_idx], 0x0000FF, "after raise → red");

    // ── Step 3: Drag red window via title bar ─────────────
    // Create a titled window for the test (replace red)
    let mut wm = WindowManager::new();
    let red_id = wm.create_titled_window(10, 10, 100, 100, 0x0000FF, "Red");
    let _blue_id = wm.create_titled_window(60, 60, 100, 100, 0xFF0000, "Blue");
    // Raise red so it's on top for the drag test
    wm.raise_to_top(red_id);

    // Click title bar (y=20 is inside title bar: window.y=10..30)
    wm.on_mouse_down(50, 20);
    wm.on_mouse_move(200, 100);

    let scene = Scene::new(wm.windows(), None, 0x808080);
    Compositor::render(&scene, &mut target);

    let old_red_idx = (15 * W + 15) as usize;
    assert_eq!(target.pixels[old_red_idx], 0x808080, "old red → background");

    // offset=(50-10,20-10)=(40,10), new=(200-40,100-10)=(160,90)
    // client area starts at y=90+TITLE_BAR_HEIGHT(20)=110
    let new_red_idx = (115 * W + 165) as usize;
    assert_eq!(target.pixels[new_red_idx], 0x0000FF, "new red position");

    wm.on_mouse_up();

    // ── Step 4: Composite with cursor ─────────────────────
    let cursor = Cursor::new(150, 100);
    let scene = Scene::new(wm.windows(), Some(&cursor), 0x808080);
    Compositor::render(&scene, &mut target);

    let cursor_idx = (100 * W + 150) as usize;
    assert_ne!(
        target.pixels[cursor_idx], 0x808080,
        "cursor hotspot not background"
    );
}

#[test]
fn test_surface_blit() {
    let mut dst = Surface::new(10, 10, 0x000000);
    let mut src = Surface::new(4, 4, 0xFFFFFF);
    src.fill_rect(1, 1, 2, 2, 0xFF0000);
    dst.blit_at(&src, 3, 3);

    assert_eq!(dst.get_pixel(4, 4), Some(0xFF0000));
    assert_eq!(dst.get_pixel(3, 3), Some(0xFFFFFF));
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
    assert!(!wm.remove_window(id));
}