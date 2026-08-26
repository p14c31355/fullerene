/// Qualcomm SM7250 / Pixel 4a 5G (bramble) early-boot addresses.
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
pub const USB_PDC_DP_HS_PARENT_IRQ: u32 = 494;
pub const USB_PDC_SS_PARENT_IRQ: u32 = 489;
pub const USB_PDC_DM_HS_PARENT_IRQ: u32 = 495;

/// GCC register block and USB3 power-domain registers from the Lito DT.
pub const GCC_BASE: usize = 0x0010_0000;
/// Unlike GCC branch registers, the GDSC node is exposed at an absolute SoC
/// address in the DT (`reg = <0x10f004 0x4>`), not as an offset from GCC_BASE.
pub const USB30_PRIM_GDSC: usize = 0x10f004;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockResource {
    pub name: &'static str,
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
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbIrqKind {
    Pdc,
    GicSpi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrqTrigger {
    RisingEdge,
    LevelHigh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqResource {
    pub name: &'static str,
    pub number: u32,
    pub kind: UsbIrqKind,
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
    pub vectors: &'static [[BusVoteVector; 3]; 4],
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandDbResource {
    pub base: usize,
    pub size: usize,
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
        data: ((commit as u32) << 30)
            | ((valid as u32) << 29)
            | (x << 14)
            | y,
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
            Self::Nominal => 1,
            Self::Svs => 2,
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

/// Resolve the Android glue's performance state without performing an
/// interconnect transaction. The transport for RPMh/msm-bus is a separate
/// firmware interface; keeping the resolution pure prevents an unverified
/// MMIO write from corrupting the bootloader's active vote.
pub const fn usb_performance_state(vote: UsbBusVote) -> UsbPerformanceState {
    let core_rate_hz = match vote {
        UsbBusVote::Nominal => 133_333_333,
        UsbBusVote::Svs => 66_666_667,
        UsbBusVote::Suspend | UsbBusVote::Minimum => 0,
    };
    UsbPerformanceState {
        vote,
        pm_qos_latency_us: BRAMBLE_USB_RESOURCES.pm_qos_latency_us,
        core_rate_hz,
    }
}

pub const fn usb_bus_vectors(vote: UsbBusVote) -> &'static [BusVoteVector; 3] {
    &BRAMBLE_BUS_VECTORS[vote.index()]
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

/// Switch the GCC branch resources required by the Qualcomm glue. This is
/// intentionally platform-owned: DWC3 should not need to know GCC offsets or
/// which of the six controller clocks are present in the DT.
pub unsafe fn enable_usb_clock_branches() -> bool {
    let mut ok = true;
    for clock in BRAMBLE_USB_RESOURCES.controller_clocks {
        let address = (GCC_BASE + clock.branch_offset) as *mut u32;
        let value = unsafe { core::ptr::read_volatile(address) } | 1;
        unsafe { core::ptr::write_volatile(address, value) };
        ok &= unsafe { core::ptr::read_volatile(address) & 1 != 0 };
    }
    for clock in BRAMBLE_USB_RESOURCES.qmp_clocks {
        let address = (GCC_BASE + clock.branch_offset) as *mut u32;
        let value = unsafe { core::ptr::read_volatile(address) } | 1;
        unsafe { core::ptr::write_volatile(address, value) };
        ok &= unsafe { core::ptr::read_volatile(address) & 1 != 0 };
    }
    ok
}

/// Assert and release every reset exposed by the Lito USB node. The caller
/// controls the surrounding power-domain/clock ordering; this function only
/// performs the DT-described reset resources and reports readback failures.
pub unsafe fn reset_usb_blocks(super_speed: bool) -> bool {
    let mut ok = true;
    for (index, reset) in BRAMBLE_USB_RESOURCES.resets.iter().enumerate() {
        if !super_speed && index >= 2 {
            continue;
        }
        let address = (GCC_BASE + reset.offset) as *mut u32;
        let asserted = unsafe { core::ptr::read_volatile(address) } | 1;
        unsafe { core::ptr::write_volatile(address, asserted) };
        ok &= unsafe { core::ptr::read_volatile(address) & 1 != 0 };
        for _ in 0..250_000 {
            unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
        }
        unsafe { core::ptr::write_volatile(address, asserted & !1) };
        ok &= unsafe { core::ptr::read_volatile(address) & 1 == 0 };
    }
    ok
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbPlatformResources {
    pub dwc3_base: usize,
    pub dwc3_size: usize,
    pub qscratch_base: usize,
    pub hs_phy_base: usize,
    pub qmp_phy_base: usize,
    pub apps_smmu_base: usize,
    pub pdc_base: usize,
    pub gdsc: usize,
    pub controller_clocks: &'static [ClockResource; 6],
    pub qmp_clocks: &'static [ClockResource; 4],
    pub resets: &'static [ResetResource; 4],
    pub irqs: &'static [IrqResource; 5],
    pub dma_pool: DmaPoolResource,
    pub gsi: GsiResource,
    pub rpmh_rsc: RpmhRscResource,
    pub command_db: CommandDbResource,
    pub pm_qos_latency_us: u32,
    pub bus_vote: BusVoteResource,
}

const BRAMBLE_CONTROLLER_CLOCKS: [ClockResource; 6] = [
    ClockResource {
        name: "core",
        branch_offset: 0xf010,
        source_offset: 0xf020,
        normal_rate_hz: 133_333_333,
        high_speed_rate_hz: 66_666_667,
    },
    ClockResource {
        name: "iface",
        branch_offset: 0xf07c,
        source_offset: 0,
        normal_rate_hz: 0,
        high_speed_rate_hz: 0,
    },
    ClockResource {
        name: "bus_aggr",
        branch_offset: 0xf080,
        source_offset: 0,
        normal_rate_hz: 0,
        high_speed_rate_hz: 0,
    },
    ClockResource {
        name: "utmi",
        branch_offset: 0xf01c,
        source_offset: 0xf038,
        normal_rate_hz: 66_666_667,
        high_speed_rate_hz: 66_666_667,
    },
    ClockResource {
        name: "sleep",
        branch_offset: 0xf018,
        source_offset: 0,
        normal_rate_hz: 0,
        high_speed_rate_hz: 0,
    },
    ClockResource {
        name: "xo",
        branch_offset: 0x8c010,
        source_offset: 0,
        normal_rate_hz: 19_200_000,
        high_speed_rate_hz: 19_200_000,
    },
];

const BRAMBLE_QMP_CLOCKS: [ClockResource; 4] = [
    ClockResource {
        name: "aux",
        branch_offset: 0xf054,
        source_offset: 0,
        normal_rate_hz: 0,
        high_speed_rate_hz: 0,
    },
    ClockResource {
        name: "ref",
        branch_offset: 0x8c010,
        source_offset: 0,
        normal_rate_hz: 19_200_000,
        high_speed_rate_hz: 19_200_000,
    },
    ClockResource {
        name: "com_aux",
        branch_offset: 0xf058,
        source_offset: 0,
        normal_rate_hz: 0,
        high_speed_rate_hz: 0,
    },
    ClockResource {
        name: "pipe",
        branch_offset: 0xf05c,
        source_offset: 0,
        normal_rate_hz: 125_000_000,
        high_speed_rate_hz: 125_000_000,
    },
];

const BRAMBLE_USB_RESETS: [ResetResource; 4] = [
    ResetResource {
        name: "core_reset",
        offset: 0xf000,
    },
    ResetResource {
        name: "qusb2phy_reset",
        offset: 0x12000,
    },
    ResetResource {
        name: "usb3phy_reset",
        offset: 0x50000,
    },
    ResetResource {
        name: "usb3dp_phy_reset",
        offset: 0x50008,
    },
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

/// Compiled fallback for the Android Lito/Bramble DT. A future DT parser can
/// replace this value at boot, but all platform users must consume this
/// resource contract rather than growing another hard-coded MMIO list.
pub const BRAMBLE_USB_RESOURCES: UsbPlatformResources = UsbPlatformResources {
    dwc3_base: 0x0a60_0000,
    dwc3_size: 0xcd00,
    qscratch_base: 0x0a6f_8800,
    hs_phy_base: 0x088e_3000,
    qmp_phy_base: 0x088e_8000,
    apps_smmu_base: 0x0c60_0000,
    pdc_base: USB_PDC_BASE,
    gdsc: USB30_PRIM_GDSC,
    controller_clocks: &BRAMBLE_CONTROLLER_CLOCKS,
    qmp_clocks: &BRAMBLE_QMP_CLOCKS,
    resets: &BRAMBLE_USB_RESETS,
    irqs: &BRAMBLE_USB_IRQS,
    dma_pool: DmaPoolResource {
        iova_base: 0x9000_0000,
        size: 0x6000_0000,
        stream_id: 0xe0,
    },
    gsi: BRAMBLE_GSI,
    rpmh_rsc: BRAMBLE_RPMH_RSC,
    command_db: BRAMBLE_COMMAND_DB,
    pm_qos_latency_us: 61,
    bus_vote: BusVoteResource {
        mode_count: 4,
        path_count: 3,
        vectors: &BRAMBLE_BUS_VECTORS,
    },
};
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
const TYPEC_MISC_STATUS: u16 = PM8150B_TYPEC_BASE + 0x0b;
const TYPEC_MODE_CFG: u16 = PM8150B_TYPEC_BASE + 0x44;
const TYPEC_CC_ATTACHED: u8 = 1 << 0;
const TYPEC_CC_ORIENTATION: u8 = 1 << 1;
const TYPEC_DISABLE_CMD: u8 = 1 << 0;
const TYPEC_EN_SNK_ONLY: u8 = 1 << 1;
const TYPEC_EN_SRC_ONLY: u8 = 1 << 2;

const PDC_IRQ_ENABLE_BANK: usize = 0x10;
const PDC_IRQ_CONFIG: usize = 0x110;
const PDC_IRQ_CONFIG_MASK: u32 = 0x7;
const PDC_LEVEL_HIGH: u32 = 0b100;
const PDC_EDGE_RISING: u32 = 0b110;

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
    interrupt_id == USB_DWC3_IRQ
        || interrupt_id == USB_PWR_EVENT_IRQ
        || interrupt_id == USB_PDC_DP_HS_PARENT_IRQ
        || interrupt_id == USB_PDC_SS_PARENT_IRQ
        || interrupt_id == USB_PDC_DM_HS_PARENT_IRQ
}

#[derive(Clone, Copy)]
pub struct TypecState {
    pub arbiter_version: u32,
    pub misc_status: u8,
    pub mode: u8,
    pub orientation_reverse: bool,
    pub sink_mode_written: bool,
    pub attached: bool,
    pub attach_settled: bool,
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
    unsafe {
        if version >= 0x5000_0000 {
            // v5 has a flat APID -> PPID table.  Multiple APIDs can refer to
            // one peripheral; prefer one owned by execution environment 0 so
            // that a write cannot be silently rejected by the arbiter.
            let mut fallback = None;
            for apid in 0..512usize {
                let entry = spmi_read(SPMI_CORE, SPMI_APID_MAP_V5 + apid * 4);
                if ((entry >> 8) & 0x0fff) as u16 != PM8150B_TYPEC_PPID {
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
            let one = (PM8150B_TYPEC_PPID & (1 << bit)) != 0;
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

/// Read PM8150B Type-C state and select sink-only mode for a host-connected
/// phone.  This is intentionally a small, synchronous handoff operation: it
/// does not install the PMIC interrupt controller or pretend to replace the
/// full Linux Type-C state machine.
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
        let requested = (mode & !(TYPEC_EN_SNK_ONLY | TYPEC_EN_SRC_ONLY)) | TYPEC_EN_SNK_ONLY;
        // The upstream Qualcomm PMIC Type-C driver forces the state machine
        // through DISABLE before selecting a new power role. A same-value
        // write is not sufficient after Fastboot has torn down its gadget:
        // the PMIC can retain sink-only in the register while its attach
        // evaluation remains stopped.
        let mut disable = TYPEC_DISABLE_CMD;
        let disabled = unsafe { spmi_transfer(version, apid, TYPEC_MODE_CFG, &mut disable, true) };
        let mut new_mode = requested;
        sink_mode_written = disabled
            && unsafe { spmi_transfer(version, apid, TYPEC_MODE_CFG, &mut new_mode, true) };
        if sink_mode_written {
            mode = requested;
        }
    }

    // Selecting sink-only is only the role request. Qualcomm's Type-C glue
    // then waits for CC attach before asserting the VBUS/session override.
    // Fastboot handoff briefly reports detached while it tears down its own
    // gadget, so use a bounded poll and preserve the last observed state in
    // the returned contract instead of treating the role write as an attach.
    let mut attached = misc_status & TYPEC_CC_ATTACHED != 0;
    let mut attach_settled = attached;
    if sink_mode_written && !attached {
        for _ in 0..32 {
            let mut status = 0u8;
            if !unsafe { spmi_transfer(version, apid, TYPEC_MISC_STATUS, &mut status, false) } {
                break;
            }
            misc_status = status;
            attached = status & TYPEC_CC_ATTACHED != 0;
            if attached {
                attach_settled = true;
                break;
            }
            for _ in 0..10_000 {
                unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
            }
        }
    }
    Some(TypecState {
        arbiter_version: version,
        misc_status,
        mode,
        orientation_reverse: misc_status & TYPEC_CC_ORIENTATION != 0,
        sink_mode_written,
        attached,
        attach_settled,
    })
}

unsafe fn configure_pdc_irq(irq: IrqResource) -> bool {
    let base = BRAMBLE_USB_RESOURCES.pdc_base;
    let config = (irq.trigger == IrqTrigger::RisingEdge)
        .then_some(PDC_EDGE_RISING)
        .unwrap_or(PDC_LEVEL_HIGH);
    let config_address = (base + PDC_IRQ_CONFIG + irq.number as usize * 4) as *mut u32;
    let mut value = unsafe { core::ptr::read_volatile(config_address) };
    value = (value & !PDC_IRQ_CONFIG_MASK) | config;
    unsafe { core::ptr::write_volatile(config_address, value) };
    let _ = unsafe { core::ptr::read_volatile(config_address) };

    let bank_address = (base + PDC_IRQ_ENABLE_BANK + (irq.number as usize / 32) * 4) as *mut u32;
    let mut bank = unsafe { core::ptr::read_volatile(bank_address) };
    bank |= 1 << (irq.number % 32);
    unsafe { core::ptr::write_volatile(bank_address, bank) };
    unsafe { core::ptr::read_volatile(bank_address) & (1 << (irq.number % 32)) != 0 }
}

/// Program the three USB PDC pins exactly as the Qualcomm PDC irqchip does:
/// configure the polarity/trigger in the PDC, then unmask the pin. The parent
/// GIC SPIs are enabled separately after the GIC redistributor is awake.
pub unsafe fn configure_usb_pdc_irqs() -> bool {
    let mut ok = true;
    for irq in BRAMBLE_USB_RESOURCES.irqs {
        if irq.kind == UsbIrqKind::Pdc {
            ok &= unsafe { configure_pdc_irq(*irq) };
        }
    }
    ok
}

/// Enable the USB3 GDSC using the same software-controlled sequence as the
/// Qualcomm GDSC regulator driver. The parent RPMh supplies are intentionally
/// not touched here: those rails are controlled by secure firmware and are
/// already enabled by the Pixel boot chain for a temporary boot image.
pub unsafe fn enable_usb30_gdsc() -> bool {
    let address = BRAMBLE_USB_RESOURCES.gdsc as *mut u32;
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
            return true;
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    false
}

const GCC_CMD_UPDATE: u32 = 1 << 0;
const GCC_CFG_SRC_DIV_MASK: u32 = 0xff;
const GCC_CFG_SRC_SEL_MASK: u32 = 0x7 << 8;

#[inline]
unsafe fn gcc_reg(offset: usize) -> *mut u32 {
    (GCC_BASE + offset) as *mut u32
}

/// Program one Qualcomm RCG2 clock source and commit the change.
///
/// The Lito GCC driver describes the USB master clock as parent 1 divided by
/// 8 and the mock UTMI clock as parent 0 with no divider. Keeping this in the platform
/// layer prevents the DWC3 driver from depending on GCC register layout.
unsafe fn configure_rcg(cmd_offset: usize, parent: u32, divider: u32) -> bool {
    unsafe {
        let cfg = gcc_reg(cmd_offset + 0x4);
        let mut value = core::ptr::read_volatile(cfg);
        value &= !(GCC_CFG_SRC_DIV_MASK | GCC_CFG_SRC_SEL_MASK);
        value |= divider & GCC_CFG_SRC_DIV_MASK;
        value |= (parent << 8) & GCC_CFG_SRC_SEL_MASK;
        core::ptr::write_volatile(cfg, value);
        let _ = core::ptr::read_volatile(cfg);

        let cmd = gcc_reg(cmd_offset);
        let value = core::ptr::read_volatile(cmd) | GCC_CMD_UPDATE;
        core::ptr::write_volatile(cmd, value);
        let _ = core::ptr::read_volatile(cmd);

        for _ in 0..500_000u32 {
            if core::ptr::read_volatile(cmd) & GCC_CMD_UPDATE == 0 {
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
    false
}

/// Select the rates required by the Lito USB glue before its branch clocks
/// are enabled. The values mirror `qcom,core-clk-rate = 133333333` and
/// `qcom,core-clk-rate-hs = 66666667`'s source tables.
pub unsafe fn configure_usb_clocks() -> bool {
    unsafe {
        let core = BRAMBLE_USB_RESOURCES.controller_clocks[0];
        let utmi = BRAMBLE_USB_RESOURCES.controller_clocks[3];
        // gcc_usb30_prim_master_clk_src.
        if core.source_offset == 0 || !configure_rcg(core.source_offset, 1, 8) {
            return false;
        }
        // gcc_usb30_prim_mock_utmi_clk_src.
        utmi.source_offset != 0 && configure_rcg(utmi.source_offset, 0, 0)
    }
}

pub fn init_interrupt_controller(gicd_base: Option<usize>, gicr_base: Option<usize>) {
    let gicd = gicd_base.unwrap_or(GICD_BASE);
    let gicr = gicr_base.unwrap_or(GICR_BASE);
    unsafe {
        let _ = configure_usb_pdc_irqs();
    }
    super::gicv3::init(gicd, gicr, Some(USB_DWC3_IRQ));
    unsafe {
        super::gicv3::enable_spis(
            gicd,
            &[
                USB_PWR_EVENT_IRQ,
                USB_PDC_DP_HS_PARENT_IRQ,
                USB_PDC_SS_PARENT_IRQ,
                USB_PDC_DM_HS_PARENT_IRQ,
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_lito_usb_resource_contract_matches_dt() {
        let resources = BRAMBLE_USB_RESOURCES;
        assert_eq!(resources.dwc3_base, 0x0a60_0000);
        assert_eq!(resources.dwc3_size, 0xcd00);
        assert_eq!(resources.qscratch_base, 0x0a6f_8800);
        assert_eq!(resources.hs_phy_base, 0x088e_3000);
        assert_eq!(resources.qmp_phy_base, 0x088e_8000);
        assert_eq!(resources.apps_smmu_base, 0x0c60_0000);
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
        assert_eq!(clocks[5].normal_rate_hz, 19_200_000);

        let qmp = resources.qmp_clocks;
        assert_eq!(qmp[0].name, "aux");
        assert_eq!(qmp[1].name, "ref");
        assert_eq!(qmp[2].name, "com_aux");
        assert_eq!(qmp[3].name, "pipe");
        let resets = resources.resets;
        assert_eq!(resets[0].name, "core_reset");
        assert_eq!(resets[1].name, "qusb2phy_reset");
        assert_eq!(resets[2].name, "usb3phy_reset");
        assert_eq!(resets[3].name, "usb3dp_phy_reset");
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
}
