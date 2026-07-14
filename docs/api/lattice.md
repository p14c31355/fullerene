# Lattice — Public API (v0.1)

> **Status: DRAFT — Subject to Freeze**
>
> Lattice is a compositing window system combining structs, methods, and rendering traits.

---

## 1. Window — Window

`lattice::window::Window`

```rust
pub struct WindowId(pub u64);
pub struct Window {
    pub id: WindowId,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub surface: Surface,
    pub title: Option<String>,
    pub focused: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub restore_rect: Option<(i32, i32, u32, u32)>,
    pub shadow_surface: Option<Surface>,
}
```

---

## 2. Surface — Pixel Surface

`lattice::surface::Surface`

```rust
pub struct Surface { /* ... */ }

impl Surface {
    pub fn new(width: u32, height: u32, color: u32) -> Self;
    pub fn width(&self) -> u32;
    pub fn height(&self) -> u32;
    pub fn pixels(&self) -> &[u32];
    pub fn pixels_mut(&mut self) -> &mut [u32];
    pub fn blend(&mut self, x: u32, y: u32, src: &Surface);
    pub fn fill_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: u32);
    pub fn clear(&mut self, color: u32);
}
```

**Safety limit**: Surfaces exceeding 4096×2160 resolution are rejected and a 0×0 surface is returned.

---

## 3. Scene — Scene Graph

`lattice::scene::Scene`

```rust
pub struct DirtyRect {
    pub x: u32, pub y: u32, pub width: u32, pub height: u32,
}

pub enum Layer { Background, Windows, Overlay, SystemUi }

pub struct Scene {
    pub windows: Vec<Window>,
    pub cursor: Option<Cursor>,
    pub bg_color: u32,
    pub dirty: DirtyRect,
}
```

---

## 4. Cursor — Cursor

`lattice::cursor::Cursor`

```rust
pub struct Cursor {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
}
```

---

## 5. Renderer — Renderer

`lattice::renderer` module:

| Type | Role |
|---|---|
| `VecFramebuffer` | Vec-backed framebuffer for testing (supports PPM output) |
| `fn render_scene(scene: &Scene, fb: &mut [u32], stride: usize)` | Render a scene to the framebuffer |

---

## 6. Other Modules

| Module | Responsibility |
|---|---|
| `wm` | Window manager (focus, raise, resize) |
| `compositor` | Compositing management |
| `desktop` | Desktop state |
| `font` | Bitmap font rendering |
| `theme` | Color theme |
| `cursor` | Cursor rendering |
| `terminal_surface` | Terminal rendering |
| `top_panel` | Top panel (clock, status) |
| `taskbar` | Taskbar |
| `wallpaper` | Wallpaper management |
| `desktop_icons` | Desktop icons |

---

## Changelog

| Date | Change |
|---|---|
| 2026-07-13 | v0.1 initial |
