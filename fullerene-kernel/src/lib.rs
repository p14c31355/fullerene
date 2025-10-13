//! Fullerene OS Kernel Library
//!
//! This library provides the core functionality for the Fullerene OS kernel,
//! including common traits, error types, and system abstractions.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![feature(slice_ptr_get)]
#![feature(sync_unsafe_cell)]
#![feature(vec_into_raw_parts)]

// Re-export core types for convenience
pub use core::result::Result;

// Common error type for the entire system
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemError {
    // System call errors
    InvalidSyscall = 1,
    BadFileDescriptor = 9,
    PermissionDenied = 13,
    FileNotFound = 2,
    NoSuchProcess = 3,
    InvalidArgument = 22,
    SyscallOutOfMemory = 12,

    // File system errors
    FileExists = 17,
    FsInvalidFileDescriptor = 25,
    InvalidSeek = 29,
    DiskFull = 28,

    // Memory management errors
    MappingFailed = 100,
    UnmappingFailed = 101,
    FrameAllocationFailed = 102,
    MemOutOfMemory = 103,

    // Loader errors
    InvalidFormat = 200,
    LoadFailed = 201,

    // Hardware errors
    DeviceNotFound = 300,
    DeviceError = 301,
    PortError = 302,

    // General errors
    NotImplemented = 400,
    InternalError = 500,
    UnknownError = 999,
}

impl From<crate::syscall::interface::SyscallError> for SystemError {
    fn from(error: crate::syscall::interface::SyscallError) -> Self {
        match error {
            crate::syscall::interface::SyscallError::InvalidSyscall => SystemError::InvalidSyscall,
            crate::syscall::interface::SyscallError::BadFileDescriptor => {
                SystemError::BadFileDescriptor
            }
            crate::syscall::interface::SyscallError::PermissionDenied => {
                SystemError::PermissionDenied
            }
            crate::syscall::interface::SyscallError::FileNotFound => SystemError::FileNotFound,
            crate::syscall::interface::SyscallError::NoSuchProcess => SystemError::NoSuchProcess,
            crate::syscall::interface::SyscallError::InvalidArgument => {
                SystemError::InvalidArgument
            }
            crate::syscall::interface::SyscallError::OutOfMemory => SystemError::SyscallOutOfMemory,
        }
    }
}

impl From<crate::fs::FsError> for SystemError {
    fn from(error: crate::fs::FsError) -> Self {
        match error {
            crate::fs::FsError::FileNotFound => SystemError::FileNotFound,
            crate::fs::FsError::FileExists => SystemError::FileExists,
            crate::fs::FsError::PermissionDenied => SystemError::PermissionDenied,
            crate::fs::FsError::InvalidFileDescriptor => SystemError::FsInvalidFileDescriptor,
            crate::fs::FsError::InvalidSeek => SystemError::InvalidSeek,
            crate::fs::FsError::DiskFull => SystemError::DiskFull,
        }
    }
}

impl From<crate::memory_management::MapError> for SystemError {
    fn from(error: crate::memory_management::MapError) -> Self {
        match error {
            crate::memory_management::MapError::MappingFailed => SystemError::MappingFailed,
            crate::memory_management::MapError::UnmappingFailed => SystemError::UnmappingFailed,
            crate::memory_management::MapError::FrameAllocationFailed => {
                SystemError::FrameAllocationFailed
            }
        }
    }
}

impl From<crate::memory_management::AllocError> for SystemError {
    fn from(error: crate::memory_management::AllocError) -> Self {
        match error {
            crate::memory_management::AllocError::OutOfMemory => SystemError::MemOutOfMemory,
            crate::memory_management::AllocError::MappingFailed => SystemError::MappingFailed,
        }
    }
}

impl From<crate::memory_management::FreeError> for SystemError {
    fn from(error: crate::memory_management::FreeError) -> Self {
        match error {
            crate::memory_management::FreeError::UnmappingFailed => SystemError::UnmappingFailed,
        }
    }
}

impl From<crate::loader::LoadError> for SystemError {
    fn from(error: crate::loader::LoadError) -> Self {
        match error {
            crate::loader::LoadError::InvalidFormat => SystemError::InvalidFormat,
            // Map LoadFailed to LoadFailed error code
            _ => SystemError::LoadFailed,
        }
    }
}

