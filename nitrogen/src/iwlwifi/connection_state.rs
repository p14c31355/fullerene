//! Connection state, incremental device initialization, and public Wi-Fi API.

use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::sync::atomic::AtomicBool;
use spin::Mutex;

use crate::DriverContext;
use crate::debug;
use crate::mmio::{self, DmaRegion};
use crate::pci_health::PciHealth;
use bonder::wifi::{self, AccessPoint, Ssid};

use super::device::IwlWifiDevice;
use super::firmware::select_firmware_list;
use super::registers::*;
use super::types::*;

// ── Global driver context for DMA ──

static WIFI_DRIVER_CTX: Mutex<Option<&'static dyn DriverContext>> = Mutex::new(None);

pub fn set_wifi_driver_context(ctx: &'static dyn DriverContext) {
    *WIFI_DRIVER_CTX.lock() = Some(ctx);
}

// ── Stored wifi state for external access ──

static WIFI_MANAGER: Mutex<Option<WifiManager>> = Mutex::new(None);
static WIFI_DEVICE: Mutex<Option<Box<dyn crate::wifi::WifiDriver>>> = Mutex::new(None);
static WIFI_INIT_COMPLETED: AtomicBool = AtomicBool::new(false);
static WIFI_INIT_PHASE: core::sync::atomic::AtomicU8 =
    core::sync::atomic::AtomicU8::new(WifiInitPhase::Idle as u8);
static WIFI_INIT_CTX: Mutex<WifiInitContext> = Mutex::new(WifiInitContext {
    mmio_device: None,
    fw_candidate_idx: 0,
    fw_candidates: &[],
    alive_start_tsc: 0,
    pci_dev: None,
    mmio: core::ptr::null_mut(),
    driver_ctx: None,
    health: None,
    hw_rev: 0,
    mac: None,
    tx_dma_ring: None,
    rx_dma_ring: None,
    tx_bufs: Vec::new(),
    rx_bufs: Vec::new(),
});

pub fn wifi_init_completed() -> bool {
    WIFI_INIT_COMPLETED.load(core::sync::atomic::Ordering::Acquire)
}

/// Force WiFi init to be marked as failed/completed.
///
/// Called by the kernel when the incremental init has been running
/// too long (PCIe MMIO hangs on real hardware) to prevent the idle
/// loop from being permanently blocked.
///
/// Uses `try_lock` so that it can be called from the NMI watchdog
/// path without deadlocking (the lock may be held by the hung context).
pub fn force_init_failed() {
    if WIFI_INIT_COMPLETED.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    // Always mark as failed (lock-free statics) so the tick loop
    // stops attempting init, even if the Mutex is perma-locked.
    set_init_phase(WifiInitPhase::Failed);
    WIFI_INIT_COMPLETED.store(true, core::sync::atomic::Ordering::Release);

    // Try to clean up resources – skip if lock is held by hung context.
    let mut ctx = match WIFI_INIT_CTX.try_lock() {
        Some(c) => c,
        None => {
            debug::print(
                "iwlwifi",
                "step: force_init_failed (lock held, skip cleanup)",
            );
            return;
        }
    };
    let _ = ctx.mmio_device.take();
    // Disable PCI bus-mastering before freeing DMA regions
    if let Some(ref pci) = ctx.pci_dev {
        let cmd =
            crate::pci::PciConfigSpace::read_config_word(pci.bus, pci.device, pci.function, 4);
        crate::pci::PciConfigSpace::write_config_word_raw(
            pci.bus,
            pci.device,
            pci.function,
            4,
            cmd & !0x04,
        );
    }
    let drv = ctx.driver_ctx;
    for mut buf in ctx.tx_bufs.drain(..) {
        if let Some(c) = drv {
            buf.free(c);
        }
    }
    for mut buf in ctx.rx_bufs.drain(..) {
        if let Some(c) = drv {
            buf.free(c);
        }
    }
    if let Some(mut ring) = ctx.tx_dma_ring.take() {
        if let Some(c) = drv {
            ring.free(c);
        }
    }
    if let Some(mut ring) = ctx.rx_dma_ring.take() {
        if let Some(c) = drv {
            ring.free(c);
        }
    }
    drop(ctx);
    debug::print("iwlwifi", "step: force_init_failed (timeout)");
}

fn set_init_phase(phase: WifiInitPhase) {
    WIFI_INIT_PHASE.store(phase as u8, core::sync::atomic::Ordering::Release);
}

fn get_init_phase() -> WifiInitPhase {
    let raw = WIFI_INIT_PHASE.load(core::sync::atomic::Ordering::Acquire);
    WifiInitPhase::from(raw)
}

// ── Incremental init state machine ─

