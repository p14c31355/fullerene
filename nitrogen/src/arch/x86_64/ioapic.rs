//! I/O APIC Redirection Table Entry — pure data structure
//!
//! The `IoApicRedirectionEntry` struct represents a single entry in the
//! I/O APIC redirection table.  Actual I/O APIC register access is now
//! handled by [`crate::apic_controller::ApicController`].

/// I/O APIC Redirection Table Entry (RTE) structure
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IoApicRedirectionEntry {
    pub lower: u32,
    pub upper: u32,
}

impl IoApicRedirectionEntry {
    /// Create a new RTE with specified parameters
    pub fn new(
        vector: u8,
        delivery_mode: u8,
        dest_mode: bool,
        polarity: bool,
        trigger: bool,
        mask: bool,
        dest: u8,
    ) -> Self {
        let lower = (vector as u32)
            | ((delivery_mode as u32) << 8)
            | ((dest_mode as u32) << 11)
            | ((polarity as u32) << 13)
            | ((trigger as u32) << 15)
            | ((mask as u32) << 16);

        let upper = (dest as u32) << 24;

        Self { lower, upper }
    }

    /// Set the vector
    pub fn set_vector(&mut self, vector: u8) {
        self.lower = (self.lower & !0xFF) | vector as u32;
    }

    /// Set delivery mode
    pub fn set_delivery_mode(&mut self, mode: u8) {
        self.lower = (self.lower & !(0x7 << 8)) | ((mode as u32) << 8);
    }

    /// Set destination mode (0 = physical, 1 = logical)
    pub fn set_dest_mode(&mut self, logical: bool) {
        if logical {
            self.lower |= 1 << 11;
        } else {
            self.lower &= !(1 << 11);
        }
    }

    /// Set polarity (0 = high active, 1 = low active)
    pub fn set_polarity(&mut self, low_active: bool) {
        if low_active {
            self.lower |= 1 << 13;
        } else {
            self.lower &= !(1 << 13);
        }
    }

    /// Set trigger mode (0 = edge, 1 = level)
    pub fn set_trigger_mode(&mut self, level: bool) {
        if level {
            self.lower |= 1 << 15;
        } else {
            self.lower &= !(1 << 15);
        }
    }

    /// Set mask (0 = unmasked, 1 = masked)
    pub fn set_mask(&mut self, masked: bool) {
        if masked {
            self.lower |= 1 << 16;
        } else {
            self.lower &= !(1 << 16);
        }
    }

    /// Set destination
    pub fn set_destination(&mut self, dest: u8) {
        self.upper = (self.upper & !(0xFF << 24)) | ((dest as u32) << 24);
    }
}
