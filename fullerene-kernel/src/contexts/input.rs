//! InputContext — unified keyboard+mouse event queue.
use alloc::collections::VecDeque;
use alloc::vec::Vec;
pub use resonance::{InputEvent, KeyCode, MouseButton};

pub struct InputContext {
    pub queue: VecDeque<InputEvent>,
    pub mouse_x: i16,
    pub mouse_y: i16,
    pub mouse_buttons: u8,
    sensitivity: i16,
}

impl InputContext {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            mouse_x: 512,
            mouse_y: 384,
            mouse_buttons: 0,
            sensitivity: 6,
        }
    }
    pub fn set_sensitivity(&mut self, s: i16) {
        self.sensitivity = s;
    }
    /// Read sensitivity from SettingsContext if available, otherwise use the
    /// locally-configured value.
    pub fn apply_settings_sensitivity(&mut self) {
        if let Some(val) = super::kernel::with_kernel(|k| k.settings.mouse.sensitivity_raw()) {
            self.sensitivity = val;
        }
    }
    pub fn drain_events(&mut self) -> Vec<InputEvent> {
        self.queue.drain(..).collect()
    }
    pub fn has_events(&self) -> bool {
        !self.queue.is_empty()
    }
}

pub fn drain_into_event_context() {
    let has_event_ctx = super::event::with_event_mut(|_| ()).is_some();
    if !has_event_ctx {
        return;
    }
    let events = with_input_mut(|ctx| ctx.drain_events());
    let Some(events) = events else { return };
    super::event::with_event_mut(|ec| {
        for ev in events {
            ec.push(resonance::Event::Input(ev));
        }
    });
}

crate::define_context!(InputContext, input, INPUT_CTX);
