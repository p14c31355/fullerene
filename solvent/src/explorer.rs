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

use crate::{RUNTIME_CONTEXT, VfsEntry};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use lattice::surface::Surface;
use lattice::window::WindowId;

// ── File associations ──────────────────────────────────────────

pub struct FileAssociation {
    pub extensions: &'static [&'static str],
    pub app_name: &'static str,
}

pub static FILE_ASSOCIATIONS: &[FileAssociation] = &[
    FileAssociation {
        extensions: &[
            "txt",
            "md",
            "log",
            "toml",
            "rs",
            "c",
            "h",
            "py",
            "js",
            "json",
            "xml",
            "yml",
            "yaml",
            "ini",
            "cfg",
            "conf",
            "sh",
            "bat",
            "env",
            "gitignore",
            "lock",
        ],
        app_name: "Text Editor",
    },
    FileAssociation {
        extensions: &["bmp"],
        app_name: "Image Viewer",
    },
    FileAssociation {
        extensions: &["png"],
        app_name: "Image Viewer",
    },
    FileAssociation {
        extensions: &["jpg", "jpeg"],
        app_name: "Image Viewer",
    },
    FileAssociation {
        extensions: &["wav"],
        app_name: "Music Player",
    },
    FileAssociation {
        extensions: &["mp3"],
        app_name: "Music Player",
    },
    FileAssociation {
        extensions: &["tar", "tgz", "gz", "xz", "zip"],
        app_name: "Archive Manager",
    },
    FileAssociation {
        extensions: &["mp4"],
        app_name: "Movie Player",
    },
    FileAssociation {
        extensions: &["rle"],
        app_name: "RLE Player",
    },
];

/// Extract the extension from a filename (case-preserved).
pub fn extension_of(name: &str) -> &str {
    if let Some(dot) = name.rfind('.') {
        &name[dot + 1..]
    } else {
        ""
    }
}

/// Look up the app name for a given extension (case-insensitive).
pub fn lookup_association(ext: &str) -> Option<&'static str> {
    for assoc in FILE_ASSOCIATIONS {
        for e in assoc.extensions {
            if ext.eq_ignore_ascii_case(e) {
                return Some(assoc.app_name);
            }
        }
    }
    None
}

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
pub const SECTION_TEXT: u32 = 0x6A6A8A;
pub const USB_DRIVE_COLOR: u32 = 0x4A90D9;

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
    pub is_usb: bool,
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

#[derive(Debug, Clone)]
struct ClipboardEntry {
    path: String,
    name: String,
    is_dir: bool,
}

#[derive(Debug, Clone)]
enum PendingOperation {
    Rename {
        source: String,
        is_dir: bool,
        input: String,
    },
    Delete {
        path: String,
        is_dir: bool,
    },
}

