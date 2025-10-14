//! Device Manager Implementation
//!
//! This module provides a centralized device management system that handles
//! device registration, discovery, and lifecycle management.

use crate::*;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;

/// Device information structure
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub name: &'static str,
    pub device_type: &'static str,
    pub enabled: bool,
    pub priority: i32,
}

impl DeviceInfo {
    pub fn new(name: &'static str, device_type: &'static str, priority: i32) -> Self {
        Self {
            name,
            device_type,
            enabled: false,
            priority,
        }
    }
}

/// Device information structure with device
pub struct DeviceEntry {
    pub device: alloc::boxed::Box<dyn HardwareDevice + Send>,
    pub device_info: DeviceInfo,
}

/// Device manager for handling hardware devices
pub struct DeviceManager {
    devices: Mutex<BTreeMap<&'static str, DeviceEntry>>,
}

impl DeviceManager {
    /// Create a new device manager
    pub const fn new() -> Self {
        Self {
            devices: Mutex::new(BTreeMap::new()),
        }
    }

    /// Register a hardware device
    pub fn register_device(
        &self,
        device: alloc::boxed::Box<dyn HardwareDevice + Send>,
    ) -> SystemResult<()> {
        let name = device.name();

        // Get device info before moving the device
        let device_info = DeviceInfo::new(
            device.device_name(),
            device.device_type(),
            50, // Default priority for hardware devices
        );

        // Store device and its info
        let mut devices = self.devices.lock();
        devices.insert(name, DeviceEntry { device, device_info });

        log_info!("Device registered successfully");
        Ok(())
    }

    /// Enable a device by name
    pub fn enable_device(&self, name: &str) -> SystemResult<()> {
        if let Some(device_entry) = self.devices.lock().get_mut(name) {
            device_entry.device.enable()?;
            device_entry.device_info.enabled = true;
            log_info!("Device enabled");
            Ok(())
        } else {
            Err(SystemError::DeviceNotFound)
        }
    }

    /// Disable a device by name
    pub fn disable_device(&self, name: &str) -> SystemResult<()> {
        if let Some(device_entry) = self.devices.lock().get_mut(name) {
            device_entry.device.disable()?;
            device_entry.device_info.enabled = false;
            log_info!("Device disabled");
            Ok(())
        } else {
            Err(SystemError::DeviceNotFound)
        }
    }

    /// Reset a device by name
    pub fn reset_device(&self, name: &str) -> SystemResult<()> {
        if let Some(device_entry) = self.devices.lock().get_mut(name) {
            device_entry.device.reset()?;
            log_info!("Device reset");
            Ok(())
        } else {
            Err(SystemError::DeviceNotFound)
        }
    }

    /// Get a device by name for direct access using a closure
    pub fn with_device<F, R>(&self, name: &str, f: F) -> Option<R>
    where
        F: FnOnce(&(dyn HardwareDevice + Send)) -> R,
    {
        self.devices.lock().get(name).map(|d| f(&*d.device))
    }

    /// Get a mutable reference to a device by name using a closure
    pub fn with_device_mut<F, R>(&self, name: &str, f: F) -> Option<R>
    where
        F: FnOnce(&mut (dyn HardwareDevice + Send)) -> R,
    {
        self.devices.lock().get_mut(name).map(|d| f(&mut *d.device))
    }

    /// Initialize all registered devices in priority order
    pub fn initialize_all_devices(&self) -> SystemResult<()> {
        let mut devices = self.devices.lock();
        let mut device_list: Vec<_> = devices.values_mut().collect();

        // Sort by priority (higher priority first)
        device_list.sort_by(|a, b| {
            let a_priority = <dyn HardwareDevice as traits::Initializable>::priority(&*a.device);
            let b_priority = <dyn HardwareDevice as traits::Initializable>::priority(&*b.device);
            b_priority.cmp(&a_priority)
        });

        for device_entry in device_list {
            if let Err(e) = device_entry.device.init() {
                log_error!(e, "Failed to initialize device");
                return Err(e);
            }
        }

        log_info!("All devices initialized");
        Ok(())
    }

    /// Enable all registered devices
    pub fn enable_all_devices(&self) -> SystemResult<()> {
        let device_names: Vec<_> = self.devices.lock().keys().cloned().collect();

        for name in device_names {
            self.enable_device(name)?;
        }

        log_info!("All devices enabled");
        Ok(())
    }

    /// Disable all registered devices
    pub fn disable_all_devices(&self) -> SystemResult<()> {
        let device_names: Vec<_> = self.devices.lock().keys().cloned().collect();

        for name in device_names.iter().rev() {
            self.disable_device(name)?;
        }

        log_info!("All devices disabled");
        Ok(())
    }

    /// Get device information
    pub fn get_device_info(&self, name: &str) -> Option<DeviceInfo> {
        self.devices.lock().get(name).map(|entry| entry.device_info.clone())
    }

