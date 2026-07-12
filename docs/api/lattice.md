# Lattice — Public API (v0.1)

> **Status: DRAFT — 凍結予定**
>
> Lattice は compositing window system。trait ではなく構造体とそのメソッドで構成される。

---

## 1. Window — ウィンドウ

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

## 2. Surface — ピクセルサーフェス

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

**安全制限**: 4K解像度 (3840×2160) を超える領域は拒否され、0×0 の surface が返る。

---

## 3. Scene — シーングラフ

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

## 4. Cursor — カーソル

`lattice::cursor::Cursor`

```rust
pub struct Cursor {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
}
```

---

## 5. Renderer — レンダラ

`lattice::renderer` モジュール:

| 型 | 役割 |
|---|---|
| `VecFramebuffer` | テスト用 vec-backed framebuffer (PPM出力対応) |
| `fn render_scene(scene: &Scene, fb: &mut [u32], stride: usize)` | シーンをframebufferに描画 |

---

## 6. その他モジュール

| モジュール | 責務 |
|---|---|
| `wm` | ウィンドウマネージャ (focus, raise, resize) |
| `compositor` | コンポジット管理 |
| `desktop` | デスクトップ状態 |
| `font` | ビットマップフォントレンダリング |
| `theme` | カラーテーマ |
| `cursor` | カーソル描画 |
| `terminal_surface` | ターミナル描画 |
| `top_panel` | トップパネル (clock, status) |
| `taskbar` | タスクバー |
| `wallpaper` | 壁紙管理 |
| `desktop_icons` | デスクトップアイコン |

---

## 変更履歴

| 日付 | 変更 |
|---|---|
| 2026-07-13 | v0.1 初版 |
