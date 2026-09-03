/// Qualcomm SM7250 / Pixel 4a 5G (bramble) early-boot addresses.
pub mod usb_clock;
pub mod usb_reset;

#[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_link_clocks_after_runstop)]
pub use usb_clock::android_controller_block_reset;
pub use usb_clock::{
    android_dbm_reset_and_enable, configure_usb_clocks, configure_usb_controller_clocks,
    disable_usb_clock_branches, enable_usb_clock_branches, enable_usb_qmp_clock_branches,
    enable_usb2_utmi_clock, rearm_usb_controller_clock_branches, rearm_usb2_android_clock_branches,
};
pub use usb_reset::{pulse_usb2_phy_reset, reset_qmp_phy_blocks, reset_usb_blocks};
///
/// The DTB remains authoritative at boot. These constants document the
/// addresses used by the SM7250 device tree for the first bring-up.
pub const UART_BASE: usize = 0x0098_8000;
pub const GICD_BASE: usize = 0x17a0_0000;
pub const GICR_BASE: usize = 0x17a6_0000;
/// Android's Lito DT routes the primary DWC3 device event interrupt here.
pub const USB_DWC3_IRQ: u32 = 240;
/// Android's Lito DT routes the Qualcomm glue power-event interrupt here.
pub const USB_PWR_EVENT_IRQ: u32 = 144;
/// PDC interrupt numbers used by the USB2/USB3 PHYs. These are PDC-local
/// lines, not GIC SPI numbers; keeping the distinction explicit prevents
/// accidentally programming them into GICD as if they were SPIs.
pub const USB_PDC_DP_HS_PHY_IRQ: u32 = 14;
pub const USB_PDC_SS_PHY_IRQ: u32 = 9;
pub const USB_PDC_DM_HS_PHY_IRQ: u32 = 15;
pub const USB_PDC_BASE: usize = 0x0b22_0000;
/// Second `reg` resource of the Android Lito PDC node. Linux uses this
/// firmware/SPI configuration window to mirror each PDC child's trigger type
/// onto its GIC parent.
pub const USB_PDC_SPI_CFG_BASE: usize = 0x17c0_00f0;
pub const USB_PDC_SPI_CFG_SIZE: usize = 0x60;
pub const USB_PDC_DP_HS_PARENT_IRQ: u32 = 494;
pub const USB_PDC_SS_PARENT_IRQ: u32 = 489;
pub const USB_PDC_DM_HS_PARENT_IRQ: u32 = 495;