pub(crate) struct PendingCopy {
    pub(crate) source: String,
    pub(crate) destination: String,
    pub(crate) is_dir: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct PendingNavigation(String);

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
    clipboard: Option<ClipboardEntry>,
    pending_operation: Option<PendingOperation>,
    pending_copy: Option<PendingCopy>,
    status_message: Option<String>,
    rename_shift_held: bool,
    pending_navigation: Option<PendingNavigation>,
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
        Self {
            open: false,
            x: 0,
            y: 0,
            hovered_item: None,
        }
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
            clipboard: None,
            pending_operation: None,
            pending_copy: None,
            status_message: None,
            rename_shift_held: false,
            pending_navigation: None,
        }
    }

    /// Standard sidebar entries (Home, Desktop, etc.)
    const fn default_sidebar_entries() -> &'static [(&'static str, &'static str)] {
        &[
            ("Home", "/"),
            ("Desktop", "/Desktop"),
            ("Downloads", "/Downloads"),
            ("Documents", "/Documents"),
            ("Music", "/Music"),
            ("Pictures", "/Pictures"),
        ]
    }

    fn entries_from_static(entries: &[(&str, &str)]) -> Vec<SidebarItem> {
        entries
            .iter()
            .map(|&(label, path)| SidebarItem {
                label: String::from(label),
                path: String::from(path),
                is_usb: false,
            })
            .collect()
    }

    fn default_sidebar() -> Vec<SidebarItem> {
        Self::entries_from_static(Self::default_sidebar_entries())
    }

    pub fn navigate_to(&mut self, path: &str) {
        self.pending_navigation = Some(PendingNavigation(String::from(path)));
        self.status_message = Some(String::from("Loading..."));
    }

    pub(crate) fn take_navigation_request(&mut self) -> Option<String> {
        Some(self.pending_navigation.take()?.0)
    }

    pub(crate) fn take_pending_copy(&mut self) -> Option<PendingCopy> {
        self.pending_copy.take()
    }

    pub(crate) fn finish_paste(&mut self, destination: &str, result: Result<(), genome::FsError>) {
        self.status_message = Some(match result {
            Ok(()) => {
                self.refresh();
                format!("Pasted {}", basename(destination))
            }
            Err(error) => format!("Paste failed: {}", error),
        });
    }

    pub(crate) fn finish_navigation(
        &mut self,
        path: String,
        result: Result<Vec<VfsEntry>, genome::FsError>,
    ) {
        let entries = match result {
            Ok(entries) => entries,
            Err(error) => {
                self.status_message = Some(format!("Open failed: {}", error));
                return;
            }
        };
        self.current_dir = path.clone();
        self.raw_names = entries.iter().map(|entry| entry.name.clone()).collect();
        self.raw_is_dir = entries.iter().map(|entry| entry.is_dir).collect();
        self.entries = entries
            .into_iter()
            .map(|entry| FileEntryDisplay {
                size_str: if entry.is_dir {
                    String::from("<DIR>")
                } else {
                    format_size(entry.size)
                },
                name: entry.name,
                is_dir: entry.is_dir,
            })
            .collect();
        self.sort_entries();
        self.selected_index = None;
        self.scroll_offset = 0;
        if self.status_message.as_deref() == Some("Loading...") {
            self.status_message = None;
        }
        if self.history_pos >= self.history.len() || self.history[self.history_pos] != path {
            self.history.truncate(self.history_pos + 1);
            self.history.push(path);
            self.history_pos = self.history.len() - 1;
        }
    }

    pub fn refresh(&mut self) {
        let dir = self.current_dir.clone();
        self.navigate_to(&dir);
        self.refresh_sidebar();
    }

    /// Refresh sidebar items (re-detect USB drives etc.).
    pub fn refresh_sidebar(&mut self) {
        let mut items = Self::entries_from_static(Self::default_sidebar_entries());
        for (name, mount_path) in crate::get_mounted_drives() {
            items.push(SidebarItem {
                label: name,
                path: mount_path,
                is_usb: true,
            });
        }
        self.sidebar_items = items;
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
        let sorted_names: Vec<String> =
            indices.iter().map(|&i| self.raw_names[i].clone()).collect();
        let sorted_is_dir: Vec<bool> = indices.iter().map(|&i| self.raw_is_dir[i]).collect();
        self.entries = sorted_entries;
        self.raw_names = sorted_names;
        self.raw_is_dir = sorted_is_dir;
    }

    pub fn go_back(&mut self) {
        if self.history_pos > 0 {
            self.history_pos -= 1;
            let path = self.history[self.history_pos].clone();
            self.navigate_to(&path);
        }
    }

    pub fn go_forward(&mut self) {
        if self.history_pos + 1 < self.history.len() {
            self.history_pos += 1;
            let path = self.history[self.history_pos].clone();
            self.navigate_to(&path);
        }
    }

    pub fn go_up(&mut self) {
        let parent = parent_path(&self.current_dir);
        self.navigate_to(&parent);
    }

    /// Queue a directory navigation or return a file path for launching.
    pub fn activate_entry(&mut self, idx: usize) -> Option<String> {
        let path = join_path(&self.current_dir, self.raw_names.get(idx)?);
        if *self.raw_is_dir.get(idx)? {
            self.navigate_to(&path);
            None
        } else {
            Some(path)
        }
    }

    fn selected_entry(&self) -> Option<(String, String, bool)> {
        let index = self.selected_index?;
        let name = self.raw_names.get(index)?.clone();
        Some((
            join_path(&self.current_dir, &name),
            name,
            *self.raw_is_dir.get(index)?,
        ))
    }

    /// Execute an Explorer context-menu command. A returned path is a file
    /// that the runtime should launch after releasing the Explorer borrow.
    pub fn dispatch_context_action(&mut self, action: ExplorerAction) -> Option<String> {
        self.status_message = None;
        match action {
            ExplorerAction::Open => {
                let (path, _, is_dir) = self.selected_entry()?;
                if is_dir {
                    self.navigate_to(&path);
                    None
                } else {
                    Some(path)
                }
            }
            ExplorerAction::Copy => {
                if let Some((path, name, is_dir)) = self.selected_entry() {
                    self.clipboard = Some(ClipboardEntry {
                        path,
                        name: name.clone(),
                        is_dir,
                    });
                    self.status_message = Some(format!("Copied {}", name));
                } else {
                    self.status_message = Some(String::from("Copy: select a file or folder"));
                }
                None
            }
            ExplorerAction::Paste => {
                let Some(source) = self.clipboard.clone() else {
                    self.status_message = Some(String::from("Paste: clipboard is empty"));
                    return None;
                };
                let destination = unique_destination(&self.current_dir, &source.name);
                self.pending_copy = Some(PendingCopy {
                    source: source.path,
                    destination,
                    is_dir: source.is_dir,
                });
                self.status_message = Some(String::from("Pasting..."));
                None
            }
            ExplorerAction::Rename => {
                if let Some((source, name, is_dir)) = self.selected_entry() {
                    self.pending_operation = Some(PendingOperation::Rename {
                        source,
                        is_dir,
                        input: name,
                    });
                } else {
                    self.status_message = Some(String::from("Rename: select a file or folder"));
                }
                None
            }
            ExplorerAction::Delete => {
                if let Some((path, _, is_dir)) = self.selected_entry() {
                    self.pending_operation = Some(PendingOperation::Delete { path, is_dir });
                } else {
                    self.status_message = Some(String::from("Delete: select a file or folder"));
                }
                None
            }
            ExplorerAction::Properties => {
                if let Some((path, name, is_dir)) = self.selected_entry() {
                    let size = self
                        .selected_index
                        .and_then(|index| self.entries.get(index))
                        .map(|entry| entry.size_str.as_str())
                        .unwrap_or("unknown");
                    self.status_message = Some(format!(
                        "{} | {} | {} | {}",
                        name,
                        if is_dir { "folder" } else { "file" },
                        size,
                        path
                    ));
                } else {
                    self.status_message = Some(format!("Folder | {}", self.current_dir));
                }
                None
            }
            ExplorerAction::None => None,
        }
    }

    /// Handle rename/delete modal keys before normal Explorer navigation.
    pub fn handle_operation_key(&mut self, scancode: u8, pressed: bool) -> bool {
        if self.pending_operation.is_none() {
            return false;
        }
        if matches!(scancode, 0x2A | 0x36) {
            self.rename_shift_held = pressed;
            return true;
        }
        if !pressed {
            return true;
        }
        match scancode {
            0x01 => {
                self.pending_operation = None;
                self.status_message = Some(String::from("Operation cancelled"));
            }
            0x1C => self.commit_pending_operation(),
            0x0E => {
                if let Some(PendingOperation::Rename { input, .. }) =
                    self.pending_operation.as_mut()
                {
                    input.pop();
                }
            }
            _ => {
                let mut byte = crate::scancode_to_ascii(scancode);
                if self.rename_shift_held {
                    byte = shifted_ascii(byte);
                }
                if byte != 0
                    && byte != b'/'
                    && byte != b'\\'
                    && let Some(PendingOperation::Rename { input, .. }) =
                        self.pending_operation.as_mut()
                    && input.len() < 255
                {
                    input.push(byte as char);
                }
            }
        }
        true
    }

    fn commit_pending_operation(&mut self) {
        let Some(operation) = self.pending_operation.take() else {
            return;
        };
        self.status_message = Some(match operation {
            PendingOperation::Delete { path, is_dir } => match delete_entry(&path, is_dir) {
                Ok(()) => {
                    self.refresh();
                    format!("Deleted {}", basename(&path))
                }
                Err(error) => format!("Delete failed: {}", error),
            },
            PendingOperation::Rename {
                source,
                is_dir,
                input,
            } => {
                let name = input.trim();
                if name.is_empty() || matches!(name, "." | "..") {
                    String::from("Rename failed: invalid name")
                } else {
                    let destination = join_path(&parent_path(&source), name);
                    if destination == source {
                        String::from("Rename: name unchanged")
                    } else if path_exists(&destination) {
                        String::from("Rename failed: destination already exists")
                    } else {
                        match move_entry(&source, &destination, is_dir) {
                            Ok(()) => {
                                self.refresh();
                                format!("Renamed to {}", name)
                            }
                            Err(error) => format!("Rename failed: {}", error),
                        }
                    }
                }
            }
        });
    }
}

