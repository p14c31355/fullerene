/// Qualcomm SM7250 / Pixel 4a 5G (bramble) early-boot addresses.
///
/// The DTB remains authoritative at boot. These constants document the
/// addresses used by the SM7250 device tree for the first bring-up.
pub const UART_BASE: usize = 0x0098_8000;
pub const GICD_BASE: usize = 0x17a0_0000;
pub const GICR_BASE: usize = 0x17a6_0000;

pub fn init_interrupt_controller(gicd_base: Option<usize>, gicr_base: Option<usize>) {
    super::gicv3::init(
        gicd_base.unwrap_or(GICD_BASE),
        gicr_base.unwrap_or(GICR_BASE),
    );
}
