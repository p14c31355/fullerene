//! Explorer — interactive file manager with context-driven state.
//!
//! Uses the "context pattern": `ExplorerContext` holds all file‑manager
//! state (current directory, selection, clipboard, history, sort mode),
//! and rendering functions take `&ExplorerContext` to draw the UI.
//!
//! Layout:
//! ```text
//! +-----------------------------------------------------------+
//! | ← → ↑   /home/user/Documents                    [?]       |  Breadcrumb / toolbar
//! +--------------------+--------------------------------------+
//! | Favorites          |  Name            Size      Modified  |
//! |                    |--------------------------------------|
//! | Home               |  📁 Desktop                        |
//! | Desktop            |  📁 Downloads                      |
//! | Downloads          |  📄 notes.txt                      |
//! | Documents          |  📄 kernel.rs                      |
//! | Music              |  📁 src                           |
//! | Pictures           |                                      |
//! +--------------------+--------------------------------------+
//! | 23 items                                              /  |  Status bar
//! +-----------------------------------------------------------+
//! ```

use crate::SOLVENT_CALLBACKS;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use lattice::surface::Surface;
use lattice::window::WindowId;

// ── Layout constants ───────────────────────────────────────────
pub const GLYPH_W: u32 = 8;
pub const GLYPH_H: u32 = 16;
pub const SIDEBAR_WIDTH: u32 = 140;
pub const TOOLBAR_HEIGHT: u32 = 24;
pub const STATUSBAR_HEIGHT: u32 = 20;
pub const ROW_HEIGHT: u32 = 20;
pub const CONTEXT_MENU_W: u32 = 150;

// ── Colors ─────────────────────────────────────────────────────
pub const EXPLORER_BG: u32 = 0x1E1E2E;
pub const SIDEBAR_BG: u32 = 0x252536;
pub const TOOLBAR_BG: u32 = 0x1A1A28;
pub const STATUSBAR_BG: u32 = 0x1A1A28;
pub const HEADER_BG: u32 = 0x222232;
pub const ROW_SELECTED: u32 = 0x3A7BD5;
pub const ROW_ALT: u32 = 0x222232;
pub const SIDEBAR_ACTIVE: u32 = 0x3A3A5E;
pub const FOLDER_COLOR: u32 = 0xE6A817;
pub const FILE_COLOR: u32 = 0xCCCCCC;
pub const TEXT_COLOR: u32 = 0xE0E0E0;
pub const MUTED_COLOR: u32 = 0x888888;
pub const HEADER_TEXT: u32 = 0xAAAAAA;
pub const DIVIDER_COLOR: u32 = 0x333344;
pub const CMENU_BG: u32 = 0x2A2A3E;
pub const CMENU_HOVER: u32 = 0x3A7BD5;

// ── Sort mode ──────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    Name,
    Size,
    Date,
}

// ── Entry display info ────────────────────────────────────────
#[derive(Debug, Clone)]
pub struct FileEntryDisplay {
    pub name: String,
    pub size_str: String,
    pub is_dir: bool,
}

// ── Sidebar item ───────────────────────────────────────────────
#[derive(Debug, Clone)]
pub struct SidebarItem {
    pub label: String,
    pub path: String,
}

// ── Context menu action ───────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplorerAction {
    Open,
    Copy,
    Paste,
    Rename,
    Delete,
    Properties,
    None,
}

const CONTEXT_MENU_ITEMS: &[&str] = &["Open", "Copy", "Paste", "Rename", "Delete", "Properties"];

fn context_menu_action(idx: usize) -> ExplorerAction {
    match idx {
        0 => ExplorerAction::Open,
        1 => ExplorerAction::Copy,
        2 => ExplorerAction::Paste,
        3 => ExplorerAction::Rename,
        4 => ExplorerAction::Delete,
        5 => ExplorerAction::Properties,
        _ => ExplorerAction::None,
    }
}

// ── ExplorerContext ────────────────────────────────────────────