pub fn try_init_wifi_device_step() {
    let phase = get_init_phase();
    debug::print("iwlwifi", &alloc::format!("step: phase={}", phase as u8));

    match phase {
        WifiInitPhase::Idle => {
            log::info!("iwlwifi: initialization state machine starting");
            let driver_ctx_opt = WIFI_DRIVER_CTX.lock();
            let _driver_ctx = match *driver_ctx_opt {
                Some(c) => c,
                None => {
                    log::error!("iwlwifi: initialization failed — driver context is unavailable");
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
            };
            drop(driver_ctx_opt);

            let dev_guard = WIFI_DEVICE.lock();
            if dev_guard.is_some() {
                debug::print("iwlwifi", "step: already_inited");
                set_init_phase(WifiInitPhase::Done);
                return;
            }
            drop(dev_guard);

            debug::print("iwlwifi", "step: start pci_probe");
            set_init_phase(WifiInitPhase::PciProbe);
        }
        WifiInitPhase::PciProbe => {
            log::info!("iwlwifi: PCI probe phase started");
            debug::print("iwlwifi", "step: pci_probe_enter");
            let driver_ctx = match *WIFI_DRIVER_CTX.lock() {
                Some(c) => c,
                None => {
                    log::error!("iwlwifi: PCI probe failed — driver context is unavailable");
                    debug::print("iwlwifi", "step: ERR no_driver_ctx");
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
            };
            debug::print("iwlwifi", "step: call probe_pci_only");
            let raw = match crate::wifi::probe_pci_only(driver_ctx) {
                Some(r) => r,
                None => {
                    log::warn!("iwlwifi: PCI probe found no usable supported device");
                    debug::print("iwlwifi", "step: no_pci_device");
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
            };
            log::info!(
                "iwlwifi: PCI device matched {:04x}:{:04x} at {:02x}:{:02x}.{} BAR0={:#x} hw_rev={:#06x}",
                raw.pci_dev.vendor_id,
                raw.device_id,
                raw.pci_dev.bus,
                raw.pci_dev.device,
                raw.pci_dev.function,
                raw.pci_dev
                    .read_bar_info(0)
                    .map(|bar| bar.address)
                    .unwrap_or(0),
                raw.hw_rev,
            );
            {
                let health = raw.upstream_bridge.map_or_else(
                    || PciHealth::new(&raw.pci_dev),
                    |(bus, dev, func)| {
                        PciHealth::new(&raw.pci_dev).with_upstream_bridge(bus, dev, func)
                    },
                );
                let mut ctx = WIFI_INIT_CTX.lock();
                ctx.pci_dev = Some(raw.pci_dev);
                ctx.mmio = raw.mmio;
                ctx.driver_ctx = Some(raw.driver_ctx);
                ctx.health = Some(health);
                ctx.hw_rev = raw.hw_rev;
                // Firmware selection is deferred until the CSR HW_REV has
                // been read after MMIO clock initialization.  7265 and
                // 7265D share PCI IDs, so the PCI revision byte is not enough.
                ctx.fw_candidates = &[];
                ctx.fw_candidate_idx = 0;
            }
            set_init_phase(WifiInitPhase::MmioInit);
            debug::print("iwlwifi", "step: pci_probe_done");
        }
        WifiInitPhase::MmioInit => {
            log::info!("iwlwifi: MMIO reset/clock request phase started");
            debug::print("iwlwifi", "step: mmio_enter");

            // Link training belongs to firmware. Resetting the upstream bridge
            // here can strand the endpoint before the first non-posted read.
            let mmio = WIFI_INIT_CTX.lock().mmio;
            let device_present = {
                let mut ctx = WIFI_INIT_CTX.lock();
                match ctx.health.as_mut() {
                    Some(h) => h.is_device_present(),
                    None => false,
                }
            };
            if !device_present {
                log::warn!("iwlwifi: MMIO phase aborted — PCIe device disappeared before reset");
                debug::print("iwlwifi", "step: ERR device_gone_before_reset");
                set_init_phase(WifiInitPhase::Failed);
                return;
            }
            let bdf_info = {
                let ctx = WIFI_INIT_CTX.lock();
                pci_bdf_from_ctx(&ctx)
            };
            if let Some((pci_bdf, bridge_bdf)) = bdf_info {
                mmio::arm_mmio_watchdog(0, pci_bdf, bridge_bdf);
            }
            debug::print("iwlwifi", "step: mmio_reset");
            IwlWifiDevice::reset_device(mmio);
            debug::print("iwlwifi", "step: mmio_clock_req");
            unsafe {
                core::ptr::write_volatile(
                    mmio.add(CSR_GP_CNTRL as usize),
                    CSR_GP_CNTRL_MAC_ACCESS_REQ | CSR_GP_CNTRL_INIT_DONE,
                );
            }
            mmio::write_barrier();
            mmio::disarm_mmio_watchdog();
            debug::print("iwlwifi", "step: mmio_check_clock");
            let device_present = {
                let mut ctx = WIFI_INIT_CTX.lock();
                match ctx.health.as_mut() {
                    Some(h) => h.is_device_present(),
                    None => false,
                }
            };
            if !device_present {
                log::warn!("iwlwifi: MMIO phase aborted — PCIe device disappeared after reset");
                debug::print("iwlwifi", "step: ERR device_gone_before_clock");
                set_init_phase(WifiInitPhase::Failed);
                return;
            }
            let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
            {
                let mut ctx = WIFI_INIT_CTX.lock();
                ctx.alive_start_tsc = now_tsc;
            }
            debug::print("iwlwifi", "step: mmio_init_done → MmioPollMacClock");
            set_init_phase(WifiInitPhase::MmioPollMacClock);
        }
        WifiInitPhase::MmioPollMacClock => {
            debug::print("iwlwifi", "step: mmio_poll_mac");
            let (mmio, start_tsc) = {
                let ctx = WIFI_INIT_CTX.lock();
                (ctx.mmio, ctx.alive_start_tsc)
            };
            if mmio.is_null() {
                debug::print("iwlwifi", "step: ERR mmio_null");
                set_init_phase(WifiInitPhase::Failed);
                return;
            }
            const TIMEOUT_CYCLES: u64 = 4_000_000_000;
            let bdf_info = {
                let ctx = WIFI_INIT_CTX.lock();
                pci_bdf_from_ctx(&ctx)
            };
            if let Some((pci_bdf, bridge_bdf)) = bdf_info {
                mmio::arm_mmio_watchdog(0, pci_bdf, bridge_bdf);
            }
            let health = { WIFI_INIT_CTX.lock().health };
            let mac_acquired = match unsafe {
                mmio::checked_read_u32(mmio.add(CSR_GP_CNTRL as usize) as usize, health.as_ref())
            } {
                mmio::SafeReadResult::Value(v) if v & CSR_GP_CNTRL_MAC_CLOCK_READY != 0 => true,
                mmio::SafeReadResult::Value(_) => {
                    mmio::disarm_mmio_watchdog();
                    debug::print("iwlwifi", "step: mmio_mac_clock_wait");
                    if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start_tsc)
                        >= TIMEOUT_CYCLES
                    {
                        debug::print("iwlwifi", "step: mmio_force_mac");
                        unsafe {
                            core::ptr::write_volatile(
                                mmio.add(CSR_GP_CNTRL as usize),
                                CSR_GP_CNTRL_MAC_ACCESS_REQ | CSR_GP_CNTRL_INIT_DONE,
                            );
                        }
                        mmio::write_barrier();
                        crate::timing::delay_us(10_000);
                        let recovery_ok = {
                            let mut ctx = WIFI_INIT_CTX.lock();
                            match ctx.health.as_mut() {
                                Some(h) => h.recover().is_ok(),
                                None => false,
                            }
                        };
                        if !recovery_ok {
                            false
                        } else {
                            let health = { WIFI_INIT_CTX.lock().health };
                            if let Some((pci_bdf, bridge_bdf)) = bdf_info {
                                mmio::arm_mmio_watchdog(0, pci_bdf, bridge_bdf);
                            }
                            let clock_ready = match unsafe {
                                mmio::checked_read_u32(
                                    mmio.add(CSR_GP_CNTRL as usize) as usize,
                                    health.as_ref(),
                                )
                            } {
                                mmio::SafeReadResult::Value(v)
                                    if v & CSR_GP_CNTRL_MAC_CLOCK_READY != 0 =>
                                {
                                    true
                                }
                                _ => false,
                            };
                            mmio::disarm_mmio_watchdog();
                            clock_ready
                        }
                    } else {
                        return;
                    }
                }
                mmio::SafeReadResult::MasterAbort | mmio::SafeReadResult::DeviceGone => {
                    mmio::disarm_mmio_watchdog();
                    log::warn!(
                        "iwlwifi: MAC clock probe failed — MMIO read aborted or device disappeared"
                    );
                    debug::print("iwlwifi", "step: ERR mac_abort_or_gone");
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
            };
            if !mac_acquired {
                log::warn!("iwlwifi: MAC clock did not become ready");
                debug::print("iwlwifi", "step: ERR mac_not_ready");
                set_init_phase(WifiInitPhase::Failed);
                return;
            }
            let hw_rev_raw = match unsafe {
                mmio::checked_read_u32(mmio.add(CSR_HW_REV as usize) as usize, health.as_ref())
            } {
                mmio::SafeReadResult::Value(v) => v,
                _ => {
                    mmio::disarm_mmio_watchdog();
                    log::warn!("iwlwifi: CSR HW_REV read failed");
                    debug::print("iwlwifi", "step: ERR hw_rev_read");
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
            };
            let hw_rev = ((hw_rev_raw >> 4) & 0xFFFF) as u16;
            let device_id = WIFI_INIT_CTX
                .lock()
                .pci_dev
                .as_ref()
                .map(|d| d.device_id)
                .unwrap_or(0);
            let candidates = select_firmware_list(device_id, hw_rev);
            log::info!(
                "iwlwifi: detected CSR HW_REV type={:#06x}, firmware candidates={}",
                hw_rev & CSR_HW_REV_TYPE_MASK,
                candidates.len()
            );
            if candidates.is_empty() {
                mmio::disarm_mmio_watchdog();
                log::error!("iwlwifi: no firmware image matches the detected device/revision");
                debug::print("iwlwifi", "step: no_fw");
                set_init_phase(WifiInitPhase::Failed);
                return;
            }
            // The clock/revision probe is complete. Do not leave the
            // recovery watchdog armed while read_mac performs its own
            // protected MMIO sequence; that sequence has a separate arm /
            // disarm pair below.
            mmio::disarm_mmio_watchdog();
            debug::print("iwlwifi", "step: mmio_read_mac");
            let mac = {
                if let Some((pci_bdf, bridge_bdf)) = bdf_info {
                    mmio::arm_mmio_watchdog(0, pci_bdf, bridge_bdf);
                }
                let health = { WIFI_INIT_CTX.lock().health };
                let mac = IwlWifiDevice::read_mac(mmio, health.as_ref());
                mmio::disarm_mmio_watchdog();
                mac
            };
            debug::print("iwlwifi", "step: mmio_mask_ints");
            unsafe {
                core::ptr::write_volatile(mmio.add(CSR_INT_MASK as usize), CSR_INI_SET_MASK);
            }
            {
                let mut ctx = WIFI_INIT_CTX.lock();
                ctx.mac = Some(mac);
                ctx.hw_rev = hw_rev;
                ctx.fw_candidates = candidates;
            }
            debug::print("iwlwifi", "step: mmio_poll_mac_done");
            set_init_phase(WifiInitPhase::DmaAlloc);
        }
        WifiInitPhase::DmaAlloc => {
            log::info!("iwlwifi: allocating DMA rings and RX/TX buffers");
            let (pci_dev, mmio, driver_ctx, health, mac, hw_rev, tx_dma, rx_dma, tx_bufs, rx_bufs) = {
                let mut ctx = WIFI_INIT_CTX.lock();
                let pci_dev = match ctx.pci_dev.take() {
                    Some(d) => d,
                    None => {
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                let mmio = ctx.mmio;
                let driver_ctx = match ctx.driver_ctx {
                    Some(c) => c,
                    None => {
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                let health = match ctx.health.take() {
                    Some(h) => h,
                    None => {
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                let mac = match ctx.mac {
                    Some(m) => m,
                    None => {
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                let hw_rev = ctx.hw_rev;
                let mut tx_dma_ring = match DmaRegion::alloc(driver_ctx, TX_DMA_ALLOCATION_BYTES) {
                    Some(r) => r,
                    None => {
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                if tx_dma_ring
                    .dma_map(
                        driver_ctx,
                        pci_dma_device_id(pci_dev.bus, pci_dev.device, pci_dev.function),
                    )
                    .is_err()
                {
                    tx_dma_ring.free(driver_ctx);
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
                let mut rx_dma_ring = match DmaRegion::alloc(
                    driver_ctx,
                    core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE
                        + core::mem::size_of::<RxDmaStatus>(),
                ) {
                    Some(r) => r,
                    None => {
                        tx_dma_ring.free(driver_ctx);
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                if rx_dma_ring
                    .dma_map(
                        driver_ctx,
                        pci_dma_device_id(pci_dev.bus, pci_dev.device, pci_dev.function),
                    )
                    .is_err()
                {
                    rx_dma_ring.free(driver_ctx);
                    tx_dma_ring.free(driver_ctx);
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
                let mut tx_bufs: Vec<DmaRegion> = Vec::new();
                for _ in 0..TX_QUEUE_SIZE {
                    let mut buf = match DmaRegion::alloc(driver_ctx, MAX_FRAME_SIZE) {
                        Some(b) => b,
                        None => {
                            break;
                        }
                    };
                    if buf
                        .dma_map(
                            driver_ctx,
                            pci_dma_device_id(pci_dev.bus, pci_dev.device, pci_dev.function),
                        )
                        .is_err()
                    {
                        buf.free(driver_ctx);
                        break;
                    }
                    tx_bufs.push(buf);
                }
                if tx_bufs.len() < TX_QUEUE_SIZE {
                    for mut b in tx_bufs {
                        b.free(driver_ctx);
                    }
                    tx_dma_ring.free(driver_ctx);
                    rx_dma_ring.free(driver_ctx);
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
                let mut rx_bufs: Vec<DmaRegion> = Vec::new();
                let rx_virt = rx_dma_ring.virt() as *mut RxDmaDesc;
                for i in 0..RX_QUEUE_SIZE {
                    let mut buf = match DmaRegion::alloc(driver_ctx, RX_BUFFER_SIZE) {
                        Some(b) => b,
                        None => {
                            break;
                        }
                    };
                    let dma = match buf.dma_map(
                        driver_ctx,
                        pci_dma_device_id(pci_dev.bus, pci_dev.device, pci_dev.function),
                    ) {
                        Ok(d) => d,
                        Err(_) => {
                            buf.free(driver_ctx);
                            break;
                        }
                    };
                    unsafe {
                        (*rx_virt.add(i)).addr = (dma >> 8) as u32;
                        mmio::cache_flush(rx_virt.add(i) as usize);
                    }
                    rx_bufs.push(buf);
                }
                if rx_bufs.len() < RX_QUEUE_SIZE {
                    for mut b in tx_bufs {
                        b.free(driver_ctx);
                    }
                    for mut b in rx_bufs {
                        b.free(driver_ctx);
                    }
                    tx_dma_ring.free(driver_ctx);
                    rx_dma_ring.free(driver_ctx);
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
                (
                    pci_dev,
                    mmio,
                    driver_ctx,
                    health,
                    mac,
                    hw_rev,
                    tx_dma_ring,
                    rx_dma_ring,
                    tx_bufs,
                    rx_bufs,
                )
            };
            // FH RX setup is deferred until firmware reports alive; the
            // firmware reset sequence can overwrite the FH registers.
            debug::print("iwlwifi", "rx_dma_deferred_until_alive");
            let device = IwlWifiDevice {
                mac,
                _pci_dev: pci_dev,
                mmio,
                hw_rev,
                ctx: driver_ctx,
                health,
                fw_state: FwState::NotLoaded,
                fw_build: 0,
                fw_api_ver: IWL_FW_API_VER,
                iwl_state: IwlState::Init,
                wifi_conn: bonder::wifi::WifiConnection::new(),
                wpa: bonder::wpa::WpaSupplicant::new(),
                wpa_required: false,
                wpa_keys_installed: false,
                wpa_key_command_end: None,
                pending_wpa_message4: None,
                dhcp: None,
                scan_results: Vec::new(),
                scan_channel: 1,
                scan_pending: false,
                scan_result_grace_ticks: 0,
                tx_queue: alloc::collections::VecDeque::new(),
                rx_queue: alloc::collections::VecDeque::new(),
                tx_dma_ring: tx_dma,
                rx_dma_ring: rx_dma,
                tx_head: 0,
                tx_tail: 0,
                rx_head: 0,
                rx_tail: 0,
                tx_bufs,
                rx_bufs,
                ip_address: [0u8; 4],
                subnet_mask: [0u8; 4],
                gateway: [0u8; 4],
                dns_server: [0u8; 4],
            };
            {
                let mut ctx = WIFI_INIT_CTX.lock();
                ctx.mmio_device = Some(Box::new(device));
            }
            log::info!("iwlwifi: DMA rings and buffers allocated");
            debug::print("iwlwifi", "step: dma_alloc_done");
            set_init_phase(WifiInitPhase::FwUpload);
        }
        WifiInitPhase::FwUpload => {
            let (fw_data, fw_name, bdf_info) = {
                let mut ctx = WIFI_INIT_CTX.lock();
                let _dev = match ctx.mmio_device.as_mut() {
                    Some(d) => d,
                    None => {
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                if ctx.fw_candidate_idx >= ctx.fw_candidates.len() {
                    debug::print("iwlwifi", "step: all_fw_failed");
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
                let fw = &ctx.fw_candidates[ctx.fw_candidate_idx];
                let bdf = pci_bdf_from_ctx(&ctx);
                (fw.data, fw.name, bdf)
            };
            log::info!(
                "iwlwifi: step: trying firmware {} ({} bytes)",
                fw_name,
                fw_data.len()
            );
            if let Some((pci_bdf, bridge_bdf)) = bdf_info {
                mmio::arm_mmio_watchdog(0, pci_bdf, bridge_bdf);
            }
            let start_result = {
                let mut ctx = WIFI_INIT_CTX.lock();
                let dev = match ctx.mmio_device.as_mut() {
                    Some(d) => d,
                    None => {
                        mmio::disarm_mmio_watchdog();
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                dev.start_firmware(fw_data)
            };
            mmio::disarm_mmio_watchdog();
            match start_result {
                Ok(()) => {
                    log::info!(
                        "iwlwifi: step: firmware {} upload complete, waiting for alive",
                        fw_name
                    );
                    debug::print("iwlwifi", "step: fw_uploaded");
                    let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
                    WIFI_INIT_CTX.lock().alive_start_tsc = now_tsc;
                    set_init_phase(WifiInitPhase::FwWaitAlive);
                }
                Err(e) => {
                    log::warn!("iwlwifi: step: firmware {} upload failed: {}", fw_name, e);
                    let mut ctx = WIFI_INIT_CTX.lock();
                    ctx.fw_candidate_idx += 1;
                }
            }
        }
        WifiInitPhase::FwWaitAlive => {
            let (start_tsc, bdf_info) = {
                let ctx = WIFI_INIT_CTX.lock();
                (ctx.alive_start_tsc, pci_bdf_from_ctx(&ctx))
            };

            // Check PCI config space before touching the endpoint MMIO. This
            // is safe even when the firmware has left the link down and avoids
            // entering a non-posted read that cannot complete.
            let pci_health = {
                let mut ctx = WIFI_INIT_CTX.lock();
                ctx.health.as_mut().map(PciHealth::check)
            };
            if let Some(Err(error)) = pci_health {
                log::warn!("iwlwifi: alive wait PCI health check failed: {}", error);
                let mut ctx = WIFI_INIT_CTX.lock();
                ctx.fw_candidate_idx += 1;
                set_init_phase(WifiInitPhase::FwUpload);
                return;
            }

            if let Some((pci_bdf, bridge_bdf)) = bdf_info {
                mmio::arm_mmio_watchdog(0, pci_bdf, bridge_bdf);
            }
            // Do not hold WIFI_INIT_CTX while the MMIO read is in progress.
            // The NMI watchdog may abandon a stalled read and resume the
            // scheduler; retaining this Mutex across that boundary would make
            // force_init_failed() and the next state transition deadlock.
            let mut dev = {
                let mut ctx = WIFI_INIT_CTX.lock();
                match ctx.mmio_device.take() {
                    Some(d) => d,
                    None => {
                        mmio::disarm_mmio_watchdog();
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                }
            };
            debug::print("iwlwifi", "step: fw_alive_mmio_read");
            let alive_result = dev.check_alive_nonblocking(start_tsc);
            mmio::disarm_mmio_watchdog();
            debug::print("iwlwifi", "step: fw_alive_mmio_done");
            WIFI_INIT_CTX.lock().mmio_device = Some(dev);
            match alive_result {
                Ok(true) => {
                    log::info!("iwlwifi: firmware alive notification received");
                    debug::print("iwlwifi", "step: fw_alive");
                    set_init_phase(WifiInitPhase::FwInitCmds);
                }
                Ok(false) => {
                    debug::print("iwlwifi", "step: fw_wait_alive_poll");
                }
                Err(e) => {
                    log::warn!("iwlwifi: step: firmware alive failed: {}", e);
                    let mut ctx = WIFI_INIT_CTX.lock();
                    ctx.fw_candidate_idx += 1;
                    set_init_phase(WifiInitPhase::FwUpload);
                }
            }
        }
        WifiInitPhase::FwInitCmds => {
            let bdf_info = {
                let ctx = WIFI_INIT_CTX.lock();
                pci_bdf_from_ctx(&ctx)
            };
            if let Some((pci_bdf, bridge_bdf)) = bdf_info {
                mmio::arm_mmio_watchdog(0, pci_bdf, bridge_bdf);
            }
            let result = {
                let mut ctx = WIFI_INIT_CTX.lock();
                let dev = match ctx.mmio_device.as_mut() {
                    Some(d) => d,
                    None => {
                        mmio::disarm_mmio_watchdog();
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                dev.send_init_commands()
            };
            mmio::disarm_mmio_watchdog();
            match result {
                Ok(()) => {
                    log::info!("iwlwifi: firmware initialization commands accepted");
                    debug::print("iwlwifi", "step: fw_init_cmds_ok");
                    set_init_phase(WifiInitPhase::Done);
                }
                Err(e) => {
                    log::warn!("iwlwifi: step: init commands failed: {}", e);
                    set_init_phase(WifiInitPhase::Failed);
                }
            }
        }
        WifiInitPhase::Done => {
            let dev_opt = WIFI_INIT_CTX.lock().mmio_device.take();
            if let Some(dev) = dev_opt {
                let mut dev_guard = WIFI_DEVICE.lock();
                if dev_guard.is_none() {
                    *dev_guard = Some(dev);
                }
            }
            WIFI_INIT_COMPLETED.store(true, core::sync::atomic::Ordering::Release);
            log::info!("iwlwifi: initialization complete; device is ready for scanning");
            debug::print("iwlwifi", "step: init_done");
        }
        WifiInitPhase::Failed => {
            let mut ctx = WIFI_INIT_CTX.lock();
            let _ = ctx.mmio_device.take();
            let drv = ctx.driver_ctx;
            for mut buf in ctx.tx_bufs.drain(..) {
                if let Some(c) = drv {
                    buf.free(c);
                }
            }
            for mut buf in ctx.rx_bufs.drain(..) {
                if let Some(c) = drv {
                    buf.free(c);
                }
            }
            if let Some(mut ring) = ctx.tx_dma_ring.take() {
                if let Some(c) = drv {
                    ring.free(c);
                }
            }
            if let Some(mut ring) = ctx.rx_dma_ring.take() {
                if let Some(c) = drv {
                    ring.free(c);
                }
            }
            drop(ctx);
            WIFI_INIT_COMPLETED.store(true, core::sync::atomic::Ordering::Release);
            log::error!("iwlwifi: initialization failed; device disabled");
            debug::print("iwlwifi", "step: init_failed");
        }
    }
}

// ── High-level API ─────────────────

fn pci_bdf_from_ctx(ctx: &WifiInitContext) -> Option<((u8, u8, u8), Option<(u8, u8, u8)>)> {
    let pci = ctx.pci_dev.as_ref()?;
    let bdf = (pci.bus, pci.device, pci.function);
    let bridge = ctx.health.as_ref().and_then(|h| h.upstream_bridge());
    Some((bdf, bridge))
}

pub fn try_init_wifi_device() {
    debug::print("iwlwifi", "try_init_wifi_device: start");
    let ctx_opt = WIFI_DRIVER_CTX.lock();
    let ctx = match *ctx_opt {
        Some(c) => c,
        None => {
            log::warn!("iwlwifi: driver context not set, cannot init");
            debug::print("iwlwifi", "ERR no_driver_ctx");
            return;
        }
    };
    drop(ctx_opt);

    let mut dev_guard = WIFI_DEVICE.lock();
    if dev_guard.is_some() {
        debug::print("iwlwifi", "already_inited");
        return;
    }

    debug::print("iwlwifi", "init_wifi_from_pci");
    let mut probe = match crate::wifi::init_wifi_from_pci(ctx) {
        Some(p) => p,
        None => {
            debug::print("iwlwifi", "ERR no_pci_device");
            return;
        }
    };

    let candidates = select_firmware_list(probe.device_id, probe.driver.hardware_revision());
    if candidates.is_empty() {
        log::warn!(
            "iwlwifi: no firmware available for device {:#06x}",
            probe.device_id
        );
        debug::print("iwlwifi", "ERR no_firmware");
        return;
    }

    let mut fw_loaded = false;
    for fw in candidates {
        log::info!(
            "iwlwifi: trying firmware {} ({} bytes)",
            fw.name,
            fw.data.len()
        );
        debug::print("iwlwifi", "load_firmware_start");

        match probe.driver.load_firmware(fw.data) {
            Ok(()) => {
                log::info!("iwlwifi: firmware {} loaded successfully", fw.name);
                debug::print("iwlwifi", "load_firmware_ok");
                fw_loaded = true;
                break;
            }
            Err(e) => {
                log::warn!("iwlwifi: firmware {} failed: {}", fw.name, e);
                debug::print("iwlwifi", "load_firmware_fail");
            }
        }
    }

    if fw_loaded {
        *dev_guard = Some(probe.driver);
        debug::print("iwlwifi", "init_done");
    } else {
        log::error!("iwlwifi: all firmware variants failed to load");
        debug::print("iwlwifi", "ERR all_fw_failed");
    }
    WIFI_INIT_COMPLETED.store(true, core::sync::atomic::Ordering::Release);
}

pub fn tick_wifi_device() {
    if !WIFI_INIT_COMPLETED.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let mut dev_guard = WIFI_DEVICE.lock();
    if let Some(ref mut dev) = *dev_guard {
        let dev_ref: &mut dyn crate::wifi::WifiDriver = &mut **dev;
        dev_ref.tick();
        update_wifi_manager(dev_ref);
    }
}

fn update_wifi_manager(dev: &dyn crate::wifi::WifiDriver) {
    let mut mgr = WIFI_MANAGER.lock();
    if let Some(ref mut m) = *mgr {
        m.device_available = dev.device_available();
        m.scan_results = dev.get_scan_results();
        m.status = dev.get_status();
        m.connected_ssid = dev.connected_ssid().map(|s| s.to_string());
        let ip = dev.ip_address();
        if ip != [0u8; 4] {
            m.ip_address = Some(alloc::format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]));
        } else {
            m.ip_address = None;
        }
    }
}

pub fn wifi_state_snapshot() -> Option<WifiManager> {
    WIFI_MANAGER.lock().clone()
}

pub fn init_wifi_manager() {
    *WIFI_MANAGER.lock() = Some(WifiManager::new());
}

pub fn connect_to_ap(ssid: &Ssid, password: Option<&str>) {
    let mut dev_guard = WIFI_DEVICE.lock();
    if let Some(ref mut dev) = *dev_guard {
        let dev_ref: &mut dyn crate::wifi::WifiDriver = &mut **dev;
        let _ = dev_ref.connect(ssid, password);
    }
}

pub fn start_scan_if_idle() {
    let mut dev_guard = WIFI_DEVICE.lock();
    let Some(ref mut dev) = *dev_guard else {
        log::warn!("iwlwifi: scan request ignored — no operational device");
        return;
    };
    let dev_ref: &mut dyn crate::wifi::WifiDriver = &mut **dev;
    if !dev_ref.device_available() {
        log::warn!("iwlwifi: scan request ignored — firmware is not ready");
        return;
    }
    // Only start a scan if the device is ready and not already busy.
    if dev_ref.get_status() != bonder::wifi::WifiStatus::Disconnected {
        log::info!(
            "iwlwifi: scan request deferred — current status={:?}",
            dev_ref.get_status()
        );
        return;
    }
    if !dev_ref.start_scan() {
        log::warn!("iwlwifi: scan request failed before firmware submission");
    }
}

impl IwlWifiDevice {
    /// Start an active scan on the supported channel set.
    pub fn start_scan(&mut self) -> Result<(), crate::DriverError> {
        if self.fw_state != FwState::Ready {
            return Err(crate::DriverError::NotReady);
        }

        self.wifi_conn.start_scan();
        self.scan_results.clear();
        // Reset the tick-count watchdog. The LMAC request covers the 2.4/5 GHz
        // channel set and uses passive dwell times, so the host must leave
        // several seconds for firmware completion and RX delivery.
        self.scan_channel = 0;
        self.scan_pending = true;
        self.scan_result_grace_ticks = 0;
        self.iwl_state = IwlState::Scanning;

        let scan_cmd = ScanRequestCmd::new(self.mac);
        let cmd_data = unsafe {
            core::slice::from_raw_parts(
                &scan_cmd as *const ScanRequestCmd as *const u8,
                core::mem::size_of::<ScanRequestCmd>(),
            )
        };
        // SCAN_OFFLOAD_REQUEST_CMD (0x51) is a legacy-group command and uses
        // the four-byte HCMD header. SCAN_CFG_CMD above is the exception: it
        // is sent with the always-long header because its channel database is
        // a long-group command.
        if let Err(error) = self.send_hcmd(
            LegacyCmd::ScanRequest as u8,
            GroupId::Legacy as u8,
            cmd_data,
        ) {
            self.scan_pending = false;
            self.iwl_state = IwlState::Disconnected;
            self.wifi_conn.finish_scan();
            return Err(error);
        }

        log::info!(
            "iwlwifi: LMAC scan request queued: opcode=0x51 channels={} bytes={} (Klog: waiting for APs)",
            23,
            core::mem::size_of::<ScanRequestCmd>()
        );
        Ok(())
    }

    pub(super) fn process_scan_result(&mut self, frame: &[u8]) {
        if let Some(beacon) = wifi::parse_beacon(frame) {
            let ssid = beacon.ssid.clone().unwrap_or(Ssid::new(b""));
            if ssid.is_empty() {
                return;
            }

            let security = wifi::security_from_beacon(beacon.capability, beacon.rsn.as_ref());
            let ap = AccessPoint {
                ssid,
                bssid: beacon.header.addr2,
                channel: beacon.ds_channel.unwrap_or(0),
                rssi: -50,
                security,
                beacon_interval: beacon.beacon_interval,
            };
            if self
                .scan_results
                .iter()
                .any(|existing| existing.bssid == ap.bssid)
            {
                return;
            }
            log::info!(
                "iwlwifi: AP FOUND ssid=\"{}\" bssid={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} channel={} security={} rssi={}dBm",
                ap.ssid,
                ap.bssid[0],
                ap.bssid[1],
                ap.bssid[2],
                ap.bssid[3],
                ap.bssid[4],
                ap.bssid[5],
                ap.channel,
                ap.security.name(),
                ap.rssi
            );
            self.wifi_conn.add_scan_result(ap.clone());
            self.scan_results.push(ap);
        }
    }

    pub fn connect(
        &mut self,
        ssid: &Ssid,
        password: Option<&str>,
    ) -> Result<(), crate::DriverError> {
        if self.fw_state != FwState::Ready {
            return Err(crate::DriverError::NotReady);
        }

        let ap = self
            .scan_results
            .iter()
            .find(|ap| ap.ssid == *ssid)
            .cloned()
            .ok_or(crate::DriverError::DeviceNotFound)?;
        if ap.security != bonder::wifi::Security::Open && password.is_none() {
            // Never silently downgrade a protected AP to an open association.
            return Err(crate::DriverError::InvalidArgument);
        }
        if password.is_some() && ap.security != bonder::wifi::Security::Wpa2Psk {
            return Err(crate::DriverError::NotSupported);
        }
        self.wifi_conn.connect(ssid, password);
        self.wpa_required = password.is_some();
        self.wpa_keys_installed = false;
        self.wpa_key_command_end = None;
        self.pending_wpa_message4 = None;
        self.ip_address = [0; 4];
        self.subnet_mask = [0; 4];
        self.gateway = [0; 4];
        self.dns_server = [0; 4];

        if let Some(password) = password {
            self.wpa.init(password, ssid.as_str(), ap.bssid, self.mac);
        }

        self.iwl_state = IwlState::AuthSent;
        let auth_frame = wifi::build_auth_frame(ap.bssid, self.mac, 1);
        let _ = self.send_raw_80211_frame(&auth_frame);
        log::info!(
            "iwlwifi: authenticating with {} ({:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x})",
            ssid,
            ap.bssid[0],
            ap.bssid[1],
            ap.bssid[2],
            ap.bssid[3],
            ap.bssid[4],
            ap.bssid[5],
        );
        Ok(())
    }

    pub fn disconnect(&mut self) {
        if let Some(bssid) = self.wifi_conn.current_bssid {
            let deauth = wifi::build_deauth(bssid, self.mac, 3);
            let _ = self.send_raw_80211_frame(&deauth);
        }
        if let Some(ref mut dhcp) = self.dhcp {
            let release = dhcp.build_release();
            let _ = self.send_raw_80211_frame(&release);
        }
        self.dhcp = None;
        self.wifi_conn.disconnect();
        self.wpa = bonder::wpa::WpaSupplicant::new();
        self.wpa_required = false;
        self.wpa_keys_installed = false;
        self.wpa_key_command_end = None;
        self.pending_wpa_message4 = None;
        self.iwl_state = IwlState::Disconnected;
        log::info!("iwlwifi: disconnected");
    }

    pub fn access_points(&self) -> &[AccessPoint] {
        &self.scan_results
    }

    pub fn wifi_status(&self) -> &bonder::wifi::WifiConnection {
        &self.wifi_conn
    }

    pub fn is_network_ready(&self) -> bool {
        self.wifi_conn.is_connected()
            && (!self.wpa_required
                || (self.wpa.state == bonder::wpa::WpaState::Done && self.wpa_keys_installed))
            && self.ip_address != [0u8; 4]
    }
}