// Common result type
pub type SystemResult<T> = Result<T, SystemError>;

// Initializable trait for system components
pub trait Initializable: Send {
    /// Initialize the component
    fn init(&mut self) -> SystemResult<()>;

    /// Get the component name for logging
    fn name(&self) -> &'static str;

    /// Check if the component requires initialization before others
    fn priority(&self) -> i32 {
        0 // Default priority
    }

    /// Get dependencies that must be initialized first
    fn dependencies(&self) -> &'static [&'static str] {
        &[]
    }
}

// Error logging trait
pub trait ErrorLogging {
    fn log_error(&self, error: &SystemError, context: &'static str);
    fn log_warning(&self, message: &'static str);
    fn log_info(&self, message: &'static str);
}

// Hardware device trait
pub trait HardwareDevice: Initializable + ErrorLogging {
    /// Get device name
    fn device_name(&self) -> &'static str;

    /// Get device type
    fn device_type(&self) -> &'static str;

    /// Enable the device
    fn enable(&mut self) -> SystemResult<()>;

    /// Disable the device
    fn disable(&mut self) -> SystemResult<()>;

    /// Reset the device
    fn reset(&mut self) -> SystemResult<()>;

    /// Check if device is enabled
    fn is_enabled(&self) -> bool;
}

// Memory manager trait
pub trait MemoryManager: Initializable + ErrorLogging {
    /// Allocate memory pages
    fn allocate_pages(&mut self, count: usize) -> SystemResult<usize>;

    /// Free memory pages
    fn free_pages(&mut self, address: usize, count: usize) -> SystemResult<()>;

    /// Get total memory size
    fn total_memory(&self) -> usize;

    /// Get available memory size
    fn available_memory(&self) -> usize;

    /// Get used memory size
    fn used_memory(&self) -> usize {
        self.total_memory().saturating_sub(self.available_memory())
    }

    /// Map virtual address to physical address
    fn map_address(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        count: usize,
    ) -> SystemResult<()>;

    /// Unmap virtual address
    fn unmap_address(&mut self, virtual_addr: usize, count: usize) -> SystemResult<()>;

    /// Get physical address for virtual address
    fn virtual_to_physical(&self, virtual_addr: usize) -> SystemResult<usize>;

    /// Initialize paging structures
    fn init_paging(&mut self) -> SystemResult<()>;

    /// Get page size
    fn page_size(&self) -> usize {
        4096 // Default 4KB pages
    }
}

// Process memory manager trait for per-process memory management
pub trait ProcessMemoryManager: MemoryManager {
    /// Create new address space for a process
    fn create_address_space(&mut self, process_id: usize) -> SystemResult<()>;

    /// Switch to different address space
    fn switch_address_space(&mut self, process_id: usize) -> SystemResult<()>;

    /// Destroy address space for a process
    fn destroy_address_space(&mut self, process_id: usize) -> SystemResult<()>;

    /// Allocate heap memory for process
    fn allocate_heap(&mut self, size: usize) -> SystemResult<usize>;

    /// Free heap memory for process
    fn free_heap(&mut self, address: usize, size: usize) -> SystemResult<()>;

    /// Allocate stack memory for process
    fn allocate_stack(&mut self, size: usize) -> SystemResult<usize>;

    /// Free stack memory for process
    fn free_stack(&mut self, address: usize, size: usize) -> SystemResult<()>;

    /// Copy memory between different address spaces
    fn copy_memory_between_processes(
        &mut self,
        from_process: usize,
        to_process: usize,
        from_addr: usize,
        to_addr: usize,
        size: usize,
    ) -> SystemResult<()>;

    /// Get current process ID
    fn current_process_id(&self) -> usize;
}

// Page table helper trait for common page table operations
pub trait PageTableHelper: Initializable + ErrorLogging {
    /// Map a virtual page to physical frame
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageFlags,
    ) -> SystemResult<()>;

    /// Unmap a virtual page
    fn unmap_page(&mut self, virtual_addr: usize) -> SystemResult<()>;

    /// Get physical address for virtual address
    fn translate_address(&self, virtual_addr: usize) -> SystemResult<usize>;

    /// Set page flags
    fn set_page_flags(&mut self, virtual_addr: usize, flags: PageFlags) -> SystemResult<()>;

    /// Get page flags
    fn get_page_flags(&self, virtual_addr: usize) -> SystemResult<PageFlags>;

    /// Flush TLB for address
    fn flush_tlb(&mut self, virtual_addr: usize) -> SystemResult<()>;

    /// Flush entire TLB
    fn flush_tlb_all(&mut self) -> SystemResult<()>;

    /// Create new page table
    fn create_page_table(&mut self) -> SystemResult<usize>;

    /// Destroy page table
    fn destroy_page_table(&mut self, table_addr: usize) -> SystemResult<()>;

    /// Clone page table
    fn clone_page_table(&mut self, source_table: usize) -> SystemResult<usize>;

    /// Switch page table
    fn switch_page_table(&mut self, table_addr: usize) -> SystemResult<()>;

    /// Get current page table address
    fn current_page_table(&self) -> usize;
}

// Page flags for memory mapping
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags {
    pub present: bool,
    pub writable: bool,
    pub user_accessible: bool,
    pub write_through: bool,
    pub cache_disabled: bool,
    pub accessed: bool,
    pub dirty: bool,
    pub huge_page: bool,
    pub global: bool,
    pub no_execute: bool,
}

impl PageFlags {
    /// Create new page flags with default values
    pub const fn new() -> Self {
        Self {
            present: false,
            writable: false,
            user_accessible: false,
            write_through: false,
            cache_disabled: false,
            accessed: false,
            dirty: false,
            huge_page: false,
            global: false,
            no_execute: false,
        }
    }

    /// Create flags for kernel code (read-only, supervisor)
    pub const fn kernel_code() -> Self {
        Self {
            present: true,
            writable: false,
            user_accessible: false,
            write_through: false,
            cache_disabled: false,
            accessed: false,
            dirty: false,
            huge_page: false,
            global: true,
            no_execute: false,
        }
    }

    /// Create flags for kernel data (read-write, supervisor)
    pub const fn kernel_data() -> Self {
        Self {
            present: true,
            writable: true,
            user_accessible: false,
            write_through: false,
            cache_disabled: false,
            accessed: false,
            dirty: false,
            huge_page: false,
            global: true,
            no_execute: false,
        }
    }

    /// Create flags for user code (read-only, user)
    pub const fn user_code() -> Self {
        Self {
            present: true,
            writable: false,
            user_accessible: true,
            write_through: false,
            cache_disabled: false,
            accessed: false,
            dirty: false,
            huge_page: false,
            global: false,
            no_execute: false,
        }
    }

    /// Create flags for user data (read-write, user)
    pub const fn user_data() -> Self {
        Self {
            present: true,
            writable: true,
            user_accessible: true,
            write_through: false,
            cache_disabled: false,
            accessed: false,
            dirty: false,
            huge_page: false,
            global: false,
            no_execute: true,
        }
    }

    /// Create flags for memory-mapped I/O
    pub const fn mmio() -> Self {
        Self {
            present: true,
            writable: true,
            user_accessible: false,
            write_through: true,
            cache_disabled: true,
            accessed: false,
            dirty: false,
            huge_page: false,
            global: true,
            no_execute: false,
        }
    }

    /// Convert flags to architecture-specific format
    pub fn to_arch_specific(&self) -> usize {
        let mut flags = 0;

        if self.present {
            flags |= 1 << 0;
        }
        if self.writable {
            flags |= 1 << 1;
        }
        if self.user_accessible {
            flags |= 1 << 2;
        }
        if self.write_through {
            flags |= 1 << 3;
        }
        if self.cache_disabled {
            flags |= 1 << 4;
        }
        if self.accessed {
            flags |= 1 << 5;
        }
        if self.dirty {
            flags |= 1 << 6;
        }
        if self.huge_page {
            flags |= 1 << 7;
        }
        if self.global {
            flags |= 1 << 8;
        }
        if self.no_execute {
            flags |= 1 << 63;
        } // NX bit in x86_64

        flags
    }

    /// Create flags from architecture-specific format
    pub fn from_arch_specific(flags: usize) -> Self {
        Self {
            present: (flags & (1 << 0)) != 0,
            writable: (flags & (1 << 1)) != 0,
            user_accessible: (flags & (1 << 2)) != 0,
            write_through: (flags & (1 << 3)) != 0,
            cache_disabled: (flags & (1 << 4)) != 0,
            accessed: (flags & (1 << 5)) != 0,
            dirty: (flags & (1 << 6)) != 0,
            huge_page: (flags & (1 << 7)) != 0,
            global: (flags & (1 << 8)) != 0,
            no_execute: (flags & (1 << 63)) != 0,
        }
    }
}

// Frame allocator trait for physical frame management
pub trait FrameAllocator: Initializable + ErrorLogging {
    /// Allocate a single frame
    fn allocate_frame(&mut self) -> SystemResult<usize>;

    /// Free a single frame
    fn free_frame(&mut self, frame_addr: usize) -> SystemResult<()>;

    /// Allocate multiple contiguous frames
    fn allocate_contiguous_frames(&mut self, count: usize) -> SystemResult<usize>;

    /// Free multiple contiguous frames
    fn free_contiguous_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()>;

    /// Get total frame count
    fn total_frames(&self) -> usize;

    /// Get available frame count
    fn available_frames(&self) -> usize;

    /// Get used frame count
    fn used_frames(&self) -> usize {
        self.total_frames().saturating_sub(self.available_frames())
    }

    /// Reserve frame range for special use
    fn reserve_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()>;

    /// Release reserved frame range
    fn release_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()>;

    /// Check if frame is available
    fn is_frame_available(&self, frame_addr: usize) -> bool;

    /// Get frame size in bytes
    fn frame_size(&self) -> usize {
        4096 // Default 4KB frames
    }
}

// System call handler trait
pub trait SyscallHandler: Initializable + ErrorLogging {
    /// Handle a system call
    fn handle_syscall(&mut self, number: u64, args: &[u64]) -> SystemResult<u64>;

    /// Register a system call handler
    fn register_syscall(
        &mut self,
        number: u64,
        handler: fn(&[u64]) -> SystemResult<u64>,
    ) -> SystemResult<()>;

    /// Get supported system call numbers
    fn supported_syscalls(&self) -> &'static [u64];
}

// Logger trait for system logging
pub trait Logger: ErrorLogging {
    fn set_log_level(&mut self, level: LogLevel);
    fn get_log_level(&self) -> LogLevel;
}

// Log levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error = 0,
    Warning = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

// Global logger instance
use spin::Mutex;

pub struct GlobalLogger {
    level: LogLevel,
}

impl GlobalLogger {
    pub const fn new() -> Self {
        Self {
            level: LogLevel::Info,
        }
    }

    pub fn set_level(&mut self, level: LogLevel) {
        self.level = level;
    }

    pub fn get_level(&self) -> LogLevel {
        self.level
    }
}

impl ErrorLogging for GlobalLogger {
    fn log_error(&self, error: &SystemError, context: &'static str) {
        if self.level >= LogLevel::Error {
            // A more robust formatting implementation would be ideal, but this provides basic visibility.
            petroleum::serial::serial_log(format_args!("[ERROR] "));
            petroleum::serial::serial_log(format_args!("{}", context));
            petroleum::serial::serial_log(format_args!("\n"));
        }
    }

    fn log_warning(&self, message: &'static str) {
        if self.level >= LogLevel::Warning {
            // Simplified logging without format_args for now
        }
    }

    fn log_info(&self, message: &'static str) {
        if self.level >= LogLevel::Info {
            // Simplified logging without format_args for now
        }
    }
}

static GLOBAL_LOGGER: Mutex<Option<GlobalLogger>> = Mutex::new(None);

// Initialize global logger
pub fn init_global_logger() {
    *GLOBAL_LOGGER.lock() = Some(GlobalLogger::new());
}

// Set global log level
pub fn set_global_log_level(level: LogLevel) {
    if let Some(logger) = GLOBAL_LOGGER.lock().as_mut() {
        logger.set_level(level);
    }
}

// Get global log level
pub fn get_global_log_level() -> LogLevel {
    if let Some(logger) = GLOBAL_LOGGER.lock().as_ref() {
        logger.get_level()
    } else {
        LogLevel::Info // Default level
    }
}

// Global error logging functions
pub fn log_error(error: &SystemError, context: &'static str) {
    if let Some(logger) = GLOBAL_LOGGER.lock().as_ref() {
        logger.log_error(error, context);
    }
}

pub fn log_warning(message: &'static str) {
    if let Some(logger) = GLOBAL_LOGGER.lock().as_ref() {
        logger.log_warning(message);
    }
}

pub fn log_info(message: &'static str) {
    if let Some(logger) = GLOBAL_LOGGER.lock().as_ref() {
        logger.log_info(message);
    }
}

// System initializer for managing component initialization
pub struct SystemInitializer {
    components: alloc::vec::Vec<alloc::boxed::Box<dyn Initializable + Send>>,
}

impl SystemInitializer {
    pub fn new() -> Self {
        Self {
            components: alloc::vec::Vec::new(),
        }
    }

    /// Register a component for initialization
    pub fn register_component(&mut self, component: alloc::boxed::Box<dyn Initializable + Send>) {
        self.components.push(component);
    }

    /// Initialize all registered components in dependency order
    pub fn initialize_system(&mut self) -> SystemResult<()> {
        // Sort components by priority (higher priority first)
        self.components.sort_by(
            |a: &alloc::boxed::Box<dyn Initializable + Send>,
             b: &alloc::boxed::Box<dyn Initializable + Send>| {
                b.priority().cmp(&a.priority())
            },
        );

        // TODO: Implement proper dependency resolution
        // For now, just initialize in priority order

        for component in &mut self.components {
            // Initialize component without format strings for now
            if let Err(e) = component.init() {
                return Err(e);
            }
        }

        Ok(())
    }
}

static SYSTEM_INITIALIZER: spin::Once<Mutex<SystemInitializer>> = spin::Once::new();

// Register a component globally
pub fn register_system_component(component: alloc::boxed::Box<dyn Initializable + Send>) {
    SYSTEM_INITIALIZER
        .call_once(|| Mutex::new(SystemInitializer::new()))
        .lock()
        .register_component(component);
}

// Initialize the entire system
pub fn initialize_system() -> SystemResult<()> {
    SYSTEM_INITIALIZER
        .call_once(|| Mutex::new(SystemInitializer::new()))
        .lock()
        .initialize_system()
}

// Kernel modules
pub mod gdt; // Add GDT module
pub mod graphics;
pub mod hardware;
pub mod heap;
pub mod interrupts;
pub mod vga;
// Kernel modules
pub mod context_switch; // Context switching
pub mod fs; // Basic filesystem
pub mod keyboard; // Keyboard input driver
pub mod loader; // Program loader
pub mod macros; // Logging and utility macros
pub mod memory_management; // Virtual memory management
pub mod process; // Process management
pub mod shell;
pub mod syscall; // System calls // Shell/CLI interface

// Submodules for modularizing main.rs
pub mod boot;
pub mod init;
pub mod memory;
pub mod test_process;

// Re-export commonly used types for convenience
pub use graphics::vga_device::VgaDevice;
pub use hardware::{
    device_manager::DeviceManager,
    pci::{PciConfigSpace, PciDevice, PciScanner},
    ports::HardwarePorts,
};
pub use memory_management::{
    AllocError, BitmapFrameAllocator, FreeError, MapError, PageTableManager,
    ProcessMemoryManagerImpl, ProcessPageTable, UnifiedMemoryManager, convenience,
};
// Re-export critical types from memory_management module for internal use
pub use memory_management::{get_memory_manager, init_memory_manager};
pub use process::{PROCESS_LIST, Process, ProcessId};

// Core types and traits are already defined above and accessible from submodules

extern crate alloc;

use spin::Once;

// Panic handlers are defined in main.rs

use petroleum::page_table::EfiMemoryDescriptor;

static MEMORY_MAP: Once<&'static [EfiMemoryDescriptor]> = Once::new();

const VGA_BUFFER_ADDRESS: usize = 0xb8000;
const VGA_COLOR_GREEN_ON_BLACK: u16 = 0x0200;

// A simple loop that halts the CPU until the next interrupt
pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