pub struct ExplorerContext {
    pub window_id: Option<WindowId>,
    pub current_dir: String,
    pub entries: Vec<FileEntryDisplay>,
    pub raw_names: Vec<String>,
    pub raw_is_dir: Vec<bool>,
    pub selected_index: Option<usize>,
    pub scroll_offset: usize,
    pub sidebar_items: Vec<SidebarItem>,
    pub selected_sidebar: Option<usize>,
    pub history: Vec<String>,
    pub history_pos: usize,
    pub sort_mode: SortMode,
    pub sort_ascending: bool,
    pub hovered_index: Option<usize>,
    pub context_menu: ContextMenuState,
    // double-click tracking
    pub last_click_entry: Option<usize>,
    pub last_click_tick: u64,
}

#[derive(Debug, Clone)]
pub struct ContextMenuState {
    pub open: bool,
    pub x: u32,
    pub y: u32,
    pub hovered_item: Option<usize>,
}

impl ContextMenuState {
    pub const fn closed() -> Self {
        Self { open: false, x: 0, y: 0, hovered_item: None }
    }
}

impl ExplorerContext {
    pub fn new() -> Self {
        Self {
            window_id: None,
            current_dir: String::from("/"),
            entries: Vec::new(),
            raw_names: Vec::new(),
            raw_is_dir: Vec::new(),
            selected_index: None,
            scroll_offset: 0,
            sidebar_items: Self::default_sidebar(),
            selected_sidebar: Some(0),
            history: vec![String::from("/")],
            history_pos: 0,
            sort_mode: SortMode::Name,
            sort_ascending: true,
            hovered_index: None,
            context_menu: ContextMenuState::closed(),
            last_click_entry: None,
            last_click_tick: 0,
        }
    }

    fn default_sidebar() -> Vec<SidebarItem> {
        vec![
            SidebarItem { label: String::from("Home"), path: String::from("/") },
            SidebarItem { label: String::from("Desktop"), path: String::from("/Desktop") },
            SidebarItem { label: String::from("Downloads"), path: String::from("/Downloads") },
            SidebarItem { label: String::from("Documents"), path: String::from("/Documents") },
            SidebarItem { label: String::from("Music"), path: String::from("/Music") },
            SidebarItem { label: String::from("Pictures"), path: String::from("/Pictures") },
        ]
    }

    pub fn navigate_to(&mut self, path: &str) {
        let path_str = String::from(path);
        let readdir = match SOLVENT_CALLBACKS.lock().vfs_readdir {
            Some(f) => f,
            None => return,
        };
        match readdir(&path_str) {
            Ok(entries) => {
                self.current_dir = path_str.clone();
                self.raw_names = entries.iter().map(|e| e.name.clone()).collect();
                self.raw_is_dir = entries.iter().map(|e| e.is_dir).collect();
                self.entries = entries.iter().map(|e| {
                    let size_str = if e.is_dir {
                        String::from("<DIR>")
                    } else {
                        format_size(e.size)
                    };
                    FileEntryDisplay {
                        name: e.name.clone(),
                        size_str,
                        is_dir: e.is_dir,
                    }
                }).collect();
                self.sort_entries();
                self.selected_index = None;
                self.scroll_offset = 0;
                if self.history_pos >= self.history.len()
                    || self.history[self.history_pos] != path_str
                {
                    self.history.truncate(self.history_pos + 1);
                    self.history.push(path_str);
                    self.history_pos = self.history.len() - 1;
                }
            }
            Err(_) => {}
        }
    }

    pub fn refresh(&mut self) {
        let dir = self.current_dir.clone();
        self.navigate_to(&dir);
    }

    fn sort_entries(&mut self) {
        let ascending = self.sort_ascending;
        // Keep directories first
        let mut indices: Vec<usize> = (0..self.entries.len()).collect();
        indices.sort_by(|&a, &b| {
            let ea = &self.entries[a];
            let eb = &self.entries[b];
            if ea.is_dir != eb.is_dir {
                return eb.is_dir.cmp(&ea.is_dir);
            }
            let cmp = match self.sort_mode {
                SortMode::Name => ea.name.cmp(&eb.name),
                SortMode::Size => {
                    let sa = parse_size_for_sort(&ea.size_str);
                    let sb = parse_size_for_sort(&eb.size_str);
                    sa.cmp(&sb)
                }
                SortMode::Date => ea.name.cmp(&eb.name),
            };
            if ascending { cmp } else { cmp.reverse() }
        });
        let sorted_entries: Vec<FileEntryDisplay> =
            indices.iter().map(|&i| self.entries[i].clone()).collect();
        let sorted_names: Vec<String> = indices.iter().map(|&i| self.raw_names[i].clone()).collect();
        let sorted_is_dir: Vec<bool> = indices.iter().map(|&i| self.raw_is_dir[i]).collect();
        self.entries = sorted_entries;
        self.raw_names = sorted_names;
        self.raw_is_dir = sorted_is_dir;
    }