// ── Path helpers ───────────────────────────────────────────────

fn parent_path(path: &str) -> String {
    if path == "/" {
        return String::from("/");
    }
    let trimmed = path.trim_end_matches('/');
    if let Some(pos) = trimmed.rfind('/') {
        if pos == 0 {
            String::from("/")
        } else {
            String::from(&trimmed[..pos])
        }
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

fn basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

fn path_exists(path: &str) -> bool {
    let parent = parent_path(path);
    let name = basename(path);
    let Some(readdir) = RUNTIME_CONTEXT.callback_snapshot().vfs_readdir else {
        return false;
    };
    readdir(&parent).ok().is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry.name.eq_ignore_ascii_case(name))
    })
}

fn unique_destination(directory: &str, name: &str) -> String {
    let original = join_path(directory, name);
    if !path_exists(&original) {
        return original;
    }
    let (stem, extension) = name
        .rfind('.')
        .filter(|&index| index > 0)
        .map_or((name, ""), |index| (&name[..index], &name[index..]));
    (1..10_000)
        .map(|index| {
            join_path(
                directory,
                &format!("{} - Copy {}{}", stem, index, extension),
            )
        })
        .find(|candidate| !path_exists(candidate))
        .unwrap_or_else(|| join_path(directory, &format!("{} - Copy{}", stem, extension)))
}

