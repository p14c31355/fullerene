//! xHCI event-ring consumption.

use super::context::XhciContext;
use super::interrupt::wait_event_type;
use super::ring::{Trb, trb_type};

impl XhciContext {
    /// Wait for a transfer event with a timeout in microseconds.
    pub fn wait_event(&mut self, timeout: u32) -> Result<Trb, crate::DriverError> {
        wait_event_type(
            &mut self.rings.event,
            &self.registers.runtime,
            timeout,
            trb_type::TRANSFER_EVENT,
        )
    }
}
