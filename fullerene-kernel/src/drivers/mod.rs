pub mod fat;
pub mod usb_storage;
pub mod virtio_gpu;

/// Thin kernel wrapper around `nitrogen::storage::ahci`.
pub fn init_ahci() {
    nitrogen::storage::ahci::init(&crate::driver_context_impl::KernelDriverContext);
}

/// Thin kernel wrapper around `nitrogen::storage::nvme`.
pub fn init_nvme() {
    nitrogen::storage::nvme::init(&crate::driver_context_impl::KernelDriverContext);
}