fn move_entry(source: &str, destination: &str, is_dir: bool) -> Result<(), genome::FsError> {
    let move_path = RUNTIME_CONTEXT
        .callback_snapshot()
        .vfs_move
        .ok_or(genome::FsError::NotSupported)?;
    move_path(source, destination, is_dir)
}

fn delete_entry(path: &str, is_dir: bool) -> Result<(), genome::FsError> {
    let remove = RUNTIME_CONTEXT
        .callback_snapshot()
        .vfs_remove
        .ok_or(genome::FsError::NotSupported)?;
    remove(path, is_dir)
}

pub(crate) fn shifted_ascii(byte: u8) -> u8 {
    match byte {
        b'a'..=b'z' => byte.to_ascii_uppercase(),
        b'1' => b'!',
        b'2' => b'@',
        b'3' => b'#',
        b'4' => b'$',
        b'5' => b'%',
        b'6' => b'^',
        b'7' => b'&',
        b'8' => b'*',
        b'9' => b'(',
        b'0' => b')',
        b'-' => b'_',
        b'=' => b'+',
        b'[' => b'{',
        b']' => b'}',
        b';' => b':',
        b'\'' => b'"',
        b',' => b'<',
        b'.' => b'>',
        b'/' => b'?',
        b'`' => b'~',
        other => other,
    }
}

