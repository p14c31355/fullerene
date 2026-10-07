//! VGA text buffer's hardware-initializer trait impls.
//!
//! These are x86-only: the `initializer` traits describe the PC boot device
//! bring-up sequence, and the ANE never runs it. Moving them here keeps the
//! shared `graphics/text.rs` free of cfg branches.

use crate::graphics::text::VgaBuffer;

impl crate::initializer::Initializable for VgaBuffer {
    fn init(&mut self) -> crate::common::logging::SystemResult<()> {
        log::info!("VGA device initialized");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "VgaBuffer"
    }

    fn priority(&self) -> i32 {
        10 // High priority for display device
    }
}

impl crate::initializer::ErrorLogging for VgaBuffer {
    fn log_error(&self, error: &crate::common::logging::SystemError, context: &'static str) {
        log::error!("{}: {:?}", context, error);
    }

    fn log_warning(&self, message: &'static str) {
        log::warn!("{}", message);
    }

    fn log_info(&self, message: &'static str) {
        log::info!("{}", message);
    }

    fn log_debug(&self, message: &'static str) {
        log::debug!("{}", message);
    }

    fn log_trace(&self, message: &'static str) {
        log::trace!("{}", message);
    }
}

impl crate::initializer::HardwareDevice for VgaBuffer {
    fn device_name(&self) -> &'static str {
        "VGA Text Mode Display"
    }

    fn device_type(&self) -> &'static str {
        "Display"
    }

    fn enable(&mut self) -> crate::common::logging::SystemResult<()> {
        self.enabled = true;
        log::info!("VGA device enabled");
        Ok(())
    }

    fn disable(&mut self) -> crate::common::logging::SystemResult<()> {
        self.enabled = false;
        log::info!("VGA device disabled");
        Ok(())
    }

    fn reset(&mut self) -> crate::common::logging::SystemResult<()> {
        self.reset();
        log::info!("VGA device reset");
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }
}