    pub fn go_back(&mut self) {
        if self.history_pos > 0 {
            self.history_pos -= 1;
            let path = self.history[self.history_pos].clone();
            let readdir = match SOLVENT_CALLBACKS.lock().vfs_readdir {
                Some(f) => f,
                None => return,
            };
            if let Ok(entries) = readdir(&path) {
                self.current_dir = path;
                self.raw_names = entries.iter().map(|e| e.name.clone()).collect();
                self.raw_is_dir = entries.iter().map(|e| e.is_dir).collect();
                self.entries = entries.iter().map(|e| {
                    let size_str = if e.is_dir { String::from("<DIR>") } else { format_size(e.size) };
                    FileEntryDisplay { name: e.name.clone(), size_str, is_dir: e.is_dir }
                }).collect();
                self.sort_entries();
                self.selected_index = None;
                self.scroll_offset = 0;
            }
        }
    }

    pub fn go_forward(&mut self) {
        if self.history_pos + 1 < self.history.len() {
            self.history_pos += 1;
            let path = self.history[self.history_pos].clone();
            let readdir = match SOLVENT_CALLBACKS.lock().vfs_readdir {
                Some(f) => f,
                None => return,
            };
            if let Ok(entries) = readdir(&path) {
                self.current_dir = path;
                self.raw_names = entries.iter().map(|e| e.name.clone()).collect();
                self.raw_is_dir = entries.iter().map(|e| e.is_dir).collect();
                self.entries = entries.iter().map(|e| {
                    let size_str = if e.is_dir { String::from("<DIR>") } else { format_size(e.size) };
                    FileEntryDisplay { name: e.name.clone(), size_str, is_dir: e.is_dir }
                }).collect();
                self.sort_entries();
                self.selected_index = None;
                self.scroll_offset = 0;
            }
        }
    }

    pub fn go_up(&mut self) {
        let parent = parent_path(&self.current_dir);
        self.navigate_to(&parent);
    }

    pub fn open_selected(&mut self, idx: usize) {
        if idx < self.raw_is_dir.len() && self.raw_is_dir[idx] {
            let name = &self.raw_names[idx];
            let new_path = join_path(&self.current_dir, name);
            self.navigate_to(&new_path);
        }
        // For files, we would open them in an appropriate app
    }
}

// ── Path helpers ───────────────────────────────────────────────

fn parent_path(path: &str) -> String {
    if path == "/" { return String::from("/"); }
    let trimmed = path.trim_end_matches('/');
    if let Some(pos) = trimmed.rfind('/') {
        if pos == 0 { String::from("/") } else { String::from(&trimmed[..pos]) }
    } else {
        String::from("/")
    }
}

fn join_path(base: &str, name: &str) -> String {
    if base == "/" {
        format!("/{}", name)
    } else {
        format!("{}/{}", base.trim_end_matches('/'), name)
    }
}

fn format_size(size: u64) -> String {
    if size >= 1048576 {
        format!("{}.{} MB", size / 1048576, ((size % 1048576) * 10) / 1048576)
    } else if size >= 1024 {
        format!("{}.{} KB", size / 1024, (size % 1024) * 10 / 1024)
    } else {
        format!("{} B", size)
    }
}

fn parse_size_for_sort(s: &str) -> u64 {
    if s == "<DIR>" { return 0; }
    let mut num = 0u64;
    for c in s.bytes() {
        if c >= b'0' && c <= b'9' {
            num = num * 10 + (c - b'0') as u64;
        }
    }
    if s.ends_with("KB") { num * 1024 }
    else if s.ends_with("MB") { num * 1048576 }
    else { num }
}

// ── Hit testing ───────────────────────────────────────────────

pub fn hit_file_list(
    ctx: &ExplorerContext,
    win_w: u32,
    win_h: u32,
    rx: i32,
    ry: i32,
) -> Option<usize> {
    let lx = SIDEBAR_WIDTH as i32;
    let ly = TOOLBAR_HEIGHT as i32;
    let lw = win_w as i32 - lx;
    let lh = (win_h - TOOLBAR_HEIGHT - STATUSBAR_HEIGHT) as i32;
    if rx < lx || rx >= lx + lw || ry < ly || ry >= ly + lh {
        return None;
    }
    // Skip header row
    let content_y = ly + ROW_HEIGHT as i32;
    if ry < content_y { return None; }
    let row = ((ry - content_y) as u32) / ROW_HEIGHT;
    let idx = ctx.scroll_offset + row as usize;
    if idx < ctx.entries.len() { Some(idx) } else { None }
}

pub fn hit_sidebar(ctx: &ExplorerContext, rx: i32, ry: i32) -> Option<usize> {
    if rx < 0 || rx >= SIDEBAR_WIDTH as i32 || ry < TOOLBAR_HEIGHT as i32 { return None; }
    let rel = (ry - TOOLBAR_HEIGHT as i32) as u32;
    let idx = rel / ROW_HEIGHT;
    if idx < ctx.sidebar_items.len() as u32 { Some(idx as usize) } else { None }
}

pub fn hit_toolbar_button(rx: i32, ry: i32) -> Option<u8> {
    if ry < 0 || ry >= TOOLBAR_HEIGHT as i32 { return None; }
    if rx >= 0 && rx < 28 { Some(b'b') }   // back
    else if rx >= 28 && rx < 56 { Some(b'f') }  // forward
    else if rx >= 56 && rx < 84 { Some(b'u') }  // up
    else if rx >= 84 && rx < SIDEBAR_WIDTH as i32 { Some(b'r') } // refresh
    else { None }
}

/// Returns true if the click hit the context menu (was consumed).
pub fn handle_context_menu_click(ctx: &mut ExplorerContext, rx: i32, ry: i32) -> bool {
    if !ctx.context_menu.open { return false; }
    let mx = ctx.context_menu.x as i32;
    let my = ctx.context_menu.y as i32;
    let mh = (CONTEXT_MENU_ITEMS.len() as u32) * ROW_HEIGHT;
    if rx >= mx && rx < mx + CONTEXT_MENU_W as i32
        && ry >= my && ry < my + mh as i32
    {
        let idx = ((ry - my) as u32) / ROW_HEIGHT;
        if idx < CONTEXT_MENU_ITEMS.len() as u32 {
            let action = context_menu_action(idx as usize);
            ctx.context_menu.open = false;
            ctx.context_menu.hovered_item = None;
            dispatch_context_action(ctx, action);
            return true;
        }
    }
    ctx.context_menu.open = false;
    ctx.context_menu.hovered_item = None;
    true
}

fn dispatch_context_action(ctx: &mut ExplorerContext, action: ExplorerAction) {
    match action {
        ExplorerAction::Open => {
            if let Some(idx) = ctx.selected_index {
                ctx.open_selected(idx);
            }
        }
        ExplorerAction::Copy => {
            // For now, clipboard is not yet implemented
        }
        ExplorerAction::Paste => {}
        ExplorerAction::Rename => {}
        ExplorerAction::Delete => {}
        ExplorerAction::Properties => {}
        ExplorerAction::None => {}
    }
}

// ── Rendering ─────────────────────────────────────────────────

pub fn render_explorer(ctx: &ExplorerContext, surface: &mut Surface) {
    let w = surface.width();
    let h = surface.height();
    if w == 0 || h == 0 { return; }

    // Full background
    surface.fill_rect(0, 0, w, h, EXPLORER_BG);

    // Toolbar / breadcrumb area
    surface.fill_rect(0, 0, w, TOOLBAR_HEIGHT, TOOLBAR_BG);
    // Sidebar background
    surface.fill_rect(0, TOOLBAR_HEIGHT, SIDEBAR_WIDTH, h - TOOLBAR_HEIGHT - STATUSBAR_HEIGHT, SIDEBAR_BG);
    // Status bar
    surface.fill_rect(0, h - STATUSBAR_HEIGHT, w, STATUSBAR_HEIGHT, STATUSBAR_BG);

    draw_toolbar(ctx, surface);
    draw_sidebar(ctx, surface);
    draw_file_list(ctx, surface, w, h);
    draw_statusbar(ctx, surface, w, h);
    draw_context_menu(ctx, surface);
}