fn format_size(size: u64) -> String {
    if size >= 1048576 {
        format!(
            "{}.{} MB",
            size / 1048576,
            ((size % 1048576) * 10) / 1048576
        )
    } else if size >= 1024 {
        format!("{}.{} KB", size / 1024, (size % 1024) * 10 / 1024)
    } else {
        format!("{} B", size)
    }
}

fn parse_size_for_sort(s: &str) -> u64 {
    if s == "<DIR>" {
        return 0;
    }
    let mut num = 0u64;
    for c in s.bytes() {
        if c >= b'0' && c <= b'9' {
            num = num * 10 + (c - b'0') as u64;
        }
    }
    if s.ends_with("KB") {
        num * 1024
    } else if s.ends_with("MB") {
        num * 1048576
    } else {
        num
    }
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
    if ry < content_y {
        return None;
    }
    let row = ((ry - content_y) as u32) / ROW_HEIGHT;
    let idx = ctx.scroll_offset + row as usize;
    if idx < ctx.entries.len() {
        Some(idx)
    } else {
        None
    }
}

pub fn hit_file_area(win_w: u32, win_h: u32, rx: i32, ry: i32) -> bool {
    rx >= SIDEBAR_WIDTH as i32
        && rx < win_w as i32
        && ry >= (TOOLBAR_HEIGHT + ROW_HEIGHT) as i32
        && ry < win_h.saturating_sub(STATUSBAR_HEIGHT) as i32
}

pub fn hit_sidebar(ctx: &ExplorerContext, rx: i32, ry: i32) -> Option<usize> {
    if rx < 0 || rx >= SIDEBAR_WIDTH as i32 || ry < TOOLBAR_HEIGHT as i32 {
        return None;
    }
    let rel = (ry - TOOLBAR_HEIGHT as i32) as u32;
    let idx = rel / ROW_HEIGHT;
    if idx < ctx.sidebar_items.len() as u32 {
        Some(idx as usize)
    } else {
        None
    }
}

pub fn hit_toolbar_button(rx: i32, ry: i32) -> Option<u8> {
    if ry < 0 || ry >= TOOLBAR_HEIGHT as i32 {
        return None;
    }
    if rx >= 0 && rx < 28 {
        Some(b'b')
    }
    // back
    else if rx >= 28 && rx < 56 {
        Some(b'f')
    }
    // forward
    else if rx >= 56 && rx < 84 {
        Some(b'u')
    }
    // up
    else if rx >= 84 && rx < SIDEBAR_WIDTH as i32 {
        Some(b'r')
    }
    // refresh
    else {
        None
    }
}

/// Return the selected action, or `None` when the menu was dismissed.
pub fn handle_context_menu_click(
    ctx: &mut ExplorerContext,
    rx: i32,
    ry: i32,
) -> Option<ExplorerAction> {
    if !ctx.context_menu.open {
        return None;
    }
    let mx = ctx.context_menu.x as i32;
    let my = ctx.context_menu.y as i32;
    let mh = (CONTEXT_MENU_ITEMS.len() as u32) * ROW_HEIGHT;
    if rx >= mx && rx < mx + CONTEXT_MENU_W as i32 && ry >= my && ry < my + mh as i32 {
        let idx = ((ry - my) as u32) / ROW_HEIGHT;
        if idx < CONTEXT_MENU_ITEMS.len() as u32 {
            let action = context_menu_action(idx as usize);
            ctx.context_menu.open = false;
            ctx.context_menu.hovered_item = None;
            return Some(action);
        }
    }
    ctx.context_menu.open = false;
    ctx.context_menu.hovered_item = None;
    None
}

// ── Rendering ─────────────────────────────────────────────────

