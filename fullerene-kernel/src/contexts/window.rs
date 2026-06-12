//! WindowContext — window list, focus, z-order.
use alloc::vec::Vec;
use spin::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);
impl WindowId {
    pub const INVALID: Self = Self(0);
}

#[derive(Debug, Clone)]
pub struct Window {
    pub id: WindowId,
    pub title: alloc::string::String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
    pub z: i32,
}
impl Window {
    pub fn new(id: WindowId, title: &str, x: i32, y: i32, w: u32, h: u32) -> Self {
        Self {
            id,
            title: alloc::string::String::from(title),
            x,
            y,
            width: w,
            height: h,
            visible: true,
            z: 0,
        }
    }
}

pub struct WindowContext {
    pub windows: Vec<Window>,
    pub focused: WindowId,
    pub cursor_visible: bool,
    pub cursor_x: i32,
    pub cursor_y: i32,
    next_id: u64,
}

impl WindowContext {
    pub fn new() -> Self {
        Self {
            windows: Vec::new(),
            focused: WindowId::INVALID,
            cursor_visible: true,
            cursor_x: 512,
            cursor_y: 384,
            next_id: 1,
        }
    }
    pub fn next_window_id(&mut self) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id += 1;
        id
    }
    pub fn add_window(&mut self, mut win: Window) {
        let z = self.windows.iter().map(|w| w.z).max().unwrap_or(0);
        win.z = z + 1;
        self.windows.push(win);
    }
    pub fn remove_window(&mut self, id: WindowId) {
        self.windows.retain(|w| w.id != id);
        if self.focused == id {
            self.focused = WindowId::INVALID;
        }
    }
    pub fn focus(&mut self, id: WindowId) {
        if self.focused == id {
            return;
        }
        let exists = self.windows.iter().any(|w| w.id == id);
        if !exists {
            return;
        }
        self.focused = id;
        let z = self.windows.iter().map(|w| w.z).max().unwrap_or(0);
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.z = z + 1;
        }
    }
    pub fn window_at(&self, x: i32, y: i32) -> Option<&Window> {
        self.windows
            .iter()
            .filter(|w| {
                w.visible
                    && x >= w.x
                    && x < w.x + w.width as i32
                    && y >= w.y
                    && y < w.y + w.height as i32
            })
            .max_by_key(|w| w.z)
    }
}

static WINDOW_CTX: Mutex<Option<WindowContext>> = Mutex::new(None);
pub fn init_window() {
    *WINDOW_CTX.lock() = Some(WindowContext::new());
}
pub fn get_window() -> &'static Mutex<Option<WindowContext>> {
    &WINDOW_CTX
}
pub fn with_window_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut WindowContext) -> R,
{
    WINDOW_CTX.lock().as_mut().map(f)
}
pub fn with_window<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&WindowContext) -> R,
{
    WINDOW_CTX.lock().as_ref().map(f)
}