fn draw_toolbar(ctx: &ExplorerContext, surface: &mut Surface) {
    let y = 0u32;

    // Back button
    surface.fill_rect(0, y, 24, TOOLBAR_HEIGHT, TOOLBAR_BG);
    draw_text(surface, "\x1B", 6, 4, 0xCCCCCC, TOOLBAR_BG);

    // Forward button
    surface.fill_rect(28, y, 24, TOOLBAR_HEIGHT, TOOLBAR_BG);
    draw_text(surface, "\x1A", 34, 4, 0xCCCCCC, TOOLBAR_BG);

    // Up button
    surface.fill_rect(56, y, 24, TOOLBAR_HEIGHT, TOOLBAR_BG);
    draw_text(surface, "\x18", 62, 4, 0xCCCCCC, TOOLBAR_BG);

    // Refresh button
    surface.fill_rect(84, y, 24, TOOLBAR_HEIGHT, TOOLBAR_BG);
    draw_text(surface, "\x19", 90, 4, 0x888888, TOOLBAR_BG);

    // Breadcrumb path on the right of toolbar
    let path_x = SIDEBAR_WIDTH + 8;
    draw_text(surface, &ctx.current_dir, path_x, 4, TEXT_COLOR, TOOLBAR_BG);
}

fn draw_sidebar(ctx: &ExplorerContext, surface: &mut Surface) {
    for (i, item) in ctx.sidebar_items.iter().enumerate() {
        let y = TOOLBAR_HEIGHT + i as u32 * ROW_HEIGHT;
        if y + ROW_HEIGHT > surface.height() - STATUSBAR_HEIGHT { break; }

        let bg = if ctx.selected_sidebar == Some(i) {
            SIDEBAR_ACTIVE
        } else {
            SIDEBAR_BG
        };
        surface.fill_rect(0, y, SIDEBAR_WIDTH, ROW_HEIGHT, bg);

        // Folder icon
        draw_glyph(surface, b'+', 6, y + 2, FOLDER_COLOR, bg);

        // Label
        let tx = 6 + 8 + 4;
        draw_text(surface, &item.label, tx, y + 2, TEXT_COLOR, bg);
    }
}

fn draw_file_list(ctx: &ExplorerContext, surface: &mut Surface, win_w: u32, win_h: u32) {
    let lx = SIDEBAR_WIDTH;
    let ly = TOOLBAR_HEIGHT;
    let lw = win_w - SIDEBAR_WIDTH;
    let lh = win_h - TOOLBAR_HEIGHT - STATUSBAR_HEIGHT;

    // Header row
    surface.fill_rect(lx, ly, lw, ROW_HEIGHT, HEADER_BG);
    draw_text(surface, "  Name", lx + 4, ly + 2, HEADER_TEXT, HEADER_BG);

    let col2_x = lx + 240;
    draw_text(surface, "Size", col2_x, ly + 2, HEADER_TEXT, HEADER_BG);

    // Divider
    surface.fill_rect(lx, ly + ROW_HEIGHT - 1, lw, 1, DIVIDER_COLOR);

    // File entries
    let content_y = ly + ROW_HEIGHT;
    let visible_rows = (lh - ROW_HEIGHT) / ROW_HEIGHT;

    for row in 0..visible_rows {
        let idx = ctx.scroll_offset + row as usize;
        if idx >= ctx.entries.len() { break; }

        let entry = &ctx.entries[idx];
        let ey = content_y + row * ROW_HEIGHT;

        let is_selected = ctx.selected_index == Some(idx);
        let is_alt = idx % 2 == 0;
        let row_bg = if is_selected {
            ROW_SELECTED
        } else if is_alt {
            ROW_ALT
        } else {
            EXPLORER_BG
        };

        surface.fill_rect(lx, ey, lw, ROW_HEIGHT, row_bg);

        // Icon + Name
        let icon = if entry.is_dir { b'+' } else { b' ' };
        let ic = if entry.is_dir { FOLDER_COLOR } else { FILE_COLOR };
        draw_glyph(surface, icon, lx + 4, ey + 2, ic, row_bg);
        draw_text(surface, &entry.name, lx + 16, ey + 2, TEXT_COLOR, row_bg);

        // Size
        draw_text(surface, &entry.size_str, col2_x, ey + 2, MUTED_COLOR, row_bg);
    }
}