pub fn render_explorer(ctx: &ExplorerContext, surface: &mut Surface) {
    let w = surface.width();
    let h = surface.height();
    if w == 0 || h == 0 {
        return;
    }

    // Full background
    surface.fill_rect(0, 0, w, h, EXPLORER_BG);

    // Toolbar / breadcrumb area
    surface.fill_rect(0, 0, w, TOOLBAR_HEIGHT, TOOLBAR_BG);
    // Sidebar background
    surface.fill_rect(
        0,
        TOOLBAR_HEIGHT,
        SIDEBAR_WIDTH,
        h - TOOLBAR_HEIGHT - STATUSBAR_HEIGHT,
        SIDEBAR_BG,
    );
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
    let mut row = 0u32;
    // Favorites first
    for item in ctx.sidebar_items.iter() {
        if item.is_usb {
            break;
        } // stop at first USB entry
        let y = TOOLBAR_HEIGHT + row * ROW_HEIGHT;
        if y + ROW_HEIGHT > surface.height() - STATUSBAR_HEIGHT {
            break;
        }

        let bg = if ctx
            .sidebar_items
            .iter()
            .position(|x| x.label == item.label && x.path == item.path)
            .map(|idx| ctx.selected_sidebar == Some(idx))
            .unwrap_or(false)
        {
            SIDEBAR_ACTIVE
        } else {
            SIDEBAR_BG
        };
        surface.fill_rect(0, y, SIDEBAR_WIDTH, ROW_HEIGHT, bg);
        draw_glyph(surface, b'+', 6, y + 2, FOLDER_COLOR, bg);
        draw_text(surface, &item.label, 18, y + 2, TEXT_COLOR, bg);
        row += 1;
    }

    // Devices section header
    let has_usb = ctx.sidebar_items.iter().any(|x| x.is_usb);
    if has_usb {
        let y = TOOLBAR_HEIGHT + row * ROW_HEIGHT;
        if y + ROW_HEIGHT <= surface.height() - STATUSBAR_HEIGHT {
            surface.fill_rect(0, y, SIDEBAR_WIDTH, ROW_HEIGHT, SIDEBAR_BG);
            draw_text(surface, "── Devices ──", 6, y + 2, SECTION_TEXT, SIDEBAR_BG);
            row += 1;
        }
    }

    // USB drives
    for item in ctx.sidebar_items.iter() {
        if !item.is_usb {
            continue;
        }
        let y = TOOLBAR_HEIGHT + row * ROW_HEIGHT;
        if y + ROW_HEIGHT > surface.height() - STATUSBAR_HEIGHT {
            break;
        }

        let bg = if ctx
            .sidebar_items
            .iter()
            .position(|x| x.label == item.label && x.path == item.path)
            .map(|idx| ctx.selected_sidebar == Some(idx))
            .unwrap_or(false)
        {
            SIDEBAR_ACTIVE
        } else {
            SIDEBAR_BG
        };
        surface.fill_rect(0, y, SIDEBAR_WIDTH, ROW_HEIGHT, bg);
        // USB drive icon
        draw_glyph(surface, b'U', 6, y + 2, USB_DRIVE_COLOR, bg);
        draw_text(surface, &item.label, 18, y + 2, USB_DRIVE_COLOR, bg);
        row += 1;
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
        if idx >= ctx.entries.len() {
            break;
        }

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
        let ic = if entry.is_dir {
            FOLDER_COLOR
        } else {
            FILE_COLOR
        };
        draw_glyph(surface, icon, lx + 4, ey + 2, ic, row_bg);
        draw_text(surface, &entry.name, lx + 16, ey + 2, TEXT_COLOR, row_bg);

        // Size
        draw_text(
            surface,
            &entry.size_str,
            col2_x,
            ey + 2,
            MUTED_COLOR,
            row_bg,
        );
    }
}