/// GCC register block and USB3 power-domain registers from the Lito DT.
pub const GCC_BASE: usize = 0x0010_0000;
/// Unlike GCC branch registers, the GDSC node is exposed at an absolute SoC
/// address in the DT (`reg = <0x10f004 0x4>`), not as an offset from GCC_BASE.
pub const USB30_PRIM_GDSC: usize = 0x10f004;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockProvider {
    Gcc,
    Rpmh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockResource {
    pub name: &'static str,
    pub provider: ClockProvider,
    pub provider_id: u32,
    /// GCC branch or clock register from the DT clock provider.
    pub branch_offset: usize,
    /// RCG command register, or zero for a fixed/parent clock.
    pub source_offset: usize,
    pub normal_rate_hz: u32,
    pub high_speed_rate_hz: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetResource {
    pub name: &'static str,
    pub provider_id: u32,
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerOwner {
    SecureFirmware,
    AppsCpu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbRailResource {
    pub name: &'static str,
    /// RPMh VRM resource name resolved through Command DB. This is a
    /// resource identifier, not a guessed PMIC register address.
    pub rpmh_resource_id: [u8; 8],
    pub min_uv: u32,
    pub max_uv: u32,
    pub max_load_ua: u32,
    pub program_voltage: bool,
    pub owner: PowerOwner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbPowerResources {
    pub hs_phy_rails: [UsbRailResource; 3],
    pub qmp_vdd: UsbRailResource,
    pub qmp_vdd_present: bool,
    pub qmp_core: UsbRailResource,
    pub qmp_vbus_valid_override: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbIrqKind {
    Pdc,
    GicSpi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrqTrigger {
    RisingEdge,
    EdgeBoth,
    LevelHigh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqResource {
    pub name: &'static str,
    pub number: u32,
    pub kind: UsbIrqKind,
    pub trigger: IrqTrigger,
}

/// One SPMI child interrupt specifier from the PMIC DT.  The first Type-C
/// interrupt is delivered through the SPMI arbiter's summary GIC SPI; the
/// remaining fields are consumed by the PMIC interrupt block itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpmiIrqResource {
    pub sid: u8,
    pub peripheral_id: u8,
    pub irq: u8,
    pub trigger: IrqTrigger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GsiResource {
    pub event_buffer_count: u32,
    pub general_cfg_offset: usize,
    pub doorbell_low_offset: usize,
    pub doorbell_high_offset: usize,
    pub ring_base_low_offset: usize,
    pub ring_base_high_offset: usize,
    pub interface_status_offset: usize,
    pub disable_io_coherency: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaPoolResource {
    pub iova_base: usize,
    pub size: usize,
    pub stream_id: u32,
}

/// Validate a DMA object against the IOVA window advertised by the platform
/// DT.  The Qualcomm USB path must not hand DWC3 a CPU pointer outside this
/// window: an identity-mapped early buffer may look usable to the CPU while
/// the Apps SMMU still rejects the stream ID.  Keeping this check pure makes
/// it usable by both the boot-time allocator and host-side tests.
pub const fn dma_region_valid(
    pool: DmaPoolResource,
    address: u64,
    length: u64,
    alignment: u64,
) -> bool {
    if length == 0 || alignment == 0 || address % alignment != 0 {
        return false;
    }
    let Some(pool_end) = (pool.iova_base as u64).checked_add(pool.size as u64) else {
        return false;
    };
    let Some(region_end) = address.checked_add(length) else {
        return false;
    };
    address >= pool.iova_base as u64 && region_end <= pool_end
}

/// No-heap allocator for the DT-declared USB DMA window. This is the early
/// boot equivalent of dma_pool_alloc(): allocations are monotonic for one
/// controller lifetime and are never handed back while DWC3 may still own a
/// TRB. The allocator itself is platform-neutral and therefore testable
/// without MMIO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaPoolAllocator {
    base: u64,
    end: u64,
    next: u64,
}

impl DmaPoolAllocator {
    pub const fn new(pool: DmaPoolResource, first_free: u64) -> Option<Self> {
        let Some(end) = (pool.iova_base as u64).checked_add(pool.size as u64) else {
            return None;
        };
        let base = if first_free > pool.iova_base as u64 {
            first_free
        } else {
            pool.iova_base as u64
        };
        if base >= end {
            return None;
        }
        Some(Self {
            base,
            end,
            next: base,
        })
    }

    pub const fn allocate(&mut self, size: u64, alignment: u64) -> Option<u64> {
        if size == 0 || alignment == 0 || !alignment.is_power_of_two() {
            return None;
        }
        let Some(next_aligned) = self.next.checked_add(alignment - 1) else {
            return None;
        };
        let aligned = next_aligned & !(alignment - 1);
        let Some(end) = aligned.checked_add(size) else {
            return None;
        };
        if end > self.end {
            return None;
        }
        self.next = end;
        Some(aligned)
    }

    pub const fn remaining(&self) -> u64 {
        self.end.saturating_sub(self.next)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusVoteVector {
    pub master: u32,
    pub slave: u32,
    pub average_kbps: u32,
    pub instantaneous_kbps: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusVoteResource {
    pub mode_count: u8,
    pub path_count: u8,
    pub vectors: [[BusVoteVector; 3]; 4],
}

/// The RPMh transport that backs Lito's legacy `qcom,msm-bus-rsc` client.
/// These are hardware resources, not ordinary USB MMIO registers: a vote is
/// sent to a BCM through the Apps RSC TCS and the BCM address comes from the
/// reserved Command DB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RpmhRscResource {
    pub driver_base: usize,
    pub tcs_offset: usize,
    pub driver_id: u8,
    pub active_tcs: u8,
    pub sleep_tcs: u8,
    pub wake_tcs: u8,
    pub control_tcs: u8,
    pub active_tcs_offset: u8,
    pub sleep_tcs_offset: u8,
    pub wake_tcs_offset: u8,
}

/// Register layout selected from the Apps-RSC hardware revision.  Linux does
/// not assume that every Qualcomm target uses the v2.7 command stride: the
/// driver reads RSC_DRV_ID and selects the v3.0 register table for major
/// revisions >= 3.  Keep the selector pure so it can be tested without
/// touching secure-owned RPMh registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RpmhRegisterLayout {
    tcs_stride: usize,
    command_stride: usize,
    wait_for_completion: usize,
    control: usize,
    status: usize,
    command_enable: usize,
    command_msgid: usize,
    command_address: usize,
    command_data: usize,
    command_status: usize,
}

const RPMH_RSC_ID: usize = 0x00;
const RPMH_RSC_MAJOR_SHIFT: u32 = 16;
const RPMH_RSC_MAJOR_MASK: u32 = 0xff;

const RPMH_LAYOUT_V27: RpmhRegisterLayout = RpmhRegisterLayout {
    tcs_stride: 672,
    command_stride: 20,
    wait_for_completion: 0x10,
    control: 0x14,
    status: 0x18,
    command_enable: 0x1c,
    command_msgid: 0x30,
    command_address: 0x34,
    command_data: 0x38,
    command_status: 0x3c,
};

const RPMH_LAYOUT_V30: RpmhRegisterLayout = RpmhRegisterLayout {
    tcs_stride: 672,
    command_stride: 24,
    wait_for_completion: 0x20,
    control: 0x24,
    status: 0x28,
    command_enable: 0x2c,
    command_msgid: 0x34,
    command_address: 0x38,
    command_data: 0x3c,
    command_status: 0x40,
};

fn rpmh_register_layout(rsc_id: u32) -> Option<RpmhRegisterLayout> {
    let major = (rsc_id >> RPMH_RSC_MAJOR_SHIFT) & RPMH_RSC_MAJOR_MASK;
    match major {
        0 => None,
        1 | 2 => Some(RPMH_LAYOUT_V27),
        _ => Some(RPMH_LAYOUT_V30),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandDbResource {
    pub base: usize,
    pub size: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BcmVote {
    pub id: [u8; 8],
    pub average_kbps: u32,
    pub instantaneous_kbps: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbBcmVotes {
    pub count: usize,
    pub votes: [BcmVote; 4],
}

/// A single RPMh BCM command payload. In the legacy BCM TCS encoding `vote_x`
/// is the average-bandwidth field and `vote_y` is the instantaneous/peak field
/// (the names come from the RPMh ABI, not the DT spelling). The encoder is
/// kept pure so it can be tested without touching secure-owned RSC registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RpmhBcmCommand {
    pub address: u32,
    pub data: u32,
}

pub const fn encode_rpmh_bcm_command(
    address: u32,
    valid: bool,
    commit: bool,
    vote_x_average: u32,
    vote_y_peak: u32,
) -> RpmhBcmCommand {
    const VOTE_MASK: u32 = 0x3fff;
    let x = if vote_x_average > VOTE_MASK {
        VOTE_MASK
    } else {
        vote_x_average
    };
    let y = if vote_y_peak > VOTE_MASK {
        VOTE_MASK
    } else {
        vote_y_peak
    };
    RpmhBcmCommand {
        address,
        data: ((commit as u32) << 30) | ((valid as u32) << 29) | (x << 14) | y,
    }
}

/// The four msm-bus use cases declared by the Android Lito USB node.
///
/// Keep this as an explicit state instead of passing raw indices around: the
/// Android glue changes both the interconnect vote and the PM QoS request as
/// the controller moves between suspend, nominal, SVS, and the minimum vote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbBusVote {
    Suspend,
    Nominal,
    Svs,
    Minimum,
}

impl UsbBusVote {
    pub const fn index(self) -> usize {
        match self {
            Self::Suspend => 0,
            // The Android dwc3-msm glue selects bus case 2 for NOM and case
            // 1 for SVS, even though the DT comments describe the vectors
            // in suspend/nominal/svs/minimum order.
            Self::Nominal => 2,
            Self::Svs => 1,
            Self::Minimum => 3,
        }
    }
}

/// Platform-visible result of a USB performance transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbPerformanceState {
    pub vote: UsbBusVote,
    pub pm_qos_latency_us: u32,
    pub core_rate_hz: u32,
}

/// Clock-source programming selected by the Android Qualcomm glue for each
/// performance state. The RCG divider values are the encoded HID dividers
/// used by GCC (8 represents the 4.5 divider and 5 represents the 3 divider
/// in the Lito tables), not literal integer divide-by-N requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbClockPlan {
    pub core_parent: u32,
    pub core_divider: u32,
    pub utmi_parent: u32,
    pub utmi_divider: u32,
    pub branches_enabled: bool,
}

/// Linux's PM QoS request is a CPU latency constraint, not a DWC3 register.
/// Keep its lifetime and IRQ-affinity requirement explicit in the platform
/// contract so the Rust path does not silently treat the DT value as a USB
/// MMIO field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbPmQosPolicy {
    pub latency_us: u32,
    pub irq_affine: bool,
}

/// Distributor base captured when the platform interrupt controller becomes
/// the owner of the USB SPI. Zero means that early boot is still polling and
/// PM QoS is represented only in `UsbResourceState`.
static mut USB_GICD_BASE: usize = 0;

/// Resolve the Android glue's performance state without performing an
/// interconnect transaction. The transport for RPMh/msm-bus is a separate
/// firmware interface; the values are read from the active DT-derived
/// resource view while the actual vote remains an explicit unsafe operation.
pub fn usb_performance_state(vote: UsbBusVote) -> UsbPerformanceState {
    let resources = usb_resources();
    let core_rate_hz = match vote {
        UsbBusVote::Nominal => resources.core_clk_rate_hz,
        UsbBusVote::Svs => resources.core_clk_rate_hs_hz,
        UsbBusVote::Suspend | UsbBusVote::Minimum => 0,
    };
    UsbPerformanceState {
        vote,
        // dwc3_msm_perf_vote_update() installs the DT latency only for NOM;
        // SVS and OFF restore PM_QOS_DEFAULT_VALUE.
        pm_qos_latency_us: match vote {
            UsbBusVote::Nominal => resources.pm_qos_latency_us,
            UsbBusVote::Svs | UsbBusVote::Suspend | UsbBusVote::Minimum => 0,
        },
        core_rate_hz,
    }
}

pub fn usb_pm_qos_policy(vote: UsbBusVote) -> UsbPmQosPolicy {
    UsbPmQosPolicy {
        latency_us: usb_performance_state(vote).pm_qos_latency_us,
        // The Android glue binds the USB IRQ to the CPU that owns the request;
        // retaining this bit documents that the request is IRQ-driven rather
        // than a best-effort logging value.
        irq_affine: !matches!(vote, UsbBusVote::Suspend | UsbBusVote::Minimum),
    }
}

/// Apply the lifetime of the Linux `pm_qos_add_request()` equivalent used by
/// the Qualcomm USB glue. The resource state records the CPU-latency policy;
/// once GIC ownership is live, its enforceable portion is also applied to the
/// USB SPI priority and boot-CPU route.
pub fn apply_usb_pm_qos(vote: UsbBusVote) -> UsbPmQosPolicy {
    let policy = usb_pm_qos_policy(vote);
    set_usb_resource_state(|state| {
        state.pm_qos_active = policy.latency_us != 0;
        state.pm_qos_latency_us = policy.latency_us;
        state.pm_qos_irq_affine = policy.irq_affine;
    });
    let gicd = unsafe { USB_GICD_BASE };
    if gicd != 0 {
        unsafe {
            let _ = super::gicv3::set_spi_usb_latency_policy(
                gicd,
                usb_controller_irq(),
                policy.irq_affine,
            );
        }
    }
    policy
}

pub const fn usb_clock_plan(vote: UsbBusVote) -> UsbClockPlan {
    match vote {
        UsbBusVote::Nominal => UsbClockPlan {
            core_parent: 1,
            core_divider: 8,
            // gcc_usb30_prim_mock_utmi_clk_src: 19.2 MHz from BI_TCXO.
            // This is the only rate in the Bramble Lito GCC table.
            utmi_parent: 0,
            utmi_divider: 1,
            branches_enabled: true,
        },
        UsbBusVote::Svs => UsbClockPlan {
            core_parent: 6,
            core_divider: 8,
            utmi_parent: 0,
            utmi_divider: 1,
            branches_enabled: true,
        },
        UsbBusVote::Suspend | UsbBusVote::Minimum => UsbClockPlan {
            // The Qualcomm glue gates branches during OFF; no RCG update is
            // required for the suspended controller.
            core_parent: 0,
            core_divider: 0,
            utmi_parent: 0,
            utmi_divider: 0,
            branches_enabled: false,
        },
    }
}

pub fn usb_bus_vectors(vote: UsbBusVote) -> [BusVoteVector; 3] {
    usb_resources().bus_vote.vectors[vote.index()]
}

const fn bcm_id(bytes: [u8; 4]) -> [u8; 8] {
    [bytes[0], bytes[1], bytes[2], bytes[3], 0, 0, 0, 0]
}

const BCM_MC0: [u8; 8] = bcm_id(*b"MC0\0");
const BCM_ACV: [u8; 8] = bcm_id(*b"ACV\0");
const BCM_CN0: [u8; 8] = bcm_id(*b"CN0\0");
const BCM_SH4: [u8; 8] = bcm_id(*b"SH4\0");

/// Collapse the three Android USB bus paths into the BCM requests that the
/// Lito bus topology actually owns. EBI is backed by MC0 and ACV; both IPA
/// and USB3 are on CN0; the Apps master side is SH4. Average bandwidth is
/// additive on a shared BCM, while instantaneous bandwidth is a maximum.
pub fn usb_bcm_votes(vote: UsbBusVote) -> UsbBcmVotes {
    let vectors = usb_bus_vectors(vote);
    let ebi = vectors[0];
    let ipa = vectors[1];
    let usb3 = vectors[2];
    UsbBcmVotes {
        count: 4,
        votes: [
            BcmVote {
                id: BCM_MC0,
                average_kbps: ebi.average_kbps,
                instantaneous_kbps: ebi.instantaneous_kbps,
            },
            BcmVote {
                id: BCM_ACV,
                average_kbps: ebi.average_kbps,
                instantaneous_kbps: ebi.instantaneous_kbps,
            },
            BcmVote {
                id: BCM_CN0,
                average_kbps: ipa.average_kbps.saturating_add(usb3.average_kbps),
                instantaneous_kbps: if ipa.instantaneous_kbps > usb3.instantaneous_kbps {
                    ipa.instantaneous_kbps
                } else {
                    usb3.instantaneous_kbps
                },
            },
            BcmVote {
                id: BCM_SH4,
                average_kbps: usb3.average_kbps,
                instantaneous_kbps: usb3.instantaneous_kbps,
            },
        ],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbRuntimeState {
    Off,
    Powered,
    Attached,
    Running,
    Suspended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbRuntimeEvent {
    PlatformPowered,
    TypecAttached,
    ControllerStarted,
    Disconnect,
    BusReset,
    Suspend,
    Resume,
}

/// Platform resource state corresponding to the resources Linux keeps in the
/// Qualcomm glue driver's private object.  The MMIO helpers below update this
/// only after a readback/transaction succeeds; callers can therefore
/// distinguish "the write was attempted" from "the resource is live".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbResourceState {
    pub gdsc_enabled: bool,
    pub clock_branches_enabled: bool,
    /// Bit 0..3 correspond to the four DT reset resources in `resets`.
    pub reset_released_mask: u8,
    pub bus_vote: Option<UsbBusVote>,
    pub irq_routes_enabled: bool,
    /// Lifetime state for the Linux regulator and PM QoS requests.
    pub power_rails_enabled: bool,
    pub power_super_speed: bool,
    pub pm_qos_active: bool,
    pub pm_qos_latency_us: u32,
    pub pm_qos_irq_affine: bool,
}

const EMPTY_USB_RESOURCE_STATE: UsbResourceState = UsbResourceState {
    gdsc_enabled: false,
    clock_branches_enabled: false,
    reset_released_mask: 0,
    bus_vote: None,
    irq_routes_enabled: false,
    power_rails_enabled: false,
    power_super_speed: false,
    pm_qos_active: false,
    pm_qos_latency_us: 0,
    pm_qos_irq_affine: false,
};

static mut USB_RESOURCE_STATE: UsbResourceState = EMPTY_USB_RESOURCE_STATE;

/// Return a snapshot of the platform lifecycle without exposing a mutable
/// reference to the early-boot singleton.
pub fn usb_resource_state() -> UsbResourceState {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(USB_RESOURCE_STATE)) }
}

fn set_usb_resource_state(update: impl FnOnce(&mut UsbResourceState)) {
    unsafe {
        let state = &mut *core::ptr::addr_of_mut!(USB_RESOURCE_STATE);
        update(state);
    }
}

/// The state transitions Linux spreads across the Qualcomm glue, PHY, UDC,
/// and gadget callbacks. Keeping the transition table explicit makes the
/// early-boot implementation reject invalid ordering instead of issuing a
/// DWC3 command while its platform power state is suspended.
pub const fn usb_runtime_transition(
    state: UsbRuntimeState,
    event: UsbRuntimeEvent,
) -> UsbRuntimeState {
    match (state, event) {
        (UsbRuntimeState::Off, UsbRuntimeEvent::PlatformPowered) => UsbRuntimeState::Powered,
        (UsbRuntimeState::Powered, UsbRuntimeEvent::TypecAttached) => UsbRuntimeState::Attached,
        (UsbRuntimeState::Attached, UsbRuntimeEvent::ControllerStarted) => UsbRuntimeState::Running,
        (UsbRuntimeState::Running, UsbRuntimeEvent::Suspend) => UsbRuntimeState::Suspended,
        (UsbRuntimeState::Suspended, UsbRuntimeEvent::Resume) => UsbRuntimeState::Running,
        (UsbRuntimeState::Running, UsbRuntimeEvent::BusReset) => UsbRuntimeState::Running,
        (_, UsbRuntimeEvent::Disconnect) => UsbRuntimeState::Off,
        (current, _) => current,
    }
}

/// Number of Apps-SMMU context-bank interrupt specifiers in the Bramble/Lito
/// DT.  The global fault SPI is stored separately; the DT lists 80 context
/// SPIs after it (97..118, 181..192, 183.. etc. in the provider's hardware
/// ordering).
pub const SMMU_CONTEXT_IRQ_COUNT: usize = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbPlatformResources {
    pub dwc3_base: usize,
    pub dwc3_size: usize,
    pub gcc_base: usize,
    pub qscratch_base: usize,
    pub hs_phy_base: usize,
    pub qmp_phy_base: usize,
    /// QMP combo-PHY register offsets from qcom,qmp-phy-reg-offset.  The
    /// array is fixed because the Android binding has a fixed 18-slot ABI.
    pub qmp_reg_offsets: [usize; 18],
    pub apps_smmu_base: usize,
    /// The Android Apps-SMMU DT sets qcom,use-3-lvl-tables.  This selects the
    /// 39-bit AArch64 IOVA aperture used when a fresh context is installed;
    /// it does not require every mapping to terminate at an L3 page entry.
    pub smmu_use_3_level_tables: bool,
    /// Apps-SMMU interrupt 0 is the global fault line; the remaining DT
    /// entries are context-bank fault lines.  Keep the context array bounded
    /// to the Lito DT contract so the early image can own the same IRQ route
    /// without allocating an irq-domain.
    pub smmu_global_irq: u32,
    pub smmu_context_irqs: [u32; SMMU_CONTEXT_IRQ_COUNT],
    pub smmu_context_irq_count: usize,
    pub pdc_base: usize,
    pub pdc_spi_cfg_base: usize,
    pub pdc_spi_cfg_size: usize,
    pub gdsc: usize,
    pub controller_clocks: &'static [ClockResource; 6],
    pub hs_phy_clock: ClockResource,
    pub qmp_clocks: &'static [ClockResource; 4],
    pub resets: &'static [ResetResource; 4],
    pub power: UsbPowerResources,
    pub irqs: [IrqResource; 5],
    pub typec_irq: SpmiIrqResource,
    pub spmi_parent_irq: u32,
    /// PM8150B `qcom,pm8150b-vbus-reg` base, if present in the DT.  The
    /// driver uses base+0x40/0x53 through the SPMI regmap.
    pub vbus_reg_base: Option<u16>,
    pub dma_pool: DmaPoolResource,
    pub gsi: GsiResource,
    pub rpmh_rsc: RpmhRscResource,
    pub command_db: CommandDbResource,
    /// DT values owned by the Qualcomm glue rather than GCC's static clock
    /// table. Keeping these in the active resource view makes a bootloader
    /// supplied DTB authoritative without requiring heap allocation.
    pub core_clk_rate_hz: u32,
    pub core_clk_rate_hs_hz: u32,
    pub pm_qos_latency_us: u32,
    pub bus_vote: BusVoteResource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbDtContract {
    pub dma_pool: Option<(u64, u64)>,
    pub stream_id: Option<u32>,
    pub smmu_use_3_level_tables: Option<bool>,
    pub smmu_global_irq: Option<u32>,
    pub smmu_context_irqs: [Option<u32>; SMMU_CONTEXT_IRQ_COUNT],
    pub qmp_reg_offsets: [Option<u32>; 18],
    pub core_clk_rate_hz: Option<u32>,
    pub core_clk_rate_hs_hz: Option<u32>,
    pub gsi_event_buffer_count: Option<u32>,
    pub gsi_reg_offsets: [Option<u32>; 6],
    pub gsi_disable_io_coherency: bool,
    pub pm_qos_latency_us: Option<u32>,
    pub bus_mode_count: Option<u32>,
    pub bus_path_count: Option<u32>,
    /// Flattened `vectors-KBps`: master, slave, average, peak for each of
    /// four modes and three paths. `None` means the property was not present
    /// or was too short to be installed safely.
    pub bus_vectors: [[Option<u32>; 4]; 12],
    /// IRQ numbers in the order of the active resource table:
    /// dp_hs_phy, pwr_event, ss_phy, dm_hs_phy, dwc3.
    pub irq_numbers: [Option<u32>; 5],
    /// The first four cells of the Type-C SPMI interrupt specifier and the
    /// SPMI arbiter's `periph_irq` parent SPI.
    pub typec_irq: [Option<u32>; 4],
    pub spmi_parent_irq: Option<u32>,
    /// Provider-local GCC/RPMh clock IDs in the order consumed by the
    /// compiled resource tables.  These are intentionally kept separate
    /// from physical offsets: the DT provider owns the ID-to-register map.
    pub controller_clock_ids: [Option<u32>; 6],
    pub qmp_clock_ids: [Option<u32>; 4],
    pub hs_phy_clock_id: Option<u32>,
    /// GCC reset IDs in the order consumed by `BRAMBLE_USB_RESETS`:
    /// controller, USB2 PHY, USB3 PHY, and USB3 DP PHY.
    pub reset_ids: [Option<u32>; 4],
    /// Raw `qcom,param-override-seq` cells: value, register offset for each
    /// entry. The Lito binding currently contains three entries.
    pub hs_param_override: [Option<u32>; 6],
    /// Raw QMP init cells: register offset, value, delay. Lito exposes 146
    /// writes followed by the all-ones sentinel, hence 147 triples.
    pub qmp_init_seq: [Option<u32>; 441],
    pub hs_vdd_voltage_level: [Option<u32>; 3],
    pub qmp_vdd_voltage_level: [Option<u32>; 3],
    pub qmp_vdd_max_load_ua: Option<u32>,
    pub qmp_core_voltage_level: [Option<u32>; 3],
    pub qmp_core_max_load_ua: Option<u32>,
    pub qmp_vbus_valid_override: Option<bool>,
    pub vbus_reg_base: Option<u32>,
    /// `USB3_GDSC-supply` resolves to the actual GDSC MMIO resource.
    pub gdsc: Option<(u64, u64)>,
    /// RPMh resource IDs obtained by following PHY supply phandles. Order:
    /// HS vdd, HS vdda18, HS vdda33, QMP vdd, QMP core.
    pub supply_resource_ids: [Option<[u8; 8]>; 5],
}

impl UsbDtContract {
    pub const fn empty() -> Self {
        Self {
            dma_pool: None,
            stream_id: None,
            smmu_use_3_level_tables: None,
            smmu_global_irq: None,
            smmu_context_irqs: [None; SMMU_CONTEXT_IRQ_COUNT],
            qmp_reg_offsets: [None; 18],
            core_clk_rate_hz: None,
            core_clk_rate_hs_hz: None,
            gsi_event_buffer_count: None,
            gsi_reg_offsets: [None; 6],
            gsi_disable_io_coherency: false,
            pm_qos_latency_us: None,
            bus_mode_count: None,
            bus_path_count: None,
            bus_vectors: [[None; 4]; 12],
            irq_numbers: [None; 5],
            typec_irq: [None; 4],
            spmi_parent_irq: None,
            controller_clock_ids: [None; 6],
            qmp_clock_ids: [None; 4],
            hs_phy_clock_id: None,
            reset_ids: [None; 4],
            hs_param_override: [None; 6],
            qmp_init_seq: [None; 441],
            hs_vdd_voltage_level: [None; 3],
            qmp_vdd_voltage_level: [None; 3],
            qmp_vdd_max_load_ua: None,
            qmp_core_voltage_level: [None; 3],
            qmp_core_max_load_ua: None,
            qmp_vbus_valid_override: None,
            vbus_reg_base: None,
            gdsc: None,
            supply_resource_ids: [None; 5],
        }
    }
}

/// Validate provider-local clock/reset IDs before accepting a DT-selected GCC
/// base.  A numeric ID is meaningful only under its matching provider and
/// silently pairing it with another SoC's map is equivalent to writing an
/// arbitrary clock/reset register.
pub fn usb_dt_clock_reset_contract_valid(contract: &UsbDtContract) -> bool {
    let has_ids = contract
        .controller_clock_ids
        .iter()
        .chain(contract.qmp_clock_ids.iter())
        .chain(contract.reset_ids.iter())
        .any(Option::is_some)
        || contract.hs_phy_clock_id.is_some();
    if !has_ids {
        return true;
    }

    let controller = [114, 9, 130, 116, 118, 119];
    let qmp = [120, 123, 0, 122];
    let resets = [13, 15, 17, 16];
    contract
        .controller_clock_ids
        .iter()
        .zip(controller)
        .all(|(actual, expected)| *actual == Some(expected))
        && contract
            .qmp_clock_ids
            .iter()
            .zip(qmp)
            .all(|(actual, expected)| *actual == Some(expected))
        && contract
            .reset_ids
            .iter()
            .zip(resets)
            .all(|(actual, expected)| *actual == Some(expected))
        && contract.hs_phy_clock_id.map(|id| id == 0).unwrap_or(true)
}

const BRAMBLE_CONTROLLER_CLOCKS: [ClockResource; 6] = [
    ClockResource {
        name: "core",
        provider: ClockProvider::Gcc,
        provider_id: 114,
        branch_offset: 0xf010,
        source_offset: 0xf020,
        normal_rate_hz: 133_333_333,
        high_speed_rate_hz: 66_666_667,
    },
    ClockResource {
        name: "iface",
        provider: ClockProvider::Gcc,
        provider_id: 9,
        branch_offset: 0xf07c,
        source_offset: 0,
        normal_rate_hz: 0,
        high_speed_rate_hz: 0,
    },
    ClockResource {
        name: "bus_aggr",
        provider: ClockProvider::Gcc,
        provider_id: 130,
        branch_offset: 0xf080,
        source_offset: 0,
        normal_rate_hz: 0,
        high_speed_rate_hz: 0,
    },
    ClockResource {
        name: "utmi",
        provider: ClockProvider::Gcc,
        provider_id: 116,
        branch_offset: 0xf01c,
        source_offset: 0xf038,
        // gcc_usb30_prim_mock_utmi_clk_src has one supported rate:
        // BI_TCXO / 1 = 19.2 MHz.  The 60 MHz path is not in the
        // Bramble Lito GCC rate table and is retained only as an explicit
        // negative-control build option.
        normal_rate_hz: 19_200_000,
        high_speed_rate_hz: 19_200_000,
    },
    ClockResource {
        name: "sleep",
        provider: ClockProvider::Gcc,
        provider_id: 118,
        branch_offset: 0xf018,
        source_offset: 0,
        normal_rate_hz: 0,
        high_speed_rate_hz: 0,
    },
    ClockResource {
        name: "xo",
        provider: ClockProvider::Gcc,
        provider_id: 119,
        branch_offset: 0x8c010,
        source_offset: 0,
        normal_rate_hz: 19_200_000,
        high_speed_rate_hz: 19_200_000,
    },
];

const BRAMBLE_QMP_CLOCKS: [ClockResource; 4] = [
    ClockResource {
        name: "aux",
        provider: ClockProvider::Gcc,
        provider_id: 120,
        branch_offset: 0xf054,
        // gcc_usb3_prim_phy_aux_clk_src, 19.2 MHz from BI_TCXO.
        source_offset: 0xf064,
        normal_rate_hz: 19_200_000,
        high_speed_rate_hz: 19_200_000,
    },
    ClockResource {
        name: "pipe",
        provider: ClockProvider::Gcc,
        provider_id: 123,
        branch_offset: 0xf05c,
        source_offset: 0,
        normal_rate_hz: 125_000_000,
        high_speed_rate_hz: 125_000_000,
    },
    ClockResource {
        name: "ref",
        provider: ClockProvider::Rpmh,
        provider_id: 0,
        branch_offset: 0,
        source_offset: 0,
        normal_rate_hz: 19_200_000,
        high_speed_rate_hz: 19_200_000,
    },
    ClockResource {
        name: "com_aux",
        provider: ClockProvider::Gcc,
        provider_id: 122,
        branch_offset: 0xf058,
        source_offset: 0,
        normal_rate_hz: 19_200_000,
        high_speed_rate_hz: 19_200_000,
    },
];

const BRAMBLE_HS_PHY_CLOCK: ClockResource = ClockResource {
    name: "ref_clk_src",
    provider: ClockProvider::Rpmh,
    provider_id: 0,
    branch_offset: 0,
    source_offset: 0,
    normal_rate_hz: 19_200_000,
    high_speed_rate_hz: 19_200_000,
};

// qcom,lito-qmp-usb3.h / lito-usb.dtsi, in the exact order consumed by the
// Android msm-ssusb-qmp driver. Slot 6 is intentionally 0xffff because the
// combo-PHY does not expose the USB-only PCS_MISC_TYPEC_CTRL register.
const BRAMBLE_QMP_REG_OFFSETS: [usize; 18] = [
    0x1c14, 0x1f08, 0x1f14, 0x1c40, 0x1c00, 0x1c44, 0xffff, 0x2a18, 0x0008, 0x0004, 0x001c, 0x0000,
    0x0010, 0x000c, 0x1c8c, 0x1c18, 0x1c50, 0x1c70,
];

const BRAMBLE_USB_RESETS: [ResetResource; 4] = [
    ResetResource {
        name: "core_reset",
        provider_id: 13,
        offset: 0xf000,
    },
    ResetResource {
        name: "qusb2phy_reset",
        provider_id: 15,
        offset: 0x12000,
    },
    ResetResource {
        name: "usb3phy_reset",
        provider_id: 17,
        offset: 0x50000,
    },
    ResetResource {
        name: "usb3dp_phy_reset",
        provider_id: 16,
        offset: 0x50008,
    },
];

// Regulator resources declared by lito-usb.dtsi. The PM8150B rails are
// controlled by the RPMh VRM. The IDs below are only fallback names; when a
// DTB supplies the regulator phandles they are replaced by the referenced
// `regulator-name` mapping before any request is sent.
const fn rpmh_id(bytes: &[u8]) -> [u8; 8] {
    let mut id = [0; 8];
    let mut index = 0;
    while index < bytes.len() && index < id.len() {
        id[index] = bytes[index];
        index += 1;
    }
    id
}

const RPMH_LDOA5: [u8; 8] = rpmh_id(b"ldoa5");
const RPMH_LDOA12: [u8; 8] = rpmh_id(b"ldoa12");
const RPMH_LDOA2: [u8; 8] = rpmh_id(b"ldoa2");
const RPMH_LDOA9: [u8; 8] = rpmh_id(b"ldoa9");
const RPMH_LDOA18: [u8; 8] = rpmh_id(b"ldoa18");
/// The `rpmh-regulator-cxlvl` resource consumed by the GCC block
/// (`vdd_cx-supply`/`vdd_cx_ao-supply` on the `qcom,gcc@100000` node).  Every
/// USB clock branch and the USB30 GDSC live inside the CX corner domain, so
/// the ARC vote is part of the USB gadget power contract even though no USB
/// node references it directly.
const RPMH_CXLVL: [u8; 8] = rpmh_id(b"cx.lvl");
/// Lito's RPMh `bi_tcxo`/`bi_tcxo_ao` ARC clock resource. The Android USB2
/// PHY consumes `RPMH_CXO_CLK` as `ref_clk_src`; unlike the GCC mock-UTMI
/// branch, this fixed 19.2 MHz source has no local MMIO gate.
const RPMH_XO_LVL: [u8; 8] = rpmh_id(b"xo.lvl");
const RPMH_XO_LVL_ON: u32 = 0x3;

/// ARC levels from `dt-bindings/regulator/qcom,rpmh-regulator-levels.h` as
/// mapped by the lito `vdd_corner` table (`vdd-level-lito.h`):
/// VDD_LOWER = LOW_SVS and VDD_LOW = SVS.
pub const RPMH_REGULATOR_LEVEL_LOW_SVS: u32 = 64;
pub const RPMH_REGULATOR_LEVEL_SVS: u32 = 128;

/// CX corner required for the USB clock plan of one bus vote. The lito GCC
/// driver's rate tables are the binding between `qcom,core-clk-rate` and the
/// CX corner: `gcc_usb30_prim_master_clk_src` allows 133333333 Hz only from
/// VDD_LOW (SVS), while the USB AUX 19.2 MHz source requires VDD_LOWER
/// (LOW_SVS). The mock-UTMI source is the separate 60 MHz GPLL path. The
/// winning vote is therefore SVS for the nominal controller rate and LOW_SVS
/// for the HS/gated states.
pub const fn usb_cx_level(vote: UsbBusVote) -> u32 {
    match vote {
        UsbBusVote::Nominal => RPMH_REGULATOR_LEVEL_SVS,
        UsbBusVote::Svs | UsbBusVote::Suspend | UsbBusVote::Minimum => RPMH_REGULATOR_LEVEL_LOW_SVS,
    }
}

const BRAMBLE_HS_PHY_RAILS: [UsbRailResource; 3] = [
    UsbRailResource {
        name: "vdd",
        rpmh_resource_id: RPMH_LDOA5,
        min_uv: 880_000,
        max_uv: 880_000,
        // lito-regulators.dtsi gives ldoa5 `qcom,mode-threshold-currents =
        // <0 1>`: any nonzero consumer load requires HPM. The DT's
        // `qcom,init-mode` is only the firmware boot state; the PHY's
        // 0.88 V rail must run HPM while the UTMI interface is live.
        max_load_ua: 19_000,
        program_voltage: true,
        owner: PowerOwner::SecureFirmware,
    },
    UsbRailResource {
        name: "vdda18",
        rpmh_resource_id: RPMH_LDOA12,
        min_uv: 1_704_000,
        max_uv: 1_800_000,
        max_load_ua: 19_000,
        program_voltage: false,
        owner: PowerOwner::SecureFirmware,
    },
    UsbRailResource {
        name: "vdda33",
        rpmh_resource_id: RPMH_LDOA2,
        min_uv: 3_050_000,
        max_uv: 3_300_000,
        max_load_ua: 16_000,
        program_voltage: false,
        owner: PowerOwner::SecureFirmware,
    },
];

const BRAMBLE_QMP_CORE_RAIL: UsbRailResource = UsbRailResource {
    name: "core",
    rpmh_resource_id: RPMH_LDOA9,
    min_uv: 1_200_000,
    max_uv: 1_200_000,
    max_load_ua: 30_000,
    program_voltage: true,
    owner: PowerOwner::SecureFirmware,
};

// The QMP driver has separate `vdd` and `core` consumers. The Bramble USB DT
// names the core consumer as pm8150_l9; vdd is optional in the final board
// tree, so this slot is inert unless a DT supply is explicitly resolved.
const BRAMBLE_QMP_VDD_RAIL: UsbRailResource = UsbRailResource {
    name: "vdd",
    rpmh_resource_id: RPMH_LDOA18,
    min_uv: 0,
    max_uv: 0,
    max_load_ua: 0,
    program_voltage: true,
    owner: PowerOwner::SecureFirmware,
};

const BRAMBLE_USB_POWER: UsbPowerResources = UsbPowerResources {
    hs_phy_rails: BRAMBLE_HS_PHY_RAILS,
    qmp_vdd: BRAMBLE_QMP_VDD_RAIL,
    qmp_vdd_present: false,
    qmp_core: BRAMBLE_QMP_CORE_RAIL,
    qmp_vbus_valid_override: true,
};

/// Apps-SMMU DT interrupt order after the single global fault SPI.  The
/// context-bank list is sparse in SPI number space but dense in the SMMU
/// provider's `IRPTNDX` order.
const BRAMBLE_SMMU_CONTEXT_IRQS: [u32; SMMU_CONTEXT_IRQ_COUNT] = [
    97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115,
    116, 117, 118, 181, 182, 183, 184, 179, 186, 187, 188, 189, 190, 191, 192, 315, 316, 317, 318,
    319, 320, 321, 322, 323, 324, 325, 326, 327, 328, 329, 330, 331, 332, 333, 334, 335, 336, 337,
    338, 339, 340, 341, 342, 343, 344, 345, 395, 396, 397, 398, 399, 400, 401, 402, 403, 404, 405,
    406, 407, 408, 409,
];

const BRAMBLE_USB_IRQS: [IrqResource; 5] = [
    IrqResource {
        name: "dp_hs_phy_irq",
        number: USB_PDC_DP_HS_PHY_IRQ,
        kind: UsbIrqKind::Pdc,
        trigger: IrqTrigger::RisingEdge,
    },
    IrqResource {
        name: "pwr_event_irq",
        number: USB_PWR_EVENT_IRQ,
        kind: UsbIrqKind::GicSpi,
        trigger: IrqTrigger::LevelHigh,
    },
    IrqResource {
        name: "ss_phy_irq",
        number: USB_PDC_SS_PHY_IRQ,
        kind: UsbIrqKind::Pdc,
        trigger: IrqTrigger::LevelHigh,
    },
    IrqResource {
        name: "dm_hs_phy_irq",
        number: USB_PDC_DM_HS_PHY_IRQ,
        kind: UsbIrqKind::Pdc,
        trigger: IrqTrigger::RisingEdge,
    },
    IrqResource {
        name: "dwc3",
        number: USB_DWC3_IRQ,
        kind: UsbIrqKind::GicSpi,
        trigger: IrqTrigger::LevelHigh,
    },
];

const BRAMBLE_TYPEC_IRQ: SpmiIrqResource = SpmiIrqResource {
    sid: PM8150B_SID,
    peripheral_id: (PM8150B_TYPEC_PPID & 0xff) as u8,
    irq: 0,
    trigger: IrqTrigger::RisingEdge,
};

const BRAMBLE_GSI: GsiResource = GsiResource {
    event_buffer_count: 3,
    general_cfg_offset: 0x0fc,
    doorbell_low_offset: 0x110,
    doorbell_high_offset: 0x120,
    ring_base_low_offset: 0x130,
    ring_base_high_offset: 0x144,
    interface_status_offset: 0x1a4,
    disable_io_coherency: true,
};

// Apps RSC/Command DB resources from the Android Lito base DT.  The Apps RSC
// is driver 2, so the active vote path is the third 0x10000 register window.
// TCS type ordering is ACTIVE(2), SLEEP(3), WAKE(3), CONTROL(1), giving the
// global offsets 0, 2, 5, and 8 respectively.
const BRAMBLE_RPMH_RSC: RpmhRscResource = RpmhRscResource {
    driver_base: 0x1822_0000,
    tcs_offset: 0x0d00,
    driver_id: 2,
    active_tcs: 2,
    sleep_tcs: 3,
    wake_tcs: 3,
    control_tcs: 1,
    active_tcs_offset: 0,
    sleep_tcs_offset: 2,
    wake_tcs_offset: 5,
};

const BRAMBLE_COMMAND_DB: CommandDbResource = CommandDbResource {
    base: 0x8086_0000,
    size: 0x20_000,
};

// Values from qcom/lito-usb.dtsi. The old msm-bus client expresses these
// votes in KBps; preserving all four use cases is necessary for runtime PM.
const BRAMBLE_BUS_VECTORS: [[BusVoteVector; 3]; 4] = [
    [
        BusVoteVector {
            master: 61,
            slave: 512,
            average_kbps: 0,
            instantaneous_kbps: 0,
        },
        BusVoteVector {
            master: 61,
            slave: 676,
            average_kbps: 0,
            instantaneous_kbps: 0,
        },
        BusVoteVector {
            master: 1,
            slave: 583,
            average_kbps: 0,
            instantaneous_kbps: 0,
        },
    ],
    [
        BusVoteVector {
            master: 61,
            slave: 512,
            average_kbps: 1_000_000,
            instantaneous_kbps: 2_500_000,
        },
        BusVoteVector {
            master: 61,
            slave: 676,
            average_kbps: 0,
            instantaneous_kbps: 2_400,
        },
        BusVoteVector {
            master: 1,
            slave: 583,
            average_kbps: 0,
            instantaneous_kbps: 40_000,
        },
    ],
    [
        BusVoteVector {
            master: 61,
            slave: 512,
            average_kbps: 240_000,
            instantaneous_kbps: 700_000,
        },
        BusVoteVector {
            master: 61,
            slave: 676,
            average_kbps: 0,
            instantaneous_kbps: 2_400,
        },
        BusVoteVector {
            master: 1,
            slave: 583,
            average_kbps: 0,
            instantaneous_kbps: 40_000,
        },
    ],
    [
        BusVoteVector {
            master: 61,
            slave: 512,
            average_kbps: 1,
            instantaneous_kbps: 1,
        },
        BusVoteVector {
            master: 61,
            slave: 676,
            average_kbps: 1,
            instantaneous_kbps: 1,
        },
        BusVoteVector {
            master: 1,
            slave: 583,
            average_kbps: 1,
            instantaneous_kbps: 1,
        },
    ],
];

/// Compiled fallback for a boot without an FDT. When an Android DTB is
/// supplied, `install_usb_resource_contract` applies validated resource
/// overrides to this contract before the USB client consumes it; platform
/// users must not grow another hard-coded MMIO list.
pub const BRAMBLE_USB_RESOURCES: UsbPlatformResources = UsbPlatformResources {
    dwc3_base: 0x0a60_0000,
    dwc3_size: 0xcd00,
    gcc_base: GCC_BASE,
    qscratch_base: 0x0a6f_8800,
    hs_phy_base: 0x088e_3000,
    qmp_phy_base: 0x088e_8000,
    qmp_reg_offsets: BRAMBLE_QMP_REG_OFFSETS,
    // apps_smmu: qcom,qsmmu-v500 at 0x15000000 in msm-arm-smmu-lito.dtsi.
    // 0x0c600000 belongs to the SPMI arbiter channel window.
    apps_smmu_base: 0x1500_0000,
    smmu_use_3_level_tables: true,
    smmu_global_irq: 65,
    smmu_context_irqs: BRAMBLE_SMMU_CONTEXT_IRQS,
    smmu_context_irq_count: SMMU_CONTEXT_IRQ_COUNT,
    pdc_base: USB_PDC_BASE,
    pdc_spi_cfg_base: USB_PDC_SPI_CFG_BASE,
    pdc_spi_cfg_size: USB_PDC_SPI_CFG_SIZE,
    gdsc: USB30_PRIM_GDSC,
    controller_clocks: &BRAMBLE_CONTROLLER_CLOCKS,
    hs_phy_clock: BRAMBLE_HS_PHY_CLOCK,
    qmp_clocks: &BRAMBLE_QMP_CLOCKS,
    resets: &BRAMBLE_USB_RESETS,
    power: BRAMBLE_USB_POWER,
    irqs: BRAMBLE_USB_IRQS,
    typec_irq: BRAMBLE_TYPEC_IRQ,
    // qcom,spmi-pmic-arb's `periph_irq` summary line in Kona/Lito DT.
    spmi_parent_irq: 481,
    vbus_reg_base: None,
    dma_pool: DmaPoolResource {
        iova_base: 0x9000_0000,
        size: 0x6000_0000,
        stream_id: 0xe0,
    },
    gsi: BRAMBLE_GSI,
    rpmh_rsc: BRAMBLE_RPMH_RSC,
    command_db: BRAMBLE_COMMAND_DB,
    core_clk_rate_hz: 133_333_333,
    core_clk_rate_hs_hz: 66_666_667,
    pm_qos_latency_us: 61,
    bus_vote: BusVoteResource {
        mode_count: 4,
        path_count: 3,
        vectors: BRAMBLE_BUS_VECTORS,
    },
};

static mut ACTIVE_USB_RESOURCES: UsbPlatformResources = BRAMBLE_USB_RESOURCES;

/// Second `reg` resource of Bramble's `qcom,usb-hsphy-snps-femto` node.
/// `phy-msm-snps-hs.c` treats a non-zero value here as an EUD-owned PHY and
/// returns from `msm_hsphy_init()` before changing its power or analog state.
const BRAMBLE_USB_HS_PHY_EUD_BASE: usize = 0x088e_2000;
static mut ACTIVE_USB_HS_PHY_EUD_BASE: usize = BRAMBLE_USB_HS_PHY_EUD_BASE;

/// Install the DT-selected HS-PHY EUD status resource. The compiled Bramble
/// address remains the fallback when the bootloader does not pass a DTB.
pub fn install_usb_hs_phy_eud_base(base: Option<u64>) -> bool {
    let Some(base) = base else {
        return false;
    };
    if base == 0 || base > usize::MAX as u64 {
        return false;
    }
    unsafe {
        ACTIVE_USB_HS_PHY_EUD_BASE = base as usize;
    }
    true
}

/// Read the optional EUD ownership gate used by the Android HS-PHY driver.
/// This is a read-only status check; it does not synthesize or modify any PHY
/// register address.
pub unsafe fn usb_hs_phy_eud_enabled() -> bool {
    let base = unsafe { ACTIVE_USB_HS_PHY_EUD_BASE };
    unsafe { core::ptr::read_volatile(base as *const u32) != 0 }
}

/// Replace the compiled GCC provider base with the DT node selected for this
/// boot. Clock IDs/branch offsets remain provider-specific, but the provider
/// MMIO base itself is a normal DT resource and must not be hard-coded in the
/// runtime path.
pub fn install_usb_gcc_base(base: Option<u64>) -> bool {
    let Some(base) = base else {
        return false;
    };
    if base > usize::MAX as u64 {
        return false;
    }
    unsafe {
        (*core::ptr::addr_of_mut!(ACTIVE_USB_RESOURCES)).gcc_base = base as usize;
    }
    true
}

/// Return the resource view selected for the current boot.  It starts with
/// the compiled Bramble contract and may be replaced by validated DT values
/// before any USB register is touched.
pub fn usb_resources() -> UsbPlatformResources {
    unsafe { ACTIVE_USB_RESOURCES }
}

/// Validate the DT-described rail contract before touching PHY registers.
/// This is intentionally a contract check, not a fabricated PMIC driver:
/// PM8150B regulator enable/voltage changes belong to RPMh/secure firmware.
pub fn usb_power_contract_valid(super_speed: bool) -> bool {
    let power = usb_resources().power;
    let hs_valid = power.hs_phy_rails.iter().all(|rail| {
        (!rail.program_voltage || (rail.min_uv != 0 && rail.min_uv <= rail.max_uv))
            && rail.owner == PowerOwner::SecureFirmware
    });
    let qmp_valid = !super_speed
        || (power.qmp_core.min_uv != 0
            && power.qmp_core.min_uv <= power.qmp_core.max_uv
            && power.qmp_core.max_load_ua != 0
            && power.qmp_core.owner == PowerOwner::SecureFirmware
            && power.qmp_vbus_valid_override);
    let qmp_vdd_valid = !power.qmp_vdd_present
        || (power.qmp_vdd.min_uv != 0
            && power.qmp_vdd.min_uv <= power.qmp_vdd.max_uv
            && power.qmp_vdd.max_load_ua != 0
            && power.qmp_vdd.owner == PowerOwner::SecureFirmware);
    hs_valid && qmp_valid && qmp_vdd_valid
}

/// Convert the regulator node names used by the Android DT into RPMh VRM
/// resource identifiers.  The mapping is intentionally strict: an unknown
/// PMIC instance is rejected, so a DT overlay can never redirect a vote to a
/// different regulator merely because its suffix happens to match.
pub fn rpmh_resource_id_from_regulator_name(bytes: &[u8], len: usize) -> Option<[u8; 8]> {
    let name = bytes.get(..len)?;
    match name {
        b"pm8150_l5" | b"pm8150_l5_level" => Some(RPMH_LDOA5),
        b"pm8150_l12" => Some(RPMH_LDOA12),
        b"pm8150_l2" => Some(RPMH_LDOA2),
        b"pm8150_l18" => Some(RPMH_LDOA18),
        b"pm8150b_l5" => Some(rpmh_id(b"ldob5")),
        b"pm8150b_l12" => Some(rpmh_id(b"ldob12")),
        b"pm8150b_l2" => Some(rpmh_id(b"ldob2")),
        b"pm8150_l9" => Some(RPMH_LDOA9),
        _ => None,
    }
}

/// Install the address-bearing portion of the Qualcomm USB DT contract. The
/// clock/reset/IRQ tables remain compiled Lito tables because their GCC and
/// PDC specifier values are provider-local rather than physical address
/// tuples. The AArch64 entry path validates those provider IDs before it
/// accepts the DT-selected GCC base; invalid or absent tuples preserve the
/// safe fallback.
pub fn install_usb_resource_overrides(
    dwc3: Option<(u64, u64)>,
    hs_phy: Option<(u64, u64)>,
    qmp_phy: Option<(u64, u64)>,
    apps_smmu: Option<(u64, u64)>,
    pdc: Option<(u64, u64)>,
) -> bool {
    install_usb_resource_contract(
        dwc3,
        hs_phy,
        qmp_phy,
        apps_smmu,
        pdc,
        UsbDtContract::empty(),
    )
}

/// Install all DT-owned values that the Android Qualcomm glue consumes. The
/// legacy five-argument wrapper above remains useful for callers that only
/// have `reg` tuples; the AArch64 entry path uses this complete contract.
pub fn install_usb_resource_contract(
    dwc3: Option<(u64, u64)>,
    hs_phy: Option<(u64, u64)>,
    qmp_phy: Option<(u64, u64)>,
    apps_smmu: Option<(u64, u64)>,
    pdc: Option<(u64, u64)>,
    contract: UsbDtContract,
) -> bool {
    let mut installed = false;
    unsafe {
        let resources = &mut *core::ptr::addr_of_mut!(ACTIVE_USB_RESOURCES);
        if let Some((base, size)) = dwc3 {
            if base <= usize::MAX as u64
                && size != 0
                && size <= usize::MAX as u64
                && base.checked_add(size).is_some()
            {
                resources.dwc3_base = base as usize;
                resources.dwc3_size = size as usize;
                resources.qscratch_base = resources.dwc3_base + 0xf8_800;
                installed = true;
            }
        }
        for (slot, value) in [
            (&mut resources.hs_phy_base, hs_phy),
            (&mut resources.qmp_phy_base, qmp_phy),
            (&mut resources.apps_smmu_base, apps_smmu),
            (&mut resources.pdc_base, pdc),
        ] {
            if let Some((base, size)) = value {
                if base <= usize::MAX as u64 && size != 0 {
                    *slot = base as usize;
                    installed = true;
                }
            }
        }

        if let Some((base, size)) = contract.dma_pool {
            if base <= usize::MAX as u64
                && size != 0
                && size <= usize::MAX as u64
                && base.checked_add(size).is_some()
            {
                resources.dma_pool.iova_base = base as usize;
                resources.dma_pool.size = size as usize;
                installed = true;
            }
        }
        if let Some(stream_id) = contract.stream_id {
            // The Qualcomm stream ID is a 20-bit SMMU SID. Rejecting a
            // malformed cell is safer than installing an arbitrary context.
            if stream_id < (1 << 20) {
                resources.dma_pool.stream_id = stream_id;
                installed = true;
            }
        }
        if let Some(use_3_level_tables) = contract.smmu_use_3_level_tables {
            resources.smmu_use_3_level_tables = use_3_level_tables;
            installed = true;
        }
        if let Some(irq) = contract.smmu_global_irq {
            if (32..1020).contains(&irq) {
                resources.smmu_global_irq = irq;
                installed = true;
            }
        }
        let mut smmu_context_irqs = [0u32; SMMU_CONTEXT_IRQ_COUNT];
        let mut smmu_context_irqs_valid = true;
        for (slot, irq) in contract.smmu_context_irqs.iter().enumerate() {
            let Some(irq) = irq else {
                smmu_context_irqs_valid = false;
                break;
            };
            if !(32..1020).contains(irq) {
                smmu_context_irqs_valid = false;
                break;
            }
            smmu_context_irqs[slot] = *irq;
        }
        if smmu_context_irqs_valid {
            resources.smmu_context_irqs = smmu_context_irqs;
            resources.smmu_context_irq_count = SMMU_CONTEXT_IRQ_COUNT;
            installed = true;
        }
        if contract.qmp_reg_offsets.iter().all(Option::is_some) {
            let offsets = contract.qmp_reg_offsets.map(Option::unwrap);
            // 0xffff is the binding's explicit "not implemented" slot;
            // reject only offsets outside the 16-bit QMP register window.
            if offsets.iter().all(|offset| *offset <= 0xffff) {
                resources.qmp_reg_offsets = offsets.map(|offset| offset as usize);
                installed = true;
            }
        }
        if let [Some(_), Some(min_uv), Some(max_uv)] = contract.hs_vdd_voltage_level {
            if min_uv != 0 && min_uv <= max_uv {
                resources.power.hs_phy_rails[0].min_uv = min_uv;
                resources.power.hs_phy_rails[0].max_uv = max_uv;
                installed = true;
            }
        }
        if let [Some(_), Some(min_uv), Some(max_uv)] = contract.qmp_vdd_voltage_level {
            if min_uv != 0 && min_uv <= max_uv {
                resources.power.qmp_vdd.min_uv = min_uv;
                resources.power.qmp_vdd.max_uv = max_uv;
                installed = true;
            }
        }
        if let Some(max_load_ua) = contract.qmp_vdd_max_load_ua {
            if max_load_ua != 0 {
                resources.power.qmp_vdd.max_load_ua = max_load_ua;
                installed = true;
            }
        }
        if let [Some(_), Some(min_uv), Some(max_uv)] = contract.qmp_core_voltage_level {
            if min_uv != 0 && min_uv <= max_uv {
                resources.power.qmp_core.min_uv = min_uv;
                resources.power.qmp_core.max_uv = max_uv;
                installed = true;
            }
        }
        if let Some(max_load_ua) = contract.qmp_core_max_load_ua {
            if max_load_ua != 0 {
                resources.power.qmp_core.max_load_ua = max_load_ua;
                installed = true;
            }
        }
        if let Some(vbus_override) = contract.qmp_vbus_valid_override {
            resources.power.qmp_vbus_valid_override = vbus_override;
            installed = true;
        }
        if let Some(base) = contract.vbus_reg_base {
            if base <= u16::MAX as u32 {
                resources.vbus_reg_base = Some(base as u16);
                installed = true;
            }
        }
        if let Some((base, size)) = contract.gdsc {
            if base <= usize::MAX as u64
                && size == core::mem::size_of::<u32>() as u64
                && base.checked_add(size).is_some()
            {
                resources.gdsc = base as usize;
                installed = true;
            }
        }
        if let Some(resource_id) = contract.supply_resource_ids[0] {
            resources.power.hs_phy_rails[0].rpmh_resource_id = resource_id;
            resources.power.qmp_vdd.rpmh_resource_id = resource_id;
            installed = true;
        }
        if let Some(resource_id) = contract.supply_resource_ids[1] {
            resources.power.hs_phy_rails[1].rpmh_resource_id = resource_id;
            installed = true;
        }
        if let Some(resource_id) = contract.supply_resource_ids[2] {
            resources.power.hs_phy_rails[2].rpmh_resource_id = resource_id;
            installed = true;
        }
        if let Some(resource_id) = contract.supply_resource_ids[3] {
            resources.power.qmp_vdd.rpmh_resource_id = resource_id;
            resources.power.qmp_vdd_present = true;
            installed = true;
        }
        if let Some(resource_id) = contract.supply_resource_ids[4] {
            resources.power.qmp_core.rpmh_resource_id = resource_id;
            installed = true;
        }
        if let Some(rate) = contract.core_clk_rate_hz {
            if rate != 0 {
                resources.core_clk_rate_hz = rate;
                installed = true;
            }
        }
        if let Some(rate) = contract.core_clk_rate_hs_hz {
            if rate != 0 {
                resources.core_clk_rate_hs_hz = rate;
                installed = true;
            }
        }
        for (irq, number) in resources.irqs.iter_mut().zip(contract.irq_numbers) {
            if let Some(number) = number {
                if number < 1024 {
                    irq.number = number;
                    installed = true;
                }
            }
        }
        if let ([Some(sid), Some(peripheral_id), Some(irq), Some(flags)], Some(parent)) =
            (contract.typec_irq, contract.spmi_parent_irq)
        {
            // The Android SPMI binding is <sid peripheral irq flags>.  This
            // early path supports the same edge-rising primary interrupt as
            // qcom-pmic-typec; reject other encodings instead of installing
            // a resource with an incorrect PMIC polarity.
            let trigger = match flags & 0xf {
                1 => Some(IrqTrigger::RisingEdge),
                3 => Some(IrqTrigger::EdgeBoth),
                4 => Some(IrqTrigger::LevelHigh),
                _ => None,
            };
            if sid < 16
                && peripheral_id < 256
                && irq < 8
                && parent < 1024
                && let Some(trigger) = trigger
            {
                resources.typec_irq = SpmiIrqResource {
                    sid: sid as u8,
                    peripheral_id: peripheral_id as u8,
                    irq: irq as u8,
                    trigger,
                };
                resources.spmi_parent_irq = parent;
                installed = true;
            }
        }
        if let Some(count) = contract.gsi_event_buffer_count {
            if (1..=3).contains(&count) {
                resources.gsi.event_buffer_count = count;
                installed = true;
            }
        }
        if contract.gsi_reg_offsets.iter().all(Option::is_some) {
            let offsets = contract.gsi_reg_offsets.map(Option::unwrap);
            resources.gsi.general_cfg_offset = offsets[0] as usize;
            resources.gsi.doorbell_low_offset = offsets[1] as usize;
            resources.gsi.doorbell_high_offset = offsets[2] as usize;
            resources.gsi.ring_base_low_offset = offsets[3] as usize;
            resources.gsi.ring_base_high_offset = offsets[4] as usize;
            resources.gsi.interface_status_offset = offsets[5] as usize;
            resources.gsi.disable_io_coherency = contract.gsi_disable_io_coherency;
            installed = true;
        }
        if let Some(latency) = contract.pm_qos_latency_us {
            resources.pm_qos_latency_us = latency;
            installed = true;
        }
        // Bus vectors are provider-specific constants in this early path, but
        // the DT still declares their dimensions. Do not claim a match when a
        // different topology is supplied; it would make the compiled vector
        // table semantically unsafe.
        if let (Some(modes), Some(paths)) = (contract.bus_mode_count, contract.bus_path_count) {
            if modes == resources.bus_vote.mode_count as u32
                && paths == resources.bus_vote.path_count as u32
            {
                if contract
                    .bus_vectors
                    .iter()
                    .all(|vector| vector.iter().all(Option::is_some))
                {
                    let mut vectors = resources.bus_vote.vectors;
                    for flat in 0..12 {
                        let mode = flat / 3;
                        let path = flat % 3;
                        let values = contract.bus_vectors[flat];
                        vectors[mode][path] = BusVoteVector {
                            master: values[0].unwrap(),
                            slave: values[1].unwrap(),
                            average_kbps: values[2].unwrap(),
                            instantaneous_kbps: values[3].unwrap(),
                        };
                    }
                    resources.bus_vote.vectors = vectors;
                }
                installed = true;
            }
        }
    }
    installed
}
const GDSC_PWR_ON: u32 = 1 << 31;
const GDSC_HW_CONTROL: u32 = 1 << 1;
const GDSC_SW_OVERRIDE: u32 = 1 << 2;
const GDSC_SW_COLLAPSE: u32 = 1 << 0;
const GDSC_WAIT_MASK: u32 = (0xf << 20) | (0xf << 16) | (0xf << 12);
const GDSC_WAIT_VALUE: u32 = (0x2 << 20) | (0x8 << 16) | (0x2 << 12);

// Lito's SPMI arbiter.  The Pixel DT exposes the five resources below under
// qcom,spmi@c440000; the arbiter is the only path from the Apps CPU to the
// PM8150B Type-C block.
const SPMI_CORE: usize = 0x0c44_0000;
const SPMI_CHANNELS: usize = 0x0c60_0000;
const SPMI_OBSERVER: usize = 0x0e60_0000;
const SPMI_CONFIG: usize = 0x0c40_a000;
const SPMI_VERSION: usize = 0x0000;
const SPMI_APID_MAP_V5: usize = 0x0900;
const SPMI_MAPPING_TABLE: usize = 0x0b00;
const SPMI_OWNERSHIP_TABLE: usize = 0x0700;
const SPMI_STATUS: usize = 0x08;
const SPMI_WDATA0: usize = 0x10;
const SPMI_RDATA0: usize = 0x18;
const SPMI_STATUS_DONE: u32 = 1 << 0;
const SPMI_STATUS_FAILURE: u32 = 1 << 1;
const SPMI_STATUS_DENIED: u32 = 1 << 2;
const SPMI_STATUS_DROPPED: u32 = 1 << 3;
const SPMI_OP_EXT_WRITEL: u32 = 0;
const SPMI_OP_EXT_READL: u32 = 1;
const SPMI_EE: usize = 0;
const PM8150B_SID: u8 = 2;
const PM8150B_TYPEC_PPID: u16 = ((PM8150B_SID as u16) << 8) | 0x15;
const PM8150B_TYPEC_BASE: u16 = 0x1500;
// The primary PON peripheral is SID 0/peripheral 8. Its power-off reason
// survives the automatic recovery reset and can be read before the USB
// handoff to publish the previous reset's cause through attach delay.
const PM8150_PON_PPID: u16 = 0x0008;
const PON_SUBTYPE: u16 = 0x05;
const PON_OFF_SEQ: u16 = 0xc7;
const PON_REASON1: u16 = 0xc0;
pub const PON_WARM_RESET_REASON1: u16 = 0xc2;
pub const PON_SOFT_RESET_REASON1: u16 = 0xcb;
const PON_POFF_REASON1: u16 = 0xc5;
const PON_POFF_REASON2: u16 = 0xc6;
const PON_FAULT_REASON1: u16 = 0xc8;
const PON_FAULT_REASON2: u16 = 0xc9;
const PON_S3_RESET_REASON: u16 = 0xca;
/// PMIC-watchdog configuration (qpnp-power-on `PON_PMIC_WD_RESET_*`): the S1
/// and S2 timers plus the S2 control pair whose CTL2 bit 7 (`WD_EN`) says
/// whether the watchdog that survives APSS resets is armed at all. These are
/// configuration registers, so they still describe XBL's arming at the next
/// probe entry even after Android reboots in between.
pub const PON_WD_S1_TIMER: u16 = 0x54;
pub const PON_WD_S2_TIMER: u16 = 0x55;
pub const PON_WD_S2_CTL: u16 = 0x56;
pub const PON_WD_S2_CTL2: u16 = 0x57;
const PON_POFF_SEQ: u8 = 1 << 7;
const PON_FAULT_SEQ: u8 = 1 << 6;
const PON_S3_RESET_SEQ: u8 = 1 << 5;
const TYPEC_MISC_STATUS: u16 = PM8150B_TYPEC_BASE + 0x0b;
const TYPEC_MODE_CFG: u16 = PM8150B_TYPEC_BASE + 0x44;
const TYPEC_CC_ATTACHED: u8 = 1 << 0;
const TYPEC_CC_ORIENTATION: u8 = 1 << 1;
const TYPEC_SNK_SRC_MODE: u8 = 1 << 6;
const TYPEC_INTR_EN_CFG_1: u16 = PM8150B_TYPEC_BASE + 0x5e;
const TYPEC_VCONN_CONTROL: u16 = PM8150B_TYPEC_BASE + 0x46;
const TYPEC_VCONN_EN_SRC: u8 = 1 << 0;
const TYPEC_VCONN_EN_VALUE: u8 = 1 << 1;
const TYPEC_EXIT_STATE_CFG: u16 = PM8150B_TYPEC_BASE + 0x50;
const TYPEC_SEL_SRC_UPPER_REF: u8 = 1 << 2;
const TYPEC_EN_SNK_ONLY: u8 = 1 << 1;
const TYPEC_EN_SRC_ONLY: u8 = 1 << 2;

// QPNP interrupt block registers used by the SPMI irq-domain path.  The
// Type-C child node's first interrupt is the one consumed by the Android
// qcom-pmic-typec driver; it is still a real PMIC interrupt, not a second
// DWC3/GIC line.
const QPNPINT_SET_TYPE: u16 = PM8150B_TYPEC_BASE + 0x11;
const QPNPINT_POLARITY_HIGH: u16 = PM8150B_TYPEC_BASE + 0x12;
const QPNPINT_POLARITY_LOW: u16 = PM8150B_TYPEC_BASE + 0x13;
const QPNPINT_LATCHED_CLR: u16 = PM8150B_TYPEC_BASE + 0x14;
const QPNPINT_EN_SET: u16 = PM8150B_TYPEC_BASE + 0x15;
const SPMI_PIC_ACC_ENABLE: usize = 0x100;
const SPMI_PIC_IRQ_CLEAR: usize = 0x108;
const USB_VBUS_CMD_OTG: u16 = 0x40;
const USB_VBUS_OTG_CFG: u16 = 0x53;
const USB_VBUS_OTG_EN: u8 = 1 << 0;
const USB_VBUS_OTG_EN_SRC_CFG: u8 = 1 << 1;

const PDC_IRQ_ENABLE_BANK: usize = 0x10;
const PDC_IRQ_CONFIG: usize = 0x110;
const PDC_VERSION: usize = 0x1000;
const PDC_IRQ_CONFIG_MASK: u32 = 0x7;
const PDC_LEVEL_HIGH: u32 = 0b100;
const PDC_EDGE_RISING: u32 = 0b110;
const PDC_VERSION_3_2: u32 = 0x0302_00;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PdcRegisterLayout {
    enable_bank: Option<usize>,
    irq_enable_bit: Option<u32>,
}

fn pdc_register_layout(version: u32) -> PdcRegisterLayout {
    let _ = version;
    // Match qcom-pdc.c used by the Android/Linux Lito platform: every
    // supported revision enables a pin through IRQ_ENABLE_BANK at 0x10.
    // IRQ_CFG contains trigger/polarity fields, but is not the enable
    // mechanism for the direct USB PDC outputs.
    PdcRegisterLayout {
        enable_bank: Some(PDC_IRQ_ENABLE_BANK),
        irq_enable_bit: None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdcPinRange {
    pub pin_base: u32,
    pub parent_base: u32,
    pub count: u32,
}

// qcom,lito-pdc from the Android Lito DT. PDC output pins are translated to
// GIC parent SPIs by these ranges before the GIC route is enabled.
pub const LITO_PDC_RANGES: [PdcPinRange; 5] = [
    PdcPinRange {
        pin_base: 0,
        parent_base: 480,
        count: 42,
    },
    PdcPinRange {
        pin_base: 42,
        parent_base: 612,
        count: 28,
    },
    PdcPinRange {
        pin_base: 70,
        parent_base: 63,
        count: 1,
    },
    PdcPinRange {
        pin_base: 71,
        parent_base: 640,
        count: 15,
    },
    PdcPinRange {
        pin_base: 86,
        parent_base: 522,
        count: 52,
    },
];

pub fn pdc_parent_irq(pin: u32) -> Option<u32> {
    for range in LITO_PDC_RANGES {
        if pin >= range.pin_base && pin < range.pin_base + range.count {
            return Some(range.parent_base + pin - range.pin_base);
        }
    }
    None
}

pub fn is_usb_irq(interrupt_id: u32) -> bool {
    let resources = usb_resources();
    interrupt_id == resources.spmi_parent_irq
        || interrupt_id == resources.smmu_global_irq
        || resources.smmu_context_irqs[..resources.smmu_context_irq_count]
            .iter()
            .any(|irq| *irq == interrupt_id)
        || resources.irqs.iter().any(|irq| {
            irq.number == interrupt_id
                || (irq.kind == UsbIrqKind::Pdc && pdc_parent_irq(irq.number) == Some(interrupt_id))
        })
}

pub fn is_usb_smmu_irq(interrupt_id: u32) -> bool {
    let resources = usb_resources();
    interrupt_id == resources.smmu_global_irq
        || resources.smmu_context_irqs[..resources.smmu_context_irq_count]
            .iter()
            .any(|irq| *irq == interrupt_id)
}

pub fn usb_controller_irq() -> u32 {
    usb_resources().irqs[4].number
}

pub fn usb_power_event_irq() -> u32 {
    usb_resources().irqs[1].number
}

pub fn usb_typec_parent_irq() -> u32 {
    usb_resources().spmi_parent_irq
}

#[derive(Clone, Copy)]
pub struct TypecState {
    pub arbiter_version: u32,
    /// SPMI APID selected during discovery. Keeping this in the live state is
    /// what lets an IRQ/poll transition re-read the same PMIC peripheral
    /// instead of repeating a potentially destructive APID scan.
    pub apid: usize,
    pub writable: bool,
    pub misc_status: u8,
    pub mode: u8,
    pub orientation_reverse: bool,
    pub role: UsbRole,
    pub sink_mode_written: bool,
    pub attached: bool,
    pub attach_settled: bool,
    pub phase: TypecPhase,
}

/// Role reported by the PMIC Type-C state machine. The USB gadget is allowed
/// to advertise only for `Device`; a CC-attached source is a host role and
/// must not be treated as a usable USB2 device session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbRole {
    None,
    Device,
    Host,
}

pub const fn typec_role(misc_status: u8) -> UsbRole {
    if misc_status & TYPEC_CC_ATTACHED == 0 {
        UsbRole::None
    } else if misc_status & TYPEC_SNK_SRC_MODE != 0 {
        UsbRole::Host
    } else {
        UsbRole::Device
    }
}

/// Qualcomm's Type-C glue is a state machine, not a one-shot sink bit.  Keep
/// the observable states explicit so a PMIC IRQ/poll update cannot race the
/// USB session override or re-enable the gadget after detach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypecPhase {
    Disabled,
    SinkOnlyRequested,
    WaitingAttach,
    Attached,
    Host,
    Detached,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypecEvent {
    Disable,
    SinkOnlySelected,
    AttachDetected,
    HostDetected,
    DetachDetected,
}

/// Refresh the PM8150B Type-C attach state after the initial role request.
/// Linux receives this information from the PMIC interrupt domain; the
/// early-boot path has the same state transition but may be entered before a
/// stable GIC owner exists, so callers can invoke it from either polling or
/// the USB-related parent IRQ path.
pub unsafe fn refresh_usb_device_role(state: &mut TypecState) -> Option<TypecEvent> {
    let mut status = 0u8;
    if !unsafe {
        spmi_transfer(
            state.arbiter_version,
            state.apid,
            TYPEC_MISC_STATUS,
            &mut status,
            false,
        )
    } {
        return None;
    }
    let was_attached = state.attached;
    let was_role = state.role;
    state.misc_status = status;
    state.orientation_reverse = status & TYPEC_CC_ORIENTATION != 0;
    state.role = typec_role(status);
    if state.role == UsbRole::Host && !unsafe { set_typec_vbus(state, true) } {
        return None;
    }
    if state.role != UsbRole::Host && !unsafe { set_typec_vbus(state, false) } {
        return None;
    }
    state.attached = state.role == UsbRole::Device;
    if state.attached == was_attached && state.role == was_role {
        return None;
    }
    let event = match state.role {
        UsbRole::Device => TypecEvent::AttachDetected,
        UsbRole::Host => TypecEvent::HostDetected,
        UsbRole::None => TypecEvent::DetachDetected,
    };
    state.phase = typec_transition(state.phase, event);
    state.attach_settled = state.attached;
    Some(event)
}

const fn spmi_irq_polarity(trigger: IrqTrigger) -> (u8, u8, u8) {
    match trigger {
        IrqTrigger::RisingEdge => (1, 1, 0),
        IrqTrigger::EdgeBoth => (1, 1, 1),
        IrqTrigger::LevelHigh => (0, 1, 0),
    }
}

/// Complete the part normally supplied by the SPMI irq domain for the
/// qcom-pmic-typec platform IRQ.  This programs the PMIC edge/polarity,
/// clears a stale latch, enables the child bit, and finally enables the
/// APID's arbiter access bit.  The GIC parent is routed separately by
/// `init_interrupt_controller` after the distributor is ready.
pub unsafe fn configure_typec_irq(state: &TypecState) -> bool {
    let resource = usb_resources().typec_irq;
    if !state.writable
        || resource.sid != PM8150B_SID
        || resource.peripheral_id != (PM8150B_TYPEC_PPID & 0xff) as u8
        || resource.irq >= 8
    {
        return false;
    }
    let bit = 1u8 << resource.irq;
    let (edge, high, low) = spmi_irq_polarity(resource.trigger);
    let mut value = if edge != 0 { bit } else { 0 };
    if !unsafe {
        spmi_update_bits(
            state.arbiter_version,
            state.apid,
            QPNPINT_SET_TYPE,
            bit,
            value,
        )
    } {
        return false;
    }
    value = if high != 0 { bit } else { 0 };
    if !unsafe {
        spmi_update_bits(
            state.arbiter_version,
            state.apid,
            QPNPINT_POLARITY_HIGH,
            bit,
            value,
        )
    } {
        return false;
    }
    value = if low != 0 { bit } else { 0 };
    if !unsafe {
        spmi_update_bits(
            state.arbiter_version,
            state.apid,
            QPNPINT_POLARITY_LOW,
            bit,
            value,
        )
    } {
        return false;
    }

    // qpnpint_irq_unmask() clears the PMIC latch before enabling the bit.
    let mut command = bit;
    if !unsafe {
        spmi_transfer(
            state.arbiter_version,
            state.apid,
            QPNPINT_LATCHED_CLR,
            &mut command,
            true,
        )
    } || !unsafe {
        spmi_transfer(
            state.arbiter_version,
            state.apid,
            QPNPINT_EN_SET,
            &mut command,
            true,
        )
    } {
        return false;
    }

    // pmic_arb_acc_enable_v5() and pmic_arb_irq_clear_v5() are in the write
    // channel window, one 64-KiB APID window per peripheral.
    let offset = spmi_channel_offset(state.arbiter_version, state.apid, false);
    unsafe {
        spmi_write(SPMI_CHANNELS, offset + SPMI_PIC_IRQ_CLEAR, bit as u32);
        spmi_write(SPMI_CHANNELS, offset + SPMI_PIC_ACC_ENABLE, 1);
    }
    true
}

/// Acknowledge one Type-C child interrupt after its state has been sampled.
/// Both layers must be cleared: qpnpint's latched child bit and the SPMI
/// arbiter's parent summary. Leaving either asserted can retrigger the GIC
/// SPI indefinitely or lose the next attach edge.
pub unsafe fn acknowledge_typec_irq(state: &TypecState) -> bool {
    let resource = usb_resources().typec_irq;
    if !state.writable
        || resource.sid != PM8150B_SID
        || resource.peripheral_id != (PM8150B_TYPEC_PPID & 0xff) as u8
        || resource.irq >= 8
    {
        return false;
    }
    let bit = 1u8 << resource.irq;
    let mut command = bit;
    if !unsafe {
        spmi_transfer(
            state.arbiter_version,
            state.apid,
            QPNPINT_LATCHED_CLR,
            &mut command,
            true,
        )
    } {
        return false;
    }
    let offset = spmi_channel_offset(state.arbiter_version, state.apid, false);
    unsafe {
        spmi_write(SPMI_CHANNELS, offset + SPMI_PIC_IRQ_CLEAR, bit as u32);
        spmi_read(SPMI_CHANNELS, offset + SPMI_PIC_IRQ_CLEAR) & bit as u32 == 0
    }
}

pub const fn typec_transition(phase: TypecPhase, event: TypecEvent) -> TypecPhase {
    match (phase, event) {
        (_, TypecEvent::Disable) => TypecPhase::Disabled,
        (TypecPhase::Disabled, TypecEvent::SinkOnlySelected)
        | (TypecPhase::Detached, TypecEvent::SinkOnlySelected) => TypecPhase::SinkOnlyRequested,
        (TypecPhase::SinkOnlyRequested, TypecEvent::AttachDetected)
        | (TypecPhase::WaitingAttach, TypecEvent::AttachDetected) => TypecPhase::Attached,
        (TypecPhase::SinkOnlyRequested, TypecEvent::DetachDetected) => TypecPhase::WaitingAttach,
        (_, TypecEvent::HostDetected) => TypecPhase::Host,
        (TypecPhase::Attached, TypecEvent::DetachDetected) => TypecPhase::Detached,
        (TypecPhase::Host, TypecEvent::DetachDetected) => TypecPhase::Detached,
        (current, _) => current,
    }
}

#[inline]
unsafe fn spmi_reg(base: usize, offset: usize) -> *mut u32 {
    (base + offset) as *mut u32
}

#[inline]
unsafe fn spmi_read(base: usize, offset: usize) -> u32 {
    unsafe { core::ptr::read_volatile(spmi_reg(base, offset)) }
}

#[inline]
unsafe fn spmi_write(base: usize, offset: usize, value: u32) {
    unsafe { core::ptr::write_volatile(spmi_reg(base, offset), value) };
    let _ = unsafe { spmi_read(base, offset) };
}

fn find_typec_apid(version: u32) -> Option<(usize, bool)> {
    unsafe { find_spmi_apid(version, PM8150B_TYPEC_PPID) }
}

/// Resolve the APID for one `<SID>:<peripheral>` PPID. This is the same
/// owner-preferred arbiter walk used by Type-C discovery.
fn find_spmi_apid(version: u32, ppid: u16) -> Option<(usize, bool)> {
    unsafe {
        if version >= 0x5000_0000 {
            // v5 has a flat APID -> PPID table.  Multiple APIDs can refer to
            // one peripheral; prefer one owned by execution environment 0 so
            // that a write cannot be silently rejected by the arbiter.
            let mut fallback = None;
            for apid in 0..512usize {
                let entry = spmi_read(SPMI_CORE, SPMI_APID_MAP_V5 + apid * 4);
                if ((entry >> 8) & 0x0fff) as u16 != ppid {
                    continue;
                }
                let owner = spmi_read(SPMI_CONFIG, SPMI_OWNERSHIP_TABLE + apid * 4) & 0x7;
                if owner == SPMI_EE as u32 {
                    return Some((apid, true));
                }
                fallback = Some((apid, false));
            }
            return fallback;
        }

        // v2/v3 use the binary mapping tree in the configuration block and
        // the APID table at core+0x800.  This is the same lookup used by the
        // upstream SPMI driver, bounded to the arbiter's 16-bit tree depth.
        let mut index = 0usize;
        for _ in 0..16 {
            let entry = spmi_read(SPMI_CONFIG, SPMI_MAPPING_TABLE + index * 4);
            let bit = ((entry >> 18) & 0xf) as u16;
            let one = (ppid & (1 << bit)) != 0;
            let flag = if one {
                (entry >> 8) & 1
            } else {
                (entry >> 17) & 1
            };
            let result = if one {
                entry & 0xff
            } else {
                (entry >> 9) & 0xff
            };
            if flag != 0 {
                index = result as usize;
                continue;
            }
            return Some((result as usize, true));
        }
    }
    None
}

/// Read the primary PM8150 PON reset reason as a small bucket code. The
/// raw registers are a bitmask, so return the selected sequence's first set
/// bit (1-based) and add 8 for the FAULT sequence or 16 for S3_RESET.
pub unsafe fn read_pm8150_pon_reset_code() -> Option<u8> {
    let version = unsafe { spmi_read(SPMI_CORE, SPMI_VERSION) };
    if version == 0 || version == u32::MAX {
        return None;
    }
    let (apid, _) = unsafe { find_spmi_apid(version, PM8150_PON_PPID)? };
    let mut subtype = 0u8;
    if !unsafe { spmi_transfer(version, apid, PON_SUBTYPE, &mut subtype, false) } {
        return None;
    }
    // Only the primary/gen2 PON layouts have the 0xC0..0xCA reason block.
    if !matches!(subtype, 1..=5) {
        return None;
    }
    let mut reason1 = 0u8;
    let mut poff1 = 0u8;
    let mut poff2 = 0u8;
    let mut off_seq = 0u8;
    if !unsafe { spmi_transfer(version, apid, PON_REASON1, &mut reason1, false) }
        || !unsafe { spmi_transfer(version, apid, PON_POFF_REASON1, &mut poff1, false) }
        || !unsafe { spmi_transfer(version, apid, PON_POFF_REASON2, &mut poff2, false) }
        || !unsafe { spmi_transfer(version, apid, PON_OFF_SEQ, &mut off_seq, false) }
    {
        return None;
    }
    let first_set = |value: u8| -> u8 {
        if value == 0 {
            0
        } else {
            (value.trailing_zeros() + 1) as u8
        }
    };
    let poff_code = first_set(poff1).max(first_set(poff2));
    let code = if off_seq & PON_POFF_SEQ != 0 {
        poff_code
    } else if off_seq & PON_FAULT_SEQ != 0 {
        let mut fault1 = 0u8;
        let mut fault2 = 0u8;
        if !unsafe { spmi_transfer(version, apid, PON_FAULT_REASON1, &mut fault1, false) }
            || !unsafe { spmi_transfer(version, apid, PON_FAULT_REASON2, &mut fault2, false) }
        {
            return None;
        }
        8 + first_set(fault1).max(first_set(fault2))
    } else if off_seq & PON_S3_RESET_SEQ != 0 {
        let mut s3 = 0u8;
        if !unsafe { spmi_transfer(version, apid, PON_S3_RESET_REASON, &mut s3, false) } {
            return None;
        }
        16 + first_set(s3)
    } else {
        poff_code
    };
    if code != 0 {
        return Some(code);
    }
    Some(first_set(reason1))
}

/// IMEM restart-reason cookie (lito `qcom,msm-imem@146ab000`,
/// `restart_reason@65c`): the 32-bit magic the last resetter wrote before
/// rebooting. The kernel's restart path writes 0x77665500 (bootloader) and
/// 0x77665502 (recovery); a watchdog bark handler writes its own magic, and
/// XBL decodes the cookie into the bootreason it hands Android. Read at probe
/// entry the cookie names what rebooted the handset mid-probe - unless the
/// automatic recovery's `adb reboot bootloader` (0x77665500) overwrote it
/// last.
const IMEM_RESTART_REASON: usize = 0x146a_b65c;

pub fn read_imem_restart_reason() -> u32 {
    unsafe { core::ptr::read_volatile(IMEM_RESTART_REASON as *const u32) }
}

/// Read one raw PON register by page offset for the death-reason readout
/// channel. Only the registers that name the ~5.6 s post-attach watchdog are
/// whitelisted so a caller typo cannot turn into a stray SPMI transaction.
pub unsafe fn read_pm8150_pon_register(register: u16) -> Option<u8> {
    if !matches!(
        register,
        PON_WD_S1_TIMER
            | PON_WD_S2_TIMER
            | PON_WD_S2_CTL
            | PON_WD_S2_CTL2
            | PON_WARM_RESET_REASON1
            | PON_SOFT_RESET_REASON1
    ) {
        return None;
    }
    let version = unsafe { spmi_read(SPMI_CORE, SPMI_VERSION) };
    if version == 0 || version == u32::MAX {
        return None;
    }
    let (apid, _) = unsafe { find_spmi_apid(version, PM8150_PON_PPID)? };
    let mut value = 0u8;
    if !unsafe { spmi_transfer(version, apid, register, &mut value, false) } {
        return None;
    }
    Some(value)
}

fn spmi_channel_offset(version: u32, apid: usize, observer: bool) -> usize {
    if version >= 0x5000_0000 {
        if observer {
            0x10000 * SPMI_EE + 0x80 * apid
        } else {
            0x10000 * apid
        }
    } else {
        0x1000 * SPMI_EE + 0x8000 * apid
    }
}

unsafe fn spmi_transfer(
    version: u32,
    apid: usize,
    address: u16,
    value: &mut u8,
    write: bool,
) -> bool {
    let observer = !write;
    let offset = spmi_channel_offset(version, apid, observer);
    let base = if observer {
        SPMI_OBSERVER
    } else {
        SPMI_CHANNELS
    };
    let command = ((if write {
        SPMI_OP_EXT_WRITEL
    } else {
        SPMI_OP_EXT_READL
    }) << 27)
        | (((address & 0xff) as u32) << 4);

    unsafe {
        if write {
            spmi_write(SPMI_CHANNELS, offset + SPMI_WDATA0, *value as u32);
        }
        spmi_write(base, offset, command);
        for _ in 0..1_000_000u32 {
            let status = spmi_read(base, offset + SPMI_STATUS);
            if status & SPMI_STATUS_DONE != 0 {
                if status & (SPMI_STATUS_FAILURE | SPMI_STATUS_DENIED | SPMI_STATUS_DROPPED) != 0 {
                    return false;
                }
                if !write {
                    *value = spmi_read(SPMI_OBSERVER, offset + SPMI_RDATA0) as u8;
                }
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
    false
}

/// Apply the masked register update used by the Linux regmap Type-C driver.
/// PMIC register writes are not ordinary memory stores: preserve unrelated
/// bits and only publish the requested field after the read transaction has
/// completed.
unsafe fn spmi_update_bits(version: u32, apid: usize, address: u16, mask: u8, value: u8) -> bool {
    let mut current = 0u8;
    if !unsafe { spmi_transfer(version, apid, address, &mut current, false) } {
        return false;
    }
    let mut written = (current & !mask) | (value & mask);
    unsafe { spmi_transfer(version, apid, address, &mut written, true) }
}

/// Mirror `qcom_usb_vbus-regulator.c`: disable PMIC hardware source control
/// once, then use the OTG enable bit for the role-switch VBUS regulator.
pub unsafe fn set_typec_vbus(state: &TypecState, enabled: bool) -> bool {
    let Some(base) = usb_resources().vbus_reg_base else {
        // Bramble's device-role path does not source VBUS. Absence is only a
        // failure when a caller explicitly requests host/source operation.
        return !enabled;
    };
    if !state.writable {
        return false;
    }
    if !unsafe {
        spmi_update_bits(
            state.arbiter_version,
            state.apid,
            base + USB_VBUS_OTG_CFG,
            USB_VBUS_OTG_EN_SRC_CFG,
            0,
        )
    } {
        return false;
    }
    unsafe {
        spmi_update_bits(
            state.arbiter_version,
            state.apid,
            base + USB_VBUS_CMD_OTG,
            USB_VBUS_OTG_EN,
            if enabled { USB_VBUS_OTG_EN } else { 0 },
        )
    }
}

/// Observe PM8150B Type-C state without changing the controller. This is the
/// safe entry point for a `fastboot boot` handoff: Fastboot already owns the
/// live cable/PHY session, so Fullerene must not repeat the cold-boot role
/// writes while the old gadget is being torn down.
pub unsafe fn observe_usb_device_role() -> Option<TypecState> {
    let version = unsafe { spmi_read(SPMI_CORE, SPMI_VERSION) };
    if version == 0 || version == u32::MAX {
        return None;
    }
    let (apid, writable) = find_typec_apid(version)?;
    let mut misc_status = 0u8;
    if !unsafe { spmi_transfer(version, apid, TYPEC_MISC_STATUS, &mut misc_status, false) } {
        return None;
    }
    let mut mode = 0u8;
    if !unsafe { spmi_transfer(version, apid, TYPEC_MODE_CFG, &mut mode, false) } {
        return None;
    }
    let role = typec_role(misc_status);
    let attached = role == UsbRole::Device;
    let phase = match role {
        UsbRole::Device => TypecPhase::Attached,
        UsbRole::Host => TypecPhase::Host,
        UsbRole::None => TypecPhase::Detached,
    };
    Some(TypecState {
        arbiter_version: version,
        apid,
        writable,
        misc_status,
        mode,
        orientation_reverse: misc_status & TYPEC_CC_ORIENTATION != 0,
        role,
        sink_mode_written: false,
        attached,
        attach_settled: attached,
        phase,
    })
}

/// Read PM8150B Type-C state and select sink-only mode for a cold host
/// connection. The synchronous entry point is retained for a future cold
/// boot path; the Fastboot handoff uses observe_usb_device_role() above.
pub unsafe fn prepare_usb_device_role() -> Option<TypecState> {
    let version = unsafe { spmi_read(SPMI_CORE, SPMI_VERSION) };
    if version == 0 || version == u32::MAX {
        return None;
    }
    let (apid, writable) = find_typec_apid(version)?;
    let mut misc_status = 0u8;
    if !unsafe { spmi_transfer(version, apid, TYPEC_MISC_STATUS, &mut misc_status, false) } {
        return None;
    }
    let mut mode = 0u8;
    if !unsafe { spmi_transfer(version, apid, TYPEC_MODE_CFG, &mut mode, false) } {
        return None;
    }

    let mut sink_mode_written = false;
    let mut phase = TypecPhase::Disabled;
    if mode & TYPEC_EN_SNK_ONLY != 0 && mode & TYPEC_EN_SRC_ONLY == 0 {
        phase = typec_transition(phase, TypecEvent::SinkOnlySelected);
    }
    // The USB cable is attached to a host during `fastboot boot`; the phone
    // must therefore remain a sink and expose a USB device, not source VBUS.
    // Preserve unrelated PMIC bits and only replace the source/sink selection.
    // During a `fastboot boot` handoff the PMIC can report CC detached for a
    // short interval while the bootloader is tearing down its gadget.  The
    // cable is nevertheless the boot transport that brought us here, so do
    // not discard the device-role request solely because that transient bit
    // is clear.  Reassert sink-only whenever the current mode is not already
    // an unambiguous sink configuration.
    if writable {
        // Mirror qcom_pmic_typec_typec_hw_init(): disable Type-C interrupt
        // reporting while the initial role fields are changed, select UFP /
        // sink-only, leave VCONN under the PMIC policy, and select the source
        // upper reference for the exit state. The previous implementation
        // forced a transient DISABLE command before MODE_CFG; that is not the
        // Linux flow and can leave the CC state machine detached during a
        // Fastboot handoff.
        let interrupts_disabled =
            unsafe { spmi_update_bits(version, apid, TYPEC_INTR_EN_CFG_1, 0xff, 0) };
        let requested = (mode & !(TYPEC_EN_SNK_ONLY | TYPEC_EN_SRC_ONLY)) | TYPEC_EN_SNK_ONLY;
        let mode_selected = unsafe {
            spmi_update_bits(
                version,
                apid,
                TYPEC_MODE_CFG,
                TYPEC_EN_SNK_ONLY | TYPEC_EN_SRC_ONLY,
                TYPEC_EN_SNK_ONLY,
            )
        };
        let vconn_configured = unsafe {
            spmi_update_bits(
                version,
                apid,
                TYPEC_VCONN_CONTROL,
                TYPEC_VCONN_EN_SRC | TYPEC_VCONN_EN_VALUE,
                TYPEC_VCONN_EN_SRC,
            )
        };
        let exit_state_configured = unsafe {
            spmi_update_bits(
                version,
                apid,
                TYPEC_EXIT_STATE_CFG,
                TYPEC_SEL_SRC_UPPER_REF,
                TYPEC_SEL_SRC_UPPER_REF,
            )
        };
        sink_mode_written =
            interrupts_disabled && mode_selected && vconn_configured && exit_state_configured;
        if mode_selected {
            phase = typec_transition(phase, TypecEvent::SinkOnlySelected);
            mode = requested;
        }
    }

    // Selecting sink-only is only the role request. Qualcomm's Type-C glue
    // then waits for CC attach before asserting the VBUS/session override.
    // Fastboot handoff briefly reports detached while it tears down its own
    // gadget, so use a bounded poll and preserve the last observed state in
    // the returned contract instead of treating the role write as an attach.
    let mut role = typec_role(misc_status);
    let mut attached = role == UsbRole::Device;
    if attached {
        phase = typec_transition(phase, TypecEvent::AttachDetected);
    } else if role == UsbRole::Host {
        // A host/source attach is a valid Type-C role, but it is not a
        // device-mode USB session. Keep it explicit so the role-switch
        // consumer can tear down a previously advertised gadget.
        phase = typec_transition(phase, TypecEvent::HostDetected);
    } else if sink_mode_written {
        phase = typec_transition(phase, TypecEvent::DetachDetected);
    }
    let mut attach_settled = attached;
    if sink_mode_written && !attached {
        for _ in 0..32 {
            let mut status = 0u8;
            if !unsafe { spmi_transfer(version, apid, TYPEC_MISC_STATUS, &mut status, false) } {
                break;
            }
            misc_status = status;
            role = typec_role(status);
            attached = role == UsbRole::Device;
            if attached {
                phase = typec_transition(phase, TypecEvent::AttachDetected);
                attach_settled = true;
                break;
            }
            for _ in 0..10_000 {
                unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
            }
        }
    }
    let state = TypecState {
        arbiter_version: version,
        apid,
        writable,
        misc_status,
        mode,
        orientation_reverse: misc_status & TYPEC_CC_ORIENTATION != 0,
        role: typec_role(misc_status),
        sink_mode_written,
        attached,
        attach_settled,
        phase,
    };
    if state.role == UsbRole::Host && !unsafe { set_typec_vbus(&state, true) } {
        return None;
    }
    if state.role != UsbRole::Host && !unsafe { set_typec_vbus(&state, false) } {
        return None;
    }
    Some(state)
}

unsafe fn configure_pdc_irq(irq: IrqResource) -> bool {
    let base = usb_resources().pdc_base;
    let version = unsafe { core::ptr::read_volatile((base + PDC_VERSION) as *const u32) };
    let layout = pdc_register_layout(version);
    let config = matches!(irq.trigger, IrqTrigger::RisingEdge | IrqTrigger::EdgeBoth)
        .then_some(PDC_EDGE_RISING)
        .unwrap_or(PDC_LEVEL_HIGH);
    let config_address = (base + PDC_IRQ_CONFIG + irq.number as usize * 4) as *mut u32;
    let mut value = unsafe { core::ptr::read_volatile(config_address) };
    value = (value & !PDC_IRQ_CONFIG_MASK) | config;
    if let Some(bit) = layout.irq_enable_bit {
        value |= 1 << bit;
    }
    unsafe { core::ptr::write_volatile(config_address, value) };
    let _ = unsafe { core::ptr::read_volatile(config_address) };

    let pdc_enabled = if let Some(enable_bank) = layout.enable_bank {
        let bank_address = (base + enable_bank + (irq.number as usize / 32) * 4) as *mut u32;
        let mut bank = unsafe { core::ptr::read_volatile(bank_address) };
        bank |= 1 << (irq.number % 32);
        unsafe { core::ptr::write_volatile(bank_address, bank) };
        unsafe { core::ptr::read_volatile(bank_address) & (1 << (irq.number % 32)) != 0 }
    } else {
        unsafe { core::ptr::read_volatile(config_address) & (1 << 3) != 0 }
    };

    let Some(parent) = pdc_parent_irq(irq.number) else {
        return false;
    };
    let spi = parent.checked_sub(32).unwrap_or(u32::MAX) as usize;
    let pin = spi / 32;
    if pin * 4 >= usb_resources().pdc_spi_cfg_size {
        return false;
    }
    let parent_config = (usb_resources().pdc_spi_cfg_base + pin * 4) as *mut u32;
    let parent_mask = 1u32 << (spi % 32);
    let mut parent_value = unsafe { core::ptr::read_volatile(parent_config) };
    if matches!(irq.trigger, IrqTrigger::LevelHigh) {
        parent_value |= parent_mask;
    } else {
        parent_value &= !parent_mask;
    }
    unsafe { core::ptr::write_volatile(parent_config, parent_value) };
    pdc_enabled
        && unsafe {
            core::ptr::read_volatile(parent_config) & parent_mask
                == if matches!(irq.trigger, IrqTrigger::LevelHigh) {
                    parent_mask
                } else {
                    0
                }
        }
}

/// Program the three USB PDC pins exactly as the Qualcomm PDC irqchip does:
/// configure the polarity/trigger in the PDC, then unmask the pin. The parent
/// GIC SPIs are enabled separately after the GIC redistributor is awake.
pub unsafe fn configure_usb_pdc_irqs() -> bool {
    let resources = usb_resources();
    let mut ok = true;
    for irq in resources.irqs {
        if irq.kind == UsbIrqKind::Pdc {
            ok &= unsafe { configure_pdc_irq(irq) };
        }
    }
    ok
}

/// Enable the USB3 GDSC using the same software-controlled sequence as the
/// Qualcomm GDSC regulator driver. The parent RPMh supplies are intentionally
/// not touched here: those rails are controlled by secure firmware and are
/// already enabled by the Pixel boot chain for a temporary boot image.
pub unsafe fn enable_usb30_gdsc() -> bool {
    if !usb_power_contract_valid(true) {
        return false;
    }
    let address = usb_resources().gdsc as *mut u32;
    let mut value = unsafe { core::ptr::read_volatile(address) };
    value &= !(GDSC_HW_CONTROL | GDSC_SW_OVERRIDE | GDSC_WAIT_MASK);
    value |= GDSC_WAIT_VALUE;
    unsafe { core::ptr::write_volatile(address, value) };
    let _ = unsafe { core::ptr::read_volatile(address) };

    value &= !GDSC_SW_COLLAPSE;
    unsafe { core::ptr::write_volatile(address, value) };
    let _ = unsafe { core::ptr::read_volatile(address) };

    for _ in 0..1_000_000u32 {
        if unsafe { core::ptr::read_volatile(address) } & GDSC_PWR_ON != 0 {
            set_usb_resource_state(|state| state.gdsc_enabled = true);
            return true;
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    false
}

/// Force the USB3 GDSC on WITHOUT the secure-rail contract gate. The
/// attach-delay readout measured the core domain SW_COLLAPSED with a dead
/// GSNPSID at probe entry in the fastboot handoff state: the physical attach
/// is carried by the QSCRATCH/PHY domain while every DWC3 core register
/// reads dead, which is the long-standing "endpoint command wedge". Gate-run
/// flows call this before the handoff so the core answers MMIO again; the
/// parent RPMh supplies are left to the boot chain exactly as in
/// `enable_usb30_gdsc`.
pub unsafe fn force_enable_usb30_gdsc() -> bool {
    let address = usb_resources().gdsc as *mut u32;
    let mut value = unsafe { core::ptr::read_volatile(address) };
    value &= !(GDSC_HW_CONTROL | GDSC_SW_OVERRIDE | GDSC_WAIT_MASK);
    value |= GDSC_WAIT_VALUE;
    unsafe { core::ptr::write_volatile(address, value) };
    let _ = unsafe { core::ptr::read_volatile(address) };

    value &= !GDSC_SW_COLLAPSE;
    unsafe { core::ptr::write_volatile(address, value) };
    let _ = unsafe { core::ptr::read_volatile(address) };

    for _ in 0..1_000_000u32 {
        if unsafe { core::ptr::read_volatile(address) } & GDSC_PWR_ON != 0 {
            set_usb_resource_state(|state| state.gdsc_enabled = true);
            return true;
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    false
}

/// Collapse the USB3 GDSC after the controller and GSI write path are idle.
/// This is the inverse of `enable_usb30_gdsc`; secure-owned regulator rails
/// remain untouched and the caller controls the ordering relative to clocks.
pub unsafe fn disable_usb30_gdsc() -> bool {
    let address = usb_resources().gdsc as *mut u32;
    let mut value = unsafe { core::ptr::read_volatile(address) };
    value &= !(GDSC_HW_CONTROL | GDSC_SW_OVERRIDE | GDSC_WAIT_MASK);
    value |= GDSC_WAIT_VALUE | GDSC_SW_COLLAPSE;
    unsafe { core::ptr::write_volatile(address, value) };
    let _ = unsafe { core::ptr::read_volatile(address) };

    for _ in 0..1_000_000u32 {
        if unsafe { core::ptr::read_volatile(address) } & GDSC_PWR_ON == 0 {
            set_usb_resource_state(|state| state.gdsc_enabled = false);
            return true;
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    false
}

/// Submit the CX ARC vote demanded by the GCC USB clock tree. The `gcc`
/// node's `vdd_cx-supply` is `VDD_CX_LEVEL` (`cx.lvl`, `S1A`), whose DT init
/// level is RETENTION, so an unvoted CX corner drops the USB30 GDSC domain
/// and the USB clock branches out of operating spec even while the GDSC
/// reads PWR_ON. Linux issues this vote from the clock framework's
/// `vdd_class` when the USB RCGs are prepared; the vote rides the same
/// single-command VRM transport with the level as the data word.
pub unsafe fn apply_usb_cx_vote(vote: UsbBusVote) -> bool {
    let Some(address) = (unsafe { command_db_read_addr(&RPMH_CXLVL) }) else {
        return false;
    };
    let command = RpmhBcmCommand {
        address,
        data: usb_cx_level(vote),
    };
    unsafe { send_rpmh_command_batch(core::slice::from_ref(&command)) }
}

/// Enable the dedicated USB2 PHY reference clock consumed by the Android
/// `qcom,usb-hsphy-snps-femto` node. Linux implements the DT
/// `<&rpmhcc RPMH_CXO_CLK>` handle as the Lito RPMh ARC resource `xo.lvl`,
/// with an active-state value of `0x3`; it is not one of the six GCC USB
/// branches and therefore cannot be covered by `enable_usb_clock_branches`.
///
/// This is an active-only handoff request. The early image remains awake for
/// the complete enumeration window, so the sleep/wake cache writes performed
/// by Linux's clock framework are not needed here. Re-sending the same ARC
/// value is idempotent at RPMh aggregation level and is useful when XBL's
/// client vote has just disappeared.
pub unsafe fn enable_usb_hs_phy_ref_clock() -> bool {
    let clock = usb_resources().hs_phy_clock;
    if clock.name != "ref_clk_src"
        || clock.provider != ClockProvider::Rpmh
        || clock.provider_id != 0
        || clock.normal_rate_hz != 19_200_000
    {
        return false;
    }
    let Some(address) = (unsafe { command_db_read_addr(&RPMH_XO_LVL) }) else {
        return false;
    };
    let command = RpmhBcmCommand {
        address,
        data: RPMH_XO_LVL_ON,
    };
    unsafe { send_rpmh_command_batch(core::slice::from_ref(&command)) }
}

/// Apply one complete Android-style performance transition.  The bus vote is
/// issued only after the clock source has been selected, matching the order
/// in which the Qualcomm glue raises the core clock and interconnect vote.
pub unsafe fn apply_usb_performance(vote: UsbBusVote) -> bool {
    // The GCC node's `vdd_cx-supply` must be raised before any USB clock is
    // prepared; a collapsed CX corner leaves the branch enable stalled.
    if unsafe { !apply_usb_cx_vote(vote) } {
        return false;
    }
    if unsafe { !configure_usb_clocks(vote) } {
        return false;
    }
    if unsafe { !apply_usb_bus_vote(vote) } {
        return false;
    }
    let _ = apply_usb_pm_qos(vote);
    set_usb_resource_state(|state| state.bus_vote = Some(vote));
    true
}

pub fn init_interrupt_controller(gicd_base: Option<usize>, gicr_base: Option<usize>) {
    let gicd = gicd_base.unwrap_or(GICD_BASE);
    let gicr = gicr_base.unwrap_or(GICR_BASE);
    let gic_ready = super::gicv3::init(gicd, gicr, Some(usb_controller_irq()));
    unsafe {
        // DWC3 has five platform sources plus the Apps-SMMU global and up to
        // 80 context-bank fault lines.  Keep the controller, power-event,
        // and SMMU fault routes in the GIC setup pass; truncating the latter
        // would make a DMA fault look like an unexplained EP0 timeout.
        //
        // The three PDC PHY lines are deliberately absent here. Android's
        // dwc3-qcom registers them with IRQF_NO_AUTOEN and enables them only
        // for host suspend/wakeup. Enabling those wakeup-only lines during a
        // live device-mode fastboot handoff can consume a PHY transition as
        // an active runtime event and is not equivalent to the Linux path.
        let mut spis = [0u32; 128];
        let mut count = 0usize;
        let resources = usb_resources();
        for irq in resources.irqs {
            let parent = (irq.kind == UsbIrqKind::GicSpi).then_some(irq.number);
            if let Some(parent) = parent {
                if parent != usb_controller_irq()
                    && !spis[..count].iter().any(|irq| *irq == parent)
                    && count < spis.len()
                {
                    spis[count] = parent;
                    count += 1;
                }
            }
        }
        for irq in core::iter::once(resources.smmu_global_irq).chain(
            resources.smmu_context_irqs[..resources.smmu_context_irq_count]
                .iter()
                .copied(),
        ) {
            if irq != usb_controller_irq()
                && !spis[..count].iter().any(|candidate| *candidate == irq)
                && count < spis.len()
            {
                spis[count] = irq;
                count += 1;
            }
        }
        // `fastboot boot` hands over a live Type-C session. The early
        // handoff observes the PMIC state but does not own the role-switch
        // child IRQ, so do not route its parent summary into the new GIC
        // owner. A role-changing cold-boot path must explicitly install the
        // child IRQ before adding this parent route.
        super::gicv3::enable_spis(gicd, &spis[..count]);
    }
    if gic_ready {
        unsafe {
            USB_GICD_BASE = gicd;
        }
        set_usb_resource_state(|state| state.irq_routes_enabled = true);
        // Re-apply the already selected nominal policy now that the
        // distributor is owned by the kernel. Before this point the same
        // policy was intentionally state-only because GIC MMIO was not safe.
        let vote = usb_resource_state().bus_vote.unwrap_or(UsbBusVote::Nominal);
        let _ = apply_usb_pm_qos(vote);
    }
}

const CMD_DB_MAGIC: [u8; 4] = [0xdb, 0x30, 0x03, 0x0c];
const CMD_DB_HEADER_BYTES: usize = 4 + 4 + (8 * CMD_DB_RESOURCE_HEADER_BYTES) + 4 + 4;
const CMD_DB_RESOURCE_HEADER_BYTES: usize = 16;
const CMD_DB_ENTRY_BYTES: usize = 24;
const CMD_DB_MAX_RESOURCES: usize = 8;

#[inline]
unsafe fn command_db_byte(base: usize, size: usize, offset: usize) -> Option<u8> {
    if offset >= size {
        return None;
    }
    Some(unsafe { core::ptr::read_volatile((base + offset) as *const u8) })
}

fn command_db_lookup<F>(size: usize, mut byte: F, id: &[u8; 8]) -> Option<u32>
where
    F: FnMut(usize) -> Option<u8>,
{
    let le16 = |offset: usize, byte: &mut F| {
        let lo = byte(offset)? as u16;
        let hi = byte(offset + 1)? as u16;
        Some(lo | (hi << 8))
    };
    let le32 = |offset: usize, byte: &mut F| {
        let b0 = byte(offset)? as u32;
        let b1 = byte(offset + 1)? as u32;
        let b2 = byte(offset + 2)? as u32;
        let b3 = byte(offset + 3)? as u32;
        Some(b0 | (b1 << 8) | (b2 << 16) | (b3 << 24))
    };

    if size < CMD_DB_HEADER_BYTES {
        return None;
    }
    for (index, expected) in CMD_DB_MAGIC.iter().enumerate() {
        if byte(4 + index) != Some(*expected) {
            return None;
        }
    }

    for resource in 0..CMD_DB_MAX_RESOURCES {
        let resource_offset = 16 + resource * CMD_DB_RESOURCE_HEADER_BYTES;
        let slave_id = le16(resource_offset, &mut byte)?;
        if slave_id == 0 {
            break;
        }
        let header_offset = le16(resource_offset + 2, &mut byte)? as usize;
        let count = le16(resource_offset + 6, &mut byte)? as usize;
        let entries_bytes = count.checked_mul(CMD_DB_ENTRY_BYTES)?;
        let entries_end = CMD_DB_HEADER_BYTES
            .checked_add(header_offset)?
            .checked_add(entries_bytes)?;
        if entries_end > size {
            return None;
        }
        for entry in 0..count {
            let entry_offset = CMD_DB_HEADER_BYTES + header_offset + entry * CMD_DB_ENTRY_BYTES;
            let mut matches = true;
            for (index, expected) in id.iter().enumerate() {
                if byte(entry_offset + index) != Some(*expected) {
                    matches = false;
                    break;
                }
            }
            if matches {
                let address = le32(entry_offset + 16, &mut byte)?;
                return (address != 0).then_some(address);
            }
        }
    }
    None
}

/// Look up a resource address using the same bounded layout as Linux's
/// `cmd_db_read_addr()`. The Command DB is reserved memory populated by the
/// Qualcomm firmware, so every offset is checked before a volatile read.
pub unsafe fn command_db_read_addr(id: &[u8; 8]) -> Option<u32> {
    let db = usb_resources().command_db;
    command_db_lookup(
        db.size,
        |offset| unsafe { command_db_byte(db.base, db.size, offset) },
        id,
    )
}

const RPMH_TCS_AMC_ENABLE: u32 = 1 << 16;
const RPMH_TCS_AMC_TRIGGER: u32 = 1 << 24;
const RPMH_CMD_MSGID_LEN: u32 = 8;
const RPMH_CMD_MSGID_RESP_REQ: u32 = 1 << 8;
const RPMH_CMD_MSGID_WRITE: u32 = 1 << 16;
const RPMH_CMD_STATUS_ISSUED: u32 = 1 << 8;
const RPMH_CMD_STATUS_COMPLETE: u32 = 1 << 16;

#[inline]
unsafe fn rpmh_reg(base: usize, offset: usize) -> *mut u32 {
    (base + offset) as *mut u32
}

unsafe fn rpmh_write_sync(base: usize, offset: usize, value: u32) -> bool {
    unsafe { core::ptr::write_volatile(rpmh_reg(base, offset), value) };
    for _ in 0..100_000 {
        if unsafe { core::ptr::read_volatile(rpmh_reg(base, offset)) } == value {
            return true;
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    false
}

/// Submit a bounded batch of RPMh commands through an Apps-RSC active TCS.
/// Both BCM interconnect votes and VRM regulator requests use this same
/// ownership/trigger/completion protocol; keeping it shared prevents the
/// regulator path from accidentally bypassing TCS arbitration.
unsafe fn send_rpmh_command_batch(commands: &[RpmhBcmCommand]) -> bool {
    if commands.is_empty() || commands.len() > 4 {
        return false;
    }
    let resources = usb_resources().rpmh_rsc;
    let rsc_id = unsafe { core::ptr::read_volatile(rpmh_reg(resources.driver_base, RPMH_RSC_ID)) };
    let Some(layout) = rpmh_register_layout(rsc_id) else {
        return false;
    };
    let tcs_base = resources.driver_base + resources.tcs_offset;
    let mut selected = None;
    for tcs in resources.active_tcs_offset..resources.active_tcs_offset + resources.active_tcs {
        let base = tcs_base + tcs as usize * layout.tcs_stride;
        let enabled = unsafe { core::ptr::read_volatile(rpmh_reg(base, layout.command_enable)) };
        if enabled == 0 {
            selected = Some(base);
            break;
        }
    }
    let Some(base) = selected else {
        return false;
    };

    if !unsafe { rpmh_write_sync(base, layout.control, 0) }
        || !unsafe { rpmh_write_sync(base, layout.command_enable, 0) }
        || !unsafe { rpmh_write_sync(base, layout.wait_for_completion, 0) }
    {
        return false;
    }

    let mut enable = 0;
    let mut wait_for_completion = 0;
    for (index, command) in commands.iter().enumerate() {
        let offset = index * layout.command_stride;
        let msgid = RPMH_CMD_MSGID_LEN | RPMH_CMD_MSGID_RESP_REQ | RPMH_CMD_MSGID_WRITE;
        unsafe {
            core::ptr::write_volatile(rpmh_reg(base, layout.command_msgid + offset), msgid);
            core::ptr::write_volatile(
                rpmh_reg(base, layout.command_address + offset),
                command.address,
            );
            core::ptr::write_volatile(rpmh_reg(base, layout.command_data + offset), command.data);
        }
        enable |= 1 << index;
        wait_for_completion |= 1 << index;
    }
    if !unsafe { rpmh_write_sync(base, layout.wait_for_completion, wait_for_completion) }
        || !unsafe { rpmh_write_sync(base, layout.command_enable, enable) }
        || !unsafe { rpmh_write_sync(base, layout.control, RPMH_TCS_AMC_ENABLE) }
    {
        return false;
    }
    unsafe {
        core::ptr::write_volatile(
            rpmh_reg(base, layout.control),
            RPMH_TCS_AMC_ENABLE | RPMH_TCS_AMC_TRIGGER,
        );
        core::arch::asm!("dsb sy", options(nostack));
    }

    let mut complete = false;
    for _ in 0..1_000_000 {
        complete = true;
        for (index, _) in commands.iter().enumerate() {
            let status = unsafe {
                core::ptr::read_volatile(rpmh_reg(
                    base,
                    layout.command_status + index * layout.command_stride,
                ))
            };
            if status & (RPMH_CMD_STATUS_ISSUED | RPMH_CMD_STATUS_COMPLETE)
                != (RPMH_CMD_STATUS_ISSUED | RPMH_CMD_STATUS_COMPLETE)
            {
                complete = false;
            }
        }
        if complete {
            break;
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }

    let released = unsafe { rpmh_write_sync(base, layout.control, 0) }
        && unsafe { rpmh_write_sync(base, layout.command_enable, 0) }
        && unsafe { rpmh_write_sync(base, layout.wait_for_completion, 0) };
    complete && released
}

/// Send an already-resolved USB BCM vote through one free Apps-RSC active
/// TCS. This mirrors the ordering in `rpmh_rsc_send_data()` and
/// `__tcs_buffer_write()`: claim an idle TCS, program all commands, trigger
/// AMC, wait for issued+complete, then release it. It is deliberately kept
/// private to this platform because RSC ownership is not a generic MMIO API.
unsafe fn send_usb_rpmh_vote(vote: UsbBusVote) -> bool {
    let bcm_votes = usb_bcm_votes(vote);
    let mut commands = [RpmhBcmCommand {
        address: 0,
        data: 0,
    }; 4];
    for index in 0..bcm_votes.count {
        let Some(address) = (unsafe { command_db_read_addr(&bcm_votes.votes[index].id) }) else {
            return false;
        };
        let is_last = index + 1 == bcm_votes.count;
        commands[index] = encode_rpmh_bcm_command(
            address,
            bcm_votes.votes[index].average_kbps != 0
                || bcm_votes.votes[index].instantaneous_kbps != 0,
            is_last,
            bcm_votes.votes[index].average_kbps,
            bcm_votes.votes[index].instantaneous_kbps,
        );
    }

    // Linux reads the RSC driver ID and arbitrates a free active TCS before
    // sending this batch; the shared helper preserves that refusal behavior
    // for an unreadable/unsupported secure-firmware interface.
    unsafe { send_rpmh_command_batch(&commands[..bcm_votes.count]) }
}

/// Apply the Android USB bus use-case vote when the firmware exposes a valid
/// Command DB and an idle Apps-RSC active TCS. Failure is reported to the
/// caller so the handoff can preserve the bootloader's existing vote rather
/// than guessing at secure-owned state.
pub unsafe fn apply_usb_bus_vote(vote: UsbBusVote) -> bool {
    unsafe { send_usb_rpmh_vote(vote) }
}

// VRM command sub-addresses from the msm-4.19 rpmh regulator driver
// (`RPMH_REGULATOR_REG_VRM_VOLTAGE`, `_ENABLE`, `_MODE`): the enable and mode
// requests are issued at resource+0x4 and resource+0x8, not at consecutive
// byte addresses.
const RPMH_REGULATOR_VRM_VOLTAGE: u32 = 0x0;
const RPMH_REGULATOR_ENABLE: u32 = 0x4;
const RPMH_REGULATOR_MODE: u32 = 0x8;
const RPMH_REGULATOR_MODE_LPM: u32 = 5;
const RPMH_REGULATOR_MODE_HPM: u32 = 7;

/// Submit one PMIC VRM regulator request using the same RPMh transport as the
/// Android regulator framework.  Command DB resolves the resource address;
/// no PMIC register offset is synthesized in the Apps CPU.
unsafe fn send_usb_regulator_request(rail: UsbRailResource, enable: bool) -> bool {
    let Some(address) = (unsafe { command_db_read_addr(&rail.rpmh_resource_id) }) else {
        return false;
    };
    let mut commands = [RpmhBcmCommand {
        address: 0,
        data: 0,
    }; 3];
    let count = if enable {
        let mut count = 0;
        if rail.program_voltage {
            commands[count] = RpmhBcmCommand {
                address: address + RPMH_REGULATOR_VRM_VOLTAGE,
                data: rail.min_uv / 1000,
            };
            count += 1;
        }
        commands[count] = RpmhBcmCommand {
            address: address + RPMH_REGULATOR_ENABLE,
            data: 1,
        };
        count += 1;
        commands[count] = RpmhBcmCommand {
            address: address + RPMH_REGULATOR_MODE,
            data: if rail.max_load_ua != 0 {
                RPMH_REGULATOR_MODE_HPM
            } else {
                RPMH_REGULATOR_MODE_LPM
            },
        };
        count + 1
    } else {
        commands[0] = RpmhBcmCommand {
            address: address + RPMH_REGULATOR_ENABLE,
            data: 0,
        };
        1
    };
    unsafe { send_rpmh_command_batch(&commands[..count]) }
}

/// Enable or disable every DT supply consumed by the USB2/QMP PHYs.  The
/// order mirrors the Linux PHY probe/remove lifetime: regulator requests are
/// acknowledged before GDSC/PHY reset work begins and are removed only after
/// the controller is stopped.  A failed Command DB lookup or RPMh ACK is a
/// hard failure for a cold start; the caller can then retain the bootloader's
/// already-owned state in the non-destructive handoff path.
pub unsafe fn apply_usb_power(enabled: bool, super_speed: bool) -> bool {
    let state = usb_resource_state();
    let current = state.power_rails_enabled;
    if current == enabled {
        return true;
    }
    // Disable the exact set that was enabled. Runtime suspend can be entered
    // after a SuperSpeed start even when QMP calibration later reports that
    // the controller is operating only on USB2.
    let actual_super_speed = if enabled {
        super_speed
    } else {
        state.power_super_speed
    };
    if enabled && !usb_power_contract_valid(actual_super_speed) {
        return false;
    }

    let power = usb_resources().power;
    let mut rails = [power.hs_phy_rails[0]; 5];
    rails[1] = power.hs_phy_rails[1];
    rails[2] = power.hs_phy_rails[2];
    let mut count = 3;
    if actual_super_speed {
        if power.qmp_vdd_present {
            rails[count] = power.qmp_vdd;
            count += 1;
        }
        rails[count] = power.qmp_core;
        count += 1;
    }

    // Distinct PHY consumers can still share an RPMh resource. De-duplicate
    // by ID so one request cannot be sent twice in a single TCS lifetime.
    let mut unique = [false; 5];
    for index in 0..count {
        unique[index] = true;
        for previous in 0..index {
            if rails[previous].rpmh_resource_id == rails[index].rpmh_resource_id {
                unique[index] = false;
                break;
            }
        }
    }

    if enabled {
        for index in 0..count {
            if unique[index] && !unsafe { send_usb_regulator_request(rails[index], true) } {
                for rollback in 0..index {
                    if unique[rollback] {
                        let _ = unsafe { send_usb_regulator_request(rails[rollback], false) };
                    }
                }
                return false;
            }
        }
    } else {
        for index in (0..count).rev() {
            if unique[index] && !unsafe { send_usb_regulator_request(rails[index], false) } {
                return false;
            }
        }
    }
    set_usb_resource_state(|state| {
        state.power_rails_enabled = enabled;
        state.power_super_speed = actual_super_speed;
    });
    true
}

/// Re-assert every RPMh vote the USB gadget domain depends on: the CX ARC
/// corner (`gcc` `vdd_cx-supply`), the interconnect BCM vectors
/// (`qcom,msm-bus,vectors-KBps`), and the PHY rail enables. Fastboot's votes
/// die with its exit and `apply_usb_power` early-returns once its state flag
/// matches, so the keepalive paths use this refresh instead: a VRM enable
/// request that never leaves the APSS cannot hold a domain whose CX corner
/// has already been demoted to the DT init level (RETENTION).
pub unsafe fn refresh_usb_domain_votes(vote: UsbBusVote, super_speed: bool) -> bool {
    let mut ok = unsafe { apply_usb_cx_vote(vote) };
    ok &= unsafe { send_usb_rpmh_vote(vote) };
    ok &= unsafe { refresh_usb_power(super_speed) };
    ok
}

/// Re-send the USB rail enable requests unconditionally. `apply_usb_power`
/// early-returns when its state flag already matches, which silently turns
/// the periodic keepalive into a no-op: the initial vote is the only vote
/// that ever leaves the APSS, and the ~5-8 s post-attach collapse then wins.
/// The refresh re-arms the exact same VRM requests without touching GDSC,
/// clock, or reset state, so it is safe to call from any park or poll loop.
pub unsafe fn refresh_usb_power(super_speed: bool) -> bool {
    let power = usb_resources().power;
    let mut rails = [power.hs_phy_rails[0]; 5];
    rails[1] = power.hs_phy_rails[1];
    rails[2] = power.hs_phy_rails[2];
    let mut count = 3;
    if super_speed && power.qmp_vdd_present {
        rails[count] = power.qmp_vdd;
        count += 1;
    }
    if super_speed {
        rails[count] = power.qmp_core;
        count += 1;
    }
    let mut unique = [false; 5];
    for index in 0..count {
        unique[index] = true;
        for previous in 0..index {
            if rails[previous].rpmh_resource_id == rails[index].rpmh_resource_id {
                unique[index] = false;
                break;
            }
        }
    }
    let mut ok = true;
    for index in 0..count {
        if unique[index] && !unsafe { send_usb_regulator_request(rails[index], true) } {
            ok = false;
        }
    }
    ok
}

/// Re-assert only the QMP PHY regulator consumers used by the Android
/// `msm_ssusb_qmp_ldo_enable(phy, 1)` path.  The Bramble DT supplies the QMP
/// `core` consumer (`pm8150_l9`) and has no separate `vdd-supply`; keeping
/// this narrower than `refresh_usb_power(true)` avoids perturbing the live
/// USB2 PHY during a SuperSpeed-only handoff.
pub unsafe fn refresh_usb_qmp_power() -> bool {
    let power = usb_resources().power;
    let mut ok = true;
    if power.qmp_vdd_present {
        ok &= unsafe { send_usb_regulator_request(power.qmp_vdd, true) };
    }
    ok &= unsafe { send_usb_regulator_request(power.qmp_core, true) };
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_lito_usb_resource_contract_matches_dt() {
        let resources = BRAMBLE_USB_RESOURCES;
        assert_eq!(resources.dwc3_base, 0x0a60_0000);
        assert_eq!(resources.dwc3_size, 0xcd00);
        assert_eq!(resources.gcc_base, GCC_BASE);
        assert_eq!(resources.qscratch_base, 0x0a6f_8800);
        assert_eq!(resources.hs_phy_base, 0x088e_3000);
        assert_eq!(resources.qmp_phy_base, 0x088e_8000);
        assert_eq!(resources.qmp_reg_offsets[0], 0x1c14);
        assert_eq!(resources.qmp_reg_offsets[1], 0x1f08);
        assert_eq!(resources.qmp_reg_offsets[2], 0x1f14);
        assert_eq!(resources.qmp_reg_offsets[8], 0x0008);
        assert_eq!(resources.qmp_reg_offsets[14], 0x1c8c);
        assert_eq!(resources.qmp_reg_offsets[15], 0x1c18);
        assert_eq!(resources.apps_smmu_base, 0x1500_0000);
        assert!(resources.smmu_use_3_level_tables);
        assert_eq!(resources.smmu_global_irq, 65);
        assert_eq!(resources.smmu_context_irq_count, SMMU_CONTEXT_IRQ_COUNT);
        assert_eq!(resources.smmu_context_irqs[0], 97);
        assert_eq!(resources.smmu_context_irqs[79], 409);
        assert_eq!(resources.pdc_base, 0x0b22_0000);
        assert_eq!(pdc_parent_irq(14), Some(494));
        assert_eq!(pdc_parent_irq(9), Some(489));
        assert_eq!(pdc_parent_irq(15), Some(495));
        assert_eq!(resources.gdsc, 0x10f004);
        assert_eq!(resources.dma_pool.iova_base, 0x9000_0000);
        assert_eq!(resources.dma_pool.size, 0x6000_0000);
        assert_eq!(resources.dma_pool.stream_id, 0xe0);
        assert_eq!(resources.gsi.event_buffer_count, 3);
        assert_eq!(resources.gsi.ring_base_high_offset, 0x144);
        assert!(resources.gsi.disable_io_coherency);
        assert_eq!(resources.rpmh_rsc.driver_base, 0x1822_0000);
        assert_eq!(resources.rpmh_rsc.tcs_offset, 0xd00);
        assert_eq!(resources.rpmh_rsc.driver_id, 2);
        assert_eq!(resources.rpmh_rsc.active_tcs_offset, 0);
        assert_eq!(resources.rpmh_rsc.sleep_tcs_offset, 2);
        assert_eq!(resources.rpmh_rsc.wake_tcs_offset, 5);
        assert_eq!(resources.command_db.base, 0x8086_0000);
        assert_eq!(resources.command_db.size, 0x20_000);
        assert_eq!(resources.pm_qos_latency_us, 61);
        assert_eq!(resources.bus_vote.mode_count, 4);
        assert_eq!(resources.bus_vote.path_count, 3);
        assert!(usb_power_contract_valid(true));
        assert_eq!(resources.power.hs_phy_rails[0].name, "vdd");
        assert_eq!(
            resources.power.hs_phy_rails[0].rpmh_resource_id,
            *b"ldoa5\0\0\0"
        );
        assert_eq!(resources.power.hs_phy_rails[0].min_uv, 880_000);
        // ldoa5 `qcom,mode-threshold-currents = <0 1>`: the 0.88 V rail must
        // be requested in HPM for any nonzero PHY load.
        assert_eq!(resources.power.hs_phy_rails[0].max_load_ua, 19_000);
        assert_eq!(resources.power.hs_phy_rails[1].min_uv, 1_704_000);
        assert_eq!(resources.power.hs_phy_rails[1].max_uv, 1_800_000);
        assert_eq!(resources.power.hs_phy_rails[1].max_load_ua, 19_000);
        assert_eq!(resources.power.hs_phy_rails[2].min_uv, 3_050_000);
        assert_eq!(resources.power.hs_phy_rails[2].max_uv, 3_300_000);
        assert_eq!(resources.power.hs_phy_rails[2].max_load_ua, 16_000);
        assert!(!resources.power.qmp_vdd_present);
        assert_eq!(resources.power.qmp_core.min_uv, 1_200_000);
        assert_eq!(resources.power.qmp_core.max_uv, 1_200_000);
        assert_eq!(resources.power.qmp_core.max_load_ua, 30_000);
        assert_eq!(resources.power.qmp_core.rpmh_resource_id, *b"ldoa9\0\0\0");
        assert!(resources.power.qmp_vbus_valid_override);
    }

    #[test]
    fn apps_smmu_fault_spis_are_usb_irq_sources() {
        assert!(is_usb_irq(65));
        assert!(is_usb_irq(97));
        assert!(is_usb_irq(409));
        assert!(is_usb_smmu_irq(65));
        assert!(is_usb_smmu_irq(97));
        assert!(!is_usb_smmu_irq(240));
    }

    #[test]
    fn runtime_state_requires_typec_before_controller_running() {
        assert_eq!(
            usb_runtime_transition(UsbRuntimeState::Off, UsbRuntimeEvent::PlatformPowered),
            UsbRuntimeState::Powered
        );
        assert_eq!(
            usb_runtime_transition(UsbRuntimeState::Powered, UsbRuntimeEvent::ControllerStarted),
            UsbRuntimeState::Powered
        );
        assert_eq!(
            usb_runtime_transition(UsbRuntimeState::Powered, UsbRuntimeEvent::TypecAttached),
            UsbRuntimeState::Attached
        );
        assert_eq!(
            usb_runtime_transition(
                UsbRuntimeState::Attached,
                UsbRuntimeEvent::ControllerStarted
            ),
            UsbRuntimeState::Running
        );
    }

    #[test]
    fn dt_regulator_names_map_to_strict_rpmh_resources() {
        assert_eq!(
            rpmh_resource_id_from_regulator_name(b"pm8150_l5", 9),
            Some(*b"ldoa5\0\0\0")
        );
        assert_eq!(
            rpmh_resource_id_from_regulator_name(b"pm8150b_l5", 10),
            Some(*b"ldob5\0\0\0")
        );
        assert_eq!(
            rpmh_resource_id_from_regulator_name(b"pm8150_l9", 9),
            Some(*b"ldoa9\0\0\0")
        );
        assert_eq!(
            rpmh_resource_id_from_regulator_name(b"pm8150_l18", 10),
            Some(*b"ldoa18\0\0")
        );
        assert_eq!(rpmh_resource_id_from_regulator_name(b"pm8998_l5", 9), None);
    }

    #[test]
    fn pm_qos_policy_tracks_android_nominal_and_suspend_contract() {
        let nominal = usb_pm_qos_policy(UsbBusVote::Nominal);
        assert_eq!(nominal.latency_us, 61);
        assert!(nominal.irq_affine);
        let applied = apply_usb_pm_qos(UsbBusVote::Nominal);
        assert_eq!(applied, nominal);
        assert_eq!(usb_resource_state().pm_qos_latency_us, 61);
        assert!(usb_resource_state().pm_qos_active);
        let suspend = usb_pm_qos_policy(UsbBusVote::Suspend);
        assert_eq!(suspend.latency_us, 0);
        assert!(!suspend.irq_affine);
        let _ = apply_usb_pm_qos(UsbBusVote::Suspend);
        assert!(!usb_resource_state().pm_qos_active);
    }

    #[test]
    fn compiled_lito_clock_and_reset_names_are_complete() {
        let resources = BRAMBLE_USB_RESOURCES;
        let clocks = resources.controller_clocks;
        assert_eq!(clocks[0].name, "core");
        assert_eq!(clocks[1].name, "iface");
        assert_eq!(clocks[2].name, "bus_aggr");
        assert_eq!(clocks[3].name, "utmi");
        assert_eq!(clocks[4].name, "sleep");
        assert_eq!(clocks[5].name, "xo");
        assert_eq!(clocks[0].normal_rate_hz, 133_333_333);
        assert_eq!(clocks[0].high_speed_rate_hz, 66_666_667);
        assert_eq!(clocks[3].normal_rate_hz, 60_000_000);
        assert_eq!(clocks[3].high_speed_rate_hz, 60_000_000);
        assert_eq!(clocks[5].normal_rate_hz, 19_200_000);

        let qmp = resources.qmp_clocks;
        assert_eq!(qmp[0].name, "aux");
        assert_eq!(qmp[0].source_offset, 0xf064);
        assert_eq!(qmp[1].name, "pipe");
        assert_eq!(qmp[2].name, "ref");
        assert_eq!(qmp[3].name, "com_aux");
        assert_eq!(resources.hs_phy_clock.name, "ref_clk_src");
        assert_eq!(resources.hs_phy_clock.provider, ClockProvider::Rpmh);
        assert_eq!(resources.hs_phy_clock.provider_id, 0);
        let resets = resources.resets;
        assert_eq!(resets[0].name, "core_reset");
        assert_eq!(resets[1].name, "qusb2phy_reset");
        assert_eq!(resets[2].name, "usb3phy_reset");
        assert_eq!(resets[3].name, "usb3dp_phy_reset");
    }

    #[test]
    fn dt_clock_reset_provider_ids_must_match_lito() {
        let mut contract = UsbDtContract::empty();
        assert!(usb_dt_clock_reset_contract_valid(&contract));

        contract.controller_clock_ids = [
            Some(114),
            Some(9),
            Some(130),
            Some(116),
            Some(118),
            Some(119),
        ];
        contract.qmp_clock_ids = [Some(120), Some(123), Some(0), Some(122)];
        contract.hs_phy_clock_id = Some(0);
        contract.reset_ids = [Some(13), Some(15), Some(17), Some(16)];
        assert!(usb_dt_clock_reset_contract_valid(&contract));

        contract.qmp_clock_ids[1] = Some(999);
        assert!(!usb_dt_clock_reset_contract_valid(&contract));
    }

    #[test]
    fn compiled_lito_irq_kinds_keep_pdc_out_of_gic() {
        let irqs = BRAMBLE_USB_RESOURCES.irqs;
        assert_eq!(irqs[0].kind, UsbIrqKind::Pdc);
        assert_eq!(irqs[0].trigger, IrqTrigger::RisingEdge);
        assert_eq!(
            irqs[1],
            IrqResource {
                name: "pwr_event_irq",
                number: 144,
                kind: UsbIrqKind::GicSpi,
            }
        );
        assert_eq!(irqs[2].kind, UsbIrqKind::Pdc);
        assert_eq!(irqs[2].trigger, IrqTrigger::LevelHigh);
        assert_eq!(irqs[3].kind, UsbIrqKind::Pdc);
        assert_eq!(irqs[3].trigger, IrqTrigger::RisingEdge);
        assert_eq!(irqs[4].number, 240);
        assert_eq!(irqs[4].kind, UsbIrqKind::GicSpi);
        assert_eq!(BRAMBLE_USB_RESOURCES.spmi_parent_irq, 481);
        assert_eq!(BRAMBLE_USB_RESOURCES.typec_irq.sid, 2);
        assert_eq!(BRAMBLE_USB_RESOURCES.typec_irq.peripheral_id, 0x15);
        assert_eq!(BRAMBLE_USB_RESOURCES.typec_irq.irq, 0);
        assert_eq!(
            BRAMBLE_USB_RESOURCES.typec_irq.trigger,
            IrqTrigger::RisingEdge
        );
    }

    #[test]
    fn cx_arc_vote_tracks_the_lito_gcc_rate_tables() {
        assert_eq!(RPMH_CXLVL, *b"cx.lvl\0\0");
        assert_eq!(usb_cx_level(UsbBusVote::Nominal), RPMH_REGULATOR_LEVEL_SVS);
        assert_eq!(usb_cx_level(UsbBusVote::Svs), RPMH_REGULATOR_LEVEL_LOW_SVS);
        assert_eq!(
            usb_cx_level(UsbBusVote::Suspend),
            RPMH_REGULATOR_LEVEL_LOW_SVS
        );
    }

    #[test]
    fn vrm_requests_use_the_reference_sub_addresses() {
        // drivers/regulator/qcom-rpmh-regulator.c:
        // RPMH_REGULATOR_REG_VRM_VOLTAGE/ENABLE/MODE = 0x0/0x4/0x8.
        assert_eq!(RPMH_REGULATOR_VRM_VOLTAGE, 0x0);
        assert_eq!(RPMH_REGULATOR_ENABLE, 0x4);
        assert_eq!(RPMH_REGULATOR_MODE, 0x8);
    }

    #[test]
    fn typec_spmi_parent_is_a_usb_irq_and_polarity_matches_dt() {
        assert!(is_usb_irq(481));
        assert_eq!(spmi_irq_polarity(IrqTrigger::RisingEdge), (1, 1, 0));
        assert_eq!(spmi_irq_polarity(IrqTrigger::EdgeBoth), (1, 1, 1));
        assert_eq!(spmi_irq_polarity(IrqTrigger::LevelHigh), (0, 1, 0));
    }

    #[test]
    fn pdc_register_layout_matches_supported_linux_revisions() {
        let v27 = pdc_register_layout(0x0207_00);
        assert_eq!(v27.enable_bank, Some(PDC_IRQ_ENABLE_BANK));
        assert_eq!(v27.irq_enable_bit, None);

        let v30 = pdc_register_layout(0x0300_00);
        assert_eq!(v30.enable_bank, Some(PDC_IRQ_ENABLE_BANK));
        assert_eq!(v30.irq_enable_bit, None);

        let v32 = pdc_register_layout(PDC_VERSION_3_2);
        assert_eq!(v32.enable_bank, Some(PDC_IRQ_ENABLE_BANK));
        assert_eq!(v32.irq_enable_bit, None);
    }

    #[test]
    fn rpmh_bcm_command_matches_legacy_tcs_encoding() {
        assert_eq!(
            encode_rpmh_bcm_command(0x1234_0000, true, true, 2_500_000, 1_000_000),
            RpmhBcmCommand {
                address: 0x1234_0000,
                data: 0x6fff_ffff,
            }
        );
        assert_eq!(
            encode_rpmh_bcm_command(0x1234_0000, false, false, 0, 0),
            RpmhBcmCommand {
                address: 0x1234_0000,
                data: 0,
            }
        );
    }

    #[test]
    fn rpmh_register_layout_tracks_controller_major_revision() {
        let v27 = rpmh_register_layout(0x0207_00).unwrap();
        assert_eq!(v27.command_stride, 20);
        assert_eq!(v27.wait_for_completion, 0x10);
        assert_eq!(v27.control, 0x14);
        assert_eq!(v27.command_status, 0x3c);

        let v30 = rpmh_register_layout(0x0300_00).unwrap();
        assert_eq!(v30.command_stride, 24);
        assert_eq!(v30.wait_for_completion, 0x20);
        assert_eq!(v30.control, 0x24);
        assert_eq!(v30.command_status, 0x40);

        // An invalid read must not be interpreted as v2.7 and must never
        // result in a speculative write to the secure-owned RSC block.
        assert_eq!(rpmh_register_layout(0), None);
    }

    #[test]
    fn usb_bus_paths_collapse_to_lito_bcm_topology() {
        let votes = usb_bcm_votes(UsbBusVote::Nominal);
        assert_eq!(votes.count, 4);
        assert_eq!(votes.votes[0].id, *b"MC0\0\0\0\0\0");
        assert_eq!(votes.votes[1].id, *b"ACV\0\0\0\0\0");
        assert_eq!(votes.votes[2].id, *b"CN0\0\0\0\0\0");
        assert_eq!(votes.votes[3].id, *b"SH4\0\0\0\0\0");
        assert_eq!(votes.votes[0].average_kbps, 240_000);
        assert_eq!(votes.votes[0].instantaneous_kbps, 700_000);
        assert_eq!(votes.votes[2].instantaneous_kbps, 40_000);
        assert_eq!(votes.votes[3].instantaneous_kbps, 40_000);

        let svs = usb_bcm_votes(UsbBusVote::Svs);
        assert_eq!(svs.votes[0].average_kbps, 1_000_000);
        assert_eq!(svs.votes[0].instantaneous_kbps, 2_500_000);
    }

    #[test]
    fn performance_states_match_android_nominal_svs_and_off_policy() {
        let nominal = usb_performance_state(UsbBusVote::Nominal);
        assert_eq!(nominal.core_rate_hz, 133_333_333);
        assert_eq!(nominal.pm_qos_latency_us, 61);
        assert_eq!(
            usb_clock_plan(UsbBusVote::Nominal),
            UsbClockPlan {
                core_parent: 1,
                core_divider: 8,
                utmi_parent: 0,
                utmi_divider: 1,
                branches_enabled: true,
            }
        );

        let svs = usb_performance_state(UsbBusVote::Svs);
        assert_eq!(svs.core_rate_hz, 66_666_667);
        assert_eq!(svs.pm_qos_latency_us, 0);
        assert_eq!(usb_clock_plan(UsbBusVote::Svs).core_parent, 6);
        assert!(usb_pm_qos_policy(UsbBusVote::Svs).irq_affine);

        let suspend = usb_performance_state(UsbBusVote::Suspend);
        assert_eq!(suspend.core_rate_hz, 0);
        assert_eq!(suspend.pm_qos_latency_us, 0);
        assert!(!usb_clock_plan(UsbBusVote::Suspend).branches_enabled);
        assert!(!usb_pm_qos_policy(UsbBusVote::Suspend).irq_affine);
    }

    #[test]
    fn dma_region_validation_enforces_pool_bounds_and_alignment() {
        let pool = DmaPoolResource {
            iova_base: 0x9000_0000,
            size: 0x1000,
            stream_id: 0xe0,
        };
        assert!(dma_region_valid(pool, 0x9000_0000, 0x100, 0x40));
        assert!(dma_region_valid(pool, 0x9000_0f00, 0x100, 0x100));
        assert!(!dma_region_valid(pool, 0x9000_0001, 0x100, 0x40));
        assert!(!dma_region_valid(pool, 0x9000_0f80, 0x100, 0x40));
        assert!(!dma_region_valid(pool, 0x8fff_f000, 0x100, 0x100));
        assert!(!dma_region_valid(pool, 0x9000_0000, 0, 0x40));
    }

    #[test]
    fn dma_pool_allocator_is_monotonic_and_aligned() {
        let pool = DmaPoolResource {
            iova_base: 0x9000_0000,
            size: 0x1000,
            stream_id: 0xe0,
        };
        let mut allocator = DmaPoolAllocator::new(pool, 0x9000_0101).unwrap();
        assert_eq!(allocator.allocate(0x20, 0x40), Some(0x9000_0140));
        assert_eq!(allocator.allocate(0x10, 0x100), Some(0x9000_0200));
        assert_eq!(allocator.allocate(0x1000, 0x40), None);
        assert!(allocator.remaining() < 0x1000);
        assert_eq!(DmaPoolAllocator::new(pool, 0x9000_1000), None);
    }

    #[test]
    fn command_db_lookup_obeys_linux_header_layout_and_bounds() {
        let mut db = [0u8; 256];
        db[4..8].copy_from_slice(&CMD_DB_MAGIC);
        // One BCM resource header at offset 16, with one entry whose header
        // starts immediately after the fixed command-DB header.
        db[16] = 3;
        db[18] = 0;
        db[22] = 1;
        let entry = CMD_DB_HEADER_BYTES;
        db[entry..entry + 8].copy_from_slice(b"MC0\0\0\0\0\0");
        db[entry + 16..entry + 20].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        assert_eq!(
            command_db_lookup(db.len(), |offset| db.get(offset).copied(), b"MC0\0\0\0\0\0"),
            Some(0x1234_5678)
        );
        assert_eq!(
            command_db_lookup(db.len(), |offset| db.get(offset).copied(), b"CN0\0\0\0\0\0"),
            None
        );
    }

    #[test]
    fn typec_state_machine_requires_sink_selection_before_attach() {
        assert_eq!(
            typec_transition(TypecPhase::Disabled, TypecEvent::AttachDetected),
            TypecPhase::Disabled
        );
        let phase = typec_transition(TypecPhase::Disabled, TypecEvent::SinkOnlySelected);
        assert_eq!(phase, TypecPhase::SinkOnlyRequested);
        let phase = typec_transition(phase, TypecEvent::DetachDetected);
        assert_eq!(phase, TypecPhase::WaitingAttach);
        let phase = typec_transition(phase, TypecEvent::AttachDetected);
        assert_eq!(phase, TypecPhase::Attached);
        assert_eq!(
            typec_transition(phase, TypecEvent::DetachDetected),
            TypecPhase::Detached
        );

        let phase = typec_transition(TypecPhase::SinkOnlyRequested, TypecEvent::HostDetected);
        assert_eq!(phase, TypecPhase::Host);
        assert_eq!(
            typec_transition(phase, TypecEvent::DetachDetected),
            TypecPhase::Detached
        );
    }

    #[test]
    fn typec_role_distinguishes_unattached_device_and_host() {
        assert_eq!(typec_role(0), UsbRole::None);
        assert_eq!(typec_role(TYPEC_CC_ATTACHED), UsbRole::Device);
        assert_eq!(
            typec_role(TYPEC_CC_ATTACHED | TYPEC_SNK_SRC_MODE),
            UsbRole::Host
        );
    }
}