fn draw_statusbar(ctx: &ExplorerContext, surface: &mut Surface, _win_w: u32, win_h: u32) {
    let sy = win_h - STATUSBAR_HEIGHT;
    let text = format!("{} items  |  {}", ctx.entries.len(), ctx.current_dir);
    draw_text(surface, &text, 8, sy + 2, MUTED_COLOR, STATUSBAR_BG);
}

fn draw_context_menu(ctx: &ExplorerContext, surface: &mut Surface) {
    if !ctx.context_menu.open { return; }

    let mx = ctx.context_menu.x;
    let my = ctx.context_menu.y;
    let mh = (CONTEXT_MENU_ITEMS.len() as u32) * ROW_HEIGHT;

    // Menu background
    surface.fill_rect(mx, my, CONTEXT_MENU_W, mh, CMENU_BG);
    // Border
    surface.fill_rect(mx, my, CONTEXT_MENU_W, 1, 0x4A90D9);
    surface.fill_rect(mx, my + mh - 1, CONTEXT_MENU_W, 1, 0x4A90D9);
    surface.fill_rect(mx, my, 1, mh, 0x4A90D9);
    surface.fill_rect(mx + CONTEXT_MENU_W - 1, my, 1, mh, 0x4A90D9);

    for (i, label) in CONTEXT_MENU_ITEMS.iter().enumerate() {
        let iy = my + i as u32 * ROW_HEIGHT;
        let bg = if ctx.context_menu.hovered_item == Some(i) {
            CMENU_HOVER
        } else {
            CMENU_BG
        };
        surface.fill_rect(mx + 1, iy + 1, CONTEXT_MENU_W - 2, ROW_HEIGHT, bg);
        draw_text(surface, label, mx + 6, iy + 2, TEXT_COLOR, bg);
    }
}

// ── Text/glyph drawing helpers ────────────────────────────────

fn draw_text(surface: &mut Surface, text: &str, x: u32, y: u32, fg: u32, bg: u32) {
    let surf_w = surface.width() as usize;
    let surf_h = surface.height() as usize;
    let pixels = surface.pixels_mut();

    for (ci, ch) in text.bytes().enumerate() {
        if ch < 32 || ch > 126 { continue; }
        let dx = (x + ci as u32 * GLYPH_W) as usize;
        let dy = y as usize;
        if dx + GLYPH_W as usize > surf_w || dy + GLYPH_H as usize > surf_h { continue; }

        for gy in 0..GLYPH_H as usize {
            let row_base = (dy + gy) * surf_w;
            let row_slice = &mut pixels[row_base + dx..row_base + dx + GLYPH_W as usize];
            row_slice.fill(bg);
        }
        let gl = lattice::font::glyph_fast(ch);
        for gy in 0..GLYPH_H as usize {
            let row_base = (dy + gy) * surf_w;
            let byte = gl.row_byte(gy as u32);
            for gx in 0..GLYPH_W as usize {
                if byte & (0x80 >> gx) != 0 {
                    pixels[row_base + dx + gx] = fg;
                }
            }
        }
    }
}

fn draw_glyph(surface: &mut Surface, ch: u8, x: u32, y: u32, fg: u32, bg: u32) {
    let surf_w = surface.width() as usize;
    let surf_h = surface.height() as usize;
    let pixels = surface.pixels_mut();
    let dx = x as usize;
    let dy = y as usize;
    if dx + GLYPH_W as usize > surf_w || dy + GLYPH_H as usize > surf_h { return; }

    for gy in 0..GLYPH_H as usize {
        let row_base = (dy + gy) * surf_w;
        let row_slice = &mut pixels[row_base + dx..row_base + dx + GLYPH_W as usize];
        row_slice.fill(bg);
    }
    let gl = lattice::font::glyph_fast(ch);
    for gy in 0..GLYPH_H as usize {
        let row_base = (dy + gy) * surf_w;
        let byte = gl.row_byte(gy as u32);
        for gx in 0..GLYPH_W as usize {
            if byte & (0x80 >> gx) != 0 {
                pixels[row_base + dx + gx] = fg;
            }
        }
    }
}