fn draw_statusbar(ctx: &ExplorerContext, surface: &mut Surface, _win_w: u32, win_h: u32) {
    let sy = win_h - STATUSBAR_HEIGHT;
    let text = if let Some(operation) = &ctx.pending_operation {
        match operation {
            PendingOperation::Rename { input, .. } => {
                format!("Rename: {}_  |  Enter: apply  Esc: cancel", input)
            }
            PendingOperation::Delete { path, .. } => {
                format!("Delete {}?  |  Enter: yes  Esc: no", basename(path))
            }
        }
    } else if let Some(message) = &ctx.status_message {
        message.clone()
    } else if ctx.entries.is_empty() {
        format!("(empty directory)  |  {}", ctx.current_dir)
    } else {
        format!("{} items  |  {}", ctx.entries.len(), ctx.current_dir)
    };
    draw_text(surface, &text, 8, sy + 2, MUTED_COLOR, STATUSBAR_BG);
}

fn draw_context_menu(ctx: &ExplorerContext, surface: &mut Surface) {
    if !ctx.context_menu.open {
        return;
    }

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

fn draw_glyph(surface: &mut Surface, ch: u8, x: u32, y: u32, fg: u32, bg: u32) {
    let (sw, sh) = (surface.width() as usize, surface.height() as usize);
    let (dx, dy) = (x as usize, y as usize);
    if dx + GLYPH_W as usize > sw || dy + GLYPH_H as usize > sh {
        return;
    }
    let pixels = surface.pixels_mut();
    for gy in 0..GLYPH_H as usize {
        pixels[dy + gy..][..sw][dx..dx + GLYPH_W as usize].fill(bg);
    }
    let gl = lattice::font::glyph_fast(ch);
    for gy in 0..GLYPH_H as usize {
        let row = (dy + gy) * sw;
        let byte = gl.row_byte(gy as u32);
        for gx in 0..GLYPH_W as usize {
            if byte & (0x80 >> gx) != 0 {
                pixels[row + dx + gx] = fg;
            }
        }
    }
}

fn draw_text(surface: &mut Surface, text: &str, x: u32, y: u32, fg: u32, bg: u32) {
    for (ci, ch) in text
        .bytes()
        .enumerate()
        .filter(|(_, c)| (32..=126).contains(c))
    {
        draw_glyph(surface, ch, x + ci as u32 * GLYPH_W, y, fg, bg);
    }
}

#[cfg(test)]
mod tests {
    use super::ExplorerContext;
    use crate::VfsEntry;
    use alloc::string::String;

    #[test]
    fn navigation_completes_in_one_step() {
        let mut explorer = ExplorerContext::new();
        explorer.navigate_to("/mnt/sdcard");

        assert_eq!(explorer.current_dir, "/");
        assert_eq!(
            explorer.take_navigation_request().as_deref(),
            Some("/mnt/sdcard")
        );
        assert_eq!(explorer.take_navigation_request(), None);

        explorer.finish_navigation(
            String::from("/mnt/sdcard"),
            Ok(alloc::vec![VfsEntry {
                name: String::from("Bootlog.txt"),
                size: 512,
                is_dir: false,
            }]),
        );
        assert_eq!(explorer.current_dir, "/mnt/sdcard");
        assert_eq!(explorer.entries[0].name, "Bootlog.txt");
    }

    #[test]
    fn activating_entries_uses_one_path_for_keyboard_and_mouse() {
        let mut explorer = ExplorerContext::new();
        explorer.current_dir = String::from("/mnt");
        explorer.raw_names = alloc::vec![String::from("sdcard"), String::from("Bootlog.txt")];
        explorer.raw_is_dir = alloc::vec![true, false];

        assert_eq!(explorer.activate_entry(0), None);
        assert_eq!(
            explorer.take_navigation_request().as_deref(),
            Some("/mnt/sdcard")
        );
        assert_eq!(
            explorer.activate_entry(1).as_deref(),
            Some("/mnt/Bootlog.txt")
        );
    }
}