    /// List all registered devices
    pub fn list_devices(&self) -> Vec<DeviceInfo> {
        self.devices.lock().values().map(|entry| entry.device_info.clone()).collect()
    }
}

impl Initializable for DeviceManager {
    fn init(&mut self) -> SystemResult<()> {
        log_info!("DeviceManager initialized");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "DeviceManager"
    }

    fn priority(&self) -> i32 {
        100 // Very high priority for device manager
    }
}

impl ErrorLogging for DeviceManager {
    fn log_error(&self, error: &SystemError, context: &'static str) {
        log_error!(error, context);
    }

    fn log_warning(&self, message: &'static str) {
        log_warning!(message);
    }

    fn log_info(&self, message: &'static str) {
        log_info!(message);
    }

    fn log_debug(&self, message: &'static str) {
        log_debug!(message);
    }

    fn log_trace(&self, message: &'static str) {
        log_trace!(message);
    }
}

// Global device manager instance
static DEVICE_MANAGER: Mutex<Option<DeviceManager>> = Mutex::new(None);

/// Initialize the global device manager
pub fn init_device_manager() -> SystemResult<()> {
    let mut manager = DEVICE_MANAGER.lock();
    *manager = Some(DeviceManager::new());
    log_info!("Global device manager initialized");
    Ok(())
}

/// Get a reference to the global device manager
pub fn get_device_manager() -> &'static spin::Mutex<Option<DeviceManager>> {
    // This is safe because we initialize the device manager early in system startup
    &DEVICE_MANAGER
}

/// Register a device globally
pub fn register_device(device: alloc::boxed::Box<dyn HardwareDevice + Send>) -> SystemResult<()> {
    if let Some(manager) = DEVICE_MANAGER.lock().as_mut() {
        manager.register_device(device)
    } else {
        Err(SystemError::InternalError)
    }
}

/// Convenience function to register VGA device
pub fn register_vga_device() -> SystemResult<()> {
    use crate::graphics::vga_device::VgaDevice;

    let vga_device = alloc::boxed::Box::new(VgaDevice::new());
    register_device(vga_device)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;

    // Mock device for testing
    struct MockDevice {
        name: &'static str,
        enabled: bool,
    }

    impl MockDevice {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                enabled: false,
            }
        }
    }

    impl Initializable for MockDevice {
        fn init(&mut self) -> SystemResult<()> {
            Ok(())
        }

        fn name(&self) -> &'static str {
            self.name
        }

        fn priority(&self) -> i32 {
            50 // Default priority for mock devices
        }
    }

    impl ErrorLogging for MockDevice {
        fn log_error(&self, _error: &SystemError, _context: &'static str) {}
        fn log_warning(&self, _message: &'static str) {}
        fn log_info(&self, _message: &'static str) {}
        fn log_debug(&self, _message: &'static str) {}
        fn log_trace(&self, _message: &'static str) {}
    }

    impl HardwareDevice for MockDevice {
        fn device_name(&self) -> &'static str {
            self.name
        }

        fn device_type(&self) -> &'static str {
            "Mock"
        }

        fn enable(&mut self) -> SystemResult<()> {
            self.enabled = true;
            Ok(())
        }

        fn disable(&mut self) -> SystemResult<()> {
            self.enabled = false;
            Ok(())
        }

        fn reset(&mut self) -> SystemResult<()> {
            Ok(())
        }

        fn is_enabled(&self) -> bool {
            self.enabled
        }

        fn priority(&self) -> i32 {
            50 // Default priority for mock devices
        }
    }

    #[test]
    fn test_device_manager_creation() {
        let manager = DeviceManager::new();
        assert_eq!(manager.name(), "DeviceManager");
        assert_eq!(manager.priority(), 100);
    }

    #[test]
    fn test_device_registration() {
        let manager = DeviceManager::new();
        let mock_device = Box::new(MockDevice::new("test_device"));

        assert!(manager.register_device(mock_device).is_ok());

        // Test the new closure-based API
        let device_name = manager.with_device("test_device", |device| device.device_name());
        assert_eq!(device_name, Some("test_device"));
    }

    #[test]
    fn test_device_enable_disable() {
        let manager = DeviceManager::new();
        let mock_device = Box::new(MockDevice::new("test_device"));

        manager.register_device(mock_device).unwrap();

        assert!(manager.enable_device("test_device").is_ok());
        let info = manager.get_device_info("test_device").unwrap();
        assert!(info.enabled);

        assert!(manager.disable_device("test_device").is_ok());
        let info = manager.get_device_info("test_device").unwrap();
        assert!(!info.enabled);
    }

    #[test]
    fn test_nonexistent_device() {
        let manager = DeviceManager::new();

        assert!(manager.enable_device("nonexistent").is_err());
        assert!(manager.disable_device("nonexistent").is_err());
        assert!(manager.reset_device("nonexistent").is_err());
        assert!(manager.get_device_info("nonexistent").is_none());
    }
}
