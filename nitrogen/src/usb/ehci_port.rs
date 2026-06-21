//! EHCI Port Management — port status, reset, and device detection.
//!
//! Manages all EHCI root hub ports.  Port state is tracked per-port
//! to avoid re-enumerating the same device across poll cycles.

// ============================================================================
//  Port — data for a single port
// ============================================================================

/// State for a single EHCI port.
#[derive(Debug, Clone)]
pub struct EhciPort {
    /// Port number (0-based).
    pub index: u32,
    /// Whether this port has been fully processed (device enumerated or given up).
    pub processed: bool,
}

impl EhciPort {
    pub fn new(index: u32) -> Self {
        Self { index, processed: false }
    }
}

// ============================================================================
//  EhciPortContext — manages all root hub ports
// ============================================================================

/// Manages all EHCI root hub ports.
pub struct EhciPortContext {
    pub ports: alloc::vec::Vec<EhciPort>,
    pub n_ports: u32,
    /// Bitmask of processed ports (used for fast hotplug-aware re-evaluation).
    pub processed_mask: u32,
}

impl EhciPortContext {
    pub fn new(n_ports: u32) -> Self {
        let mut ports = alloc::vec::Vec::new();
        for i in 0..n_ports {
            ports.push(EhciPort::new(i));
        }
        Self { ports, n_ports, processed_mask: 0 }
    }

    /// Clear the "processed" flags for all ports (e.g. on PCD hotplug).
    pub fn clear_processed(&mut self) {
        for port in &mut self.ports {
            port.processed = false;
        }
        self.processed_mask = 0;
    }

    /// Mark a port as processed.
    pub fn mark_processed(&mut self, port: u32) {
        if let Some(p) = self.ports.get_mut(port as usize) {
            p.processed = true;
        }
        if port < 32 {
            self.processed_mask |= 1 << port;
        }
    }

    /// Check if a port has been processed.
    pub fn is_processed(&self, port: u32) -> bool {
        if port < 32 {
            self.processed_mask & (1 << port) != 0
        } else {
            self.ports.get(port as usize).map(|p| p.processed).unwrap_or(true)
        }
    }
}

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_context() {
        let ctx = EhciPortContext::new(4);
        assert_eq!(ctx.n_ports, 4);
        assert!(!ctx.is_processed(0));
    }
}