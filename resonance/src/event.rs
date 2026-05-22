// ---------------------------------------------------------------------------
// Target – event routing target
// ---------------------------------------------------------------------------

/// Identifies the target subsystem or window for an event.
///
/// This allows the dispatcher or handlers to route events to the correct
/// recipient without each handler inspecting every event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// Route to the currently focused window.
    FocusedWindow,
    /// Route to a specific window by ID.
    Window(u64),
    /// Route to the shell / terminal subsystem.
    Shell,
    /// Route to the window manager / compositor.
    WindowManager,
    /// Route to the system (power, configuration).
    System,
    /// No specific target — broadcast to all handlers.
    None,
}

// ---------------------------------------------------------------------------
// EventEnvelope – event with routing metadata
// ---------------------------------------------------------------------------

/// An event wrapped with routing information.
///
/// `target` allows the dispatcher or handlers to route events efficiently
/// to the correct subsystem without broadcasting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventEnvelope {
    pub target: Target,
    pub event: Event,
}

impl EventEnvelope {
    pub fn new(target: Target, event: Event) -> Self {
        Self { target, event }
    }

    /// Create a broadcast event (no specific target).
    pub fn broadcast(event: Event) -> Self {
        Self {
            target: Target::None,
            event,
        }
    }
}

// ---------------------------------------------------------------------------
// Event – top-level event enum
// ---------------------------------------------------------------------------

/// Top-level event type.
///
/// All events flowing through the Resonance event system are one of these
/// variants. Events are **immutable** — they are created, queued, consumed,
/// and dropped without mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Input(InputEvent),
    Window(WindowEvent),
    Timer(TimerEvent),
    System(SystemEvent),
}

// ---------------------------------------------------------------------------
// InputEvent
// ---------------------------------------------------------------------------

/// Mouse button identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Other(u8),
}

/// Keyboard key code (limited to common keys for v0).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCode {
    // Alphanumeric
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,

    // Modifiers
    Shift,
    Ctrl,
    Alt,
    Meta,

    // Navigation
    Enter,
    Tab,
    Space,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,

    // Function keys
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,

    /// Catch-all for unhandled keys.
    Unknown(u32),
}

/// Input events (mouse, keyboard, etc.).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    MouseMove { x: i32, y: i32 },
    MouseDown(MouseButton),
    MouseUp(MouseButton),
    KeyDown(KeyCode),
    KeyUp(KeyCode),
}

// ---------------------------------------------------------------------------
// WindowEvent
// ---------------------------------------------------------------------------

/// Window-level events (for the compositor / WM).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowEvent {
    Created(u64),
    Closed(u64),
    Moved { id: u64, x: i32, y: i32 },
    Resized { id: u64, width: u32, height: u32 },
    Focused(u64),
    Unfocused(u64),
    Redraw(u64),
}

// ---------------------------------------------------------------------------
// TimerEvent
// ---------------------------------------------------------------------------

/// Timer expiry events — bridges `ChronoLine` into the event system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerEvent {
    pub id: u64,
    pub deadline_ticks: u64,
}

// ---------------------------------------------------------------------------
// SystemEvent
// ---------------------------------------------------------------------------

/// System-level events (power, configuration, etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemEvent {
    Shutdown,
    Reboot,
    Panic,
    Suspend,
    Resume,
}
