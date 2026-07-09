//! Global state, incremental init state machine, firmware registry,
//! and high-level public API.

use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::sync::atomic::AtomicBool;
use spin::Mutex;

use bonder::wifi::Ssid;
use crate::debug;
use crate::mmio::{self, DmaRegion};
use crate::pci_health::PciHealth;
use crate::timing;
use crate::DriverContext;

use super::regs::*;
use super::types::*;
use super::device::IwlWifiDevice;

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

fn set_init_phase(phase: WifiInitPhase) {
    WIFI_INIT_PHASE.store(phase as u8, core::sync::atomic::Ordering::Release);
}

fn get_init_phase() -> WifiInitPhase {
    let raw = WIFI_INIT_PHASE.load(core::sync::atomic::Ordering::Acquire);
    WifiInitPhase::from(raw)
}

// ── Firmware registry ─────────────

// 7260 series (PCI 0x08B1, 0x08B2)
const FW_7260_17: &[u8] = include_bytes!("../../../bonder/iwlwifi/iwlwifi-7260-17.ucode");
const FW_7260_16: &[u8] = include_bytes!("../../../bonder/iwlwifi/iwlwifi-7260-16.ucode");

// 7265 series, non-D stepping (PCI 0x095A, 0x095B)
const FW_7265_17: &[u8] = include_bytes!("../../../bonder/iwlwifi/iwlwifi-7265-17.ucode");
const FW_7265_16: &[u8] = include_bytes!("../../../bonder/iwlwifi/iwlwifi-7265-16.ucode");

// 7265D series, D stepping (PCI 0x095A, 0x095B)
const FW_7265D_29: &[u8] = include_bytes!("../../../bonder/iwlwifi/iwlwifi-7265D-29.ucode");
const FW_7265D_27: &[u8] = include_bytes!("../../../bonder/iwlwifi/iwlwifi-7265D-27.ucode");

fn select_firmware_list(device_id: u16) -> &'static [FirmwareBlob] {
    match device_id {
        0x08B1 | 0x08B2 => &[
            FirmwareBlob { data: FW_7260_17, name: "iwlwifi-7260-17" },
            FirmwareBlob { data: FW_7260_16, name: "iwlwifi-7260-16" },
        ],
        0x095A | 0x095B => &[
            FirmwareBlob { data: FW_7265D_29, name: "iwlwifi-7265D-29" },
            FirmwareBlob { data: FW_7265D_27, name: "iwlwifi-7265D-27" },
            FirmwareBlob { data: FW_7265_17, name: "iwlwifi-7265-17" },
            FirmwareBlob { data: FW_7265_16, name: "iwlwifi-7265-16" },
        ],
        _ => &[],
    }
}

// ── Incremental init state machine ─

pub fn try_init_wifi_device_step() {
    let phase = get_init_phase();

    match phase {
        WifiInitPhase::Idle => {
            let driver_ctx_opt = WIFI_DRIVER_CTX.lock();
            let _driver_ctx = match *driver_ctx_opt {
                Some(c) => c,
                None => {
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
            debug::print("iwlwifi", "step: pci_probe_enter");
            let driver_ctx = match *WIFI_DRIVER_CTX.lock() {
                Some(c) => c,
                None => {
                    debug::print("iwlwifi", "step: ERR no_driver_ctx");
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
            };
            debug::print("iwlwifi", "step: call probe_pci_only");
            let raw = match crate::wifi::probe_pci_only(driver_ctx) {
                Some(r) => r,
                None => {
                    debug::print("iwlwifi", "step: no_pci_device");
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
            };
            let candidates = select_firmware_list(raw.device_id);
            if candidates.is_empty() {
                debug::print("iwlwifi", "step: no_fw");
                set_init_phase(WifiInitPhase::Failed);
                return;
            }
            {
                let mut health = PciHealth::new(&raw.pci_dev);
                if let Some((bus, dev, func)) = raw.upstream_bridge {
                    health = health.with_upstream_bridge(bus, dev, func);
                    if let Some(_bridge) = crate::pci::PciDevice::new(bus, dev, func) {
                        let lnk_ctl_offset = crate::pci_error::find_pcie_cap(bus, dev, func)
                            .and_then(|off| off.checked_add(0x10));
                        if let Some(lnk_off) = lnk_ctl_offset {
                            let ctl = crate::pci::PciConfigSpace::read_config_word(
                                bus, dev, func, lnk_off,
                            );
                            crate::pci::PciConfigSpace::write_config_word_raw(
                                bus, dev, func, lnk_off, ctl | (1 << 5),
                            );
                            log::info!(
                                "WiFi: link retrain triggered on bridge {:02x}:{:02x}.{}",
                                bus, dev, func,
                            );
                            timing::delay_us(10_000);
                        }
                    }
                }
                let mut ctx = WIFI_INIT_CTX.lock();
                ctx.pci_dev = Some(raw.pci_dev);
                ctx.mmio = raw.mmio;
                ctx.driver_ctx = Some(raw.driver_ctx);
                ctx.health = Some(health);
                ctx.hw_rev = raw.hw_rev;
                ctx.fw_candidates = candidates;
                ctx.fw_candidate_idx = 0;
            }
            set_init_phase(WifiInitPhase::MmioInit);
            debug::print("iwlwifi", "step: pci_probe_done");
        }
        WifiInitPhase::MmioInit => {
            let (mmio, health_ok) = {
                let mut ctx = WIFI_INIT_CTX.lock();
                let mmio = ctx.mmio;
                let ok = match ctx.health.as_mut() {
                    Some(h) => h.recover().is_ok(),
                    None => false,
                };
                (mmio, ok)
            };
            if !health_ok {
                debug::print("iwlwifi", "step: ERR health_recover");
                set_init_phase(WifiInitPhase::Failed);
                return;
            }
            IwlWifiDevice::reset_device(mmio);
            unsafe {
                core::ptr::write_volatile(mmio.add(CSR_GP_CNTRL as usize), CSR_GP_CNTRL_MAC_ACCESS_REQ);
            }
            mmio::write_barrier();
            let device_present = {
                let mut ctx = WIFI_INIT_CTX.lock();
                match ctx.health.as_mut() {
                    Some(h) => h.is_device_present(),
                    None => false,
                }
            };
            if !device_present {
                debug::print("iwlwifi", "step: ERR device_gone_before_clock");
                set_init_phase(WifiInitPhase::Failed);
                return;
            }
            {
                let start = unsafe { core::arch::x86_64::_rdtsc() };
                loop {
                    if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 10_000_000 {
                        break;
                    }
                    core::hint::spin_loop();
                }
            }
            {
                let mut ctx = WIFI_INIT_CTX.lock();
                let ok = match ctx.health.as_mut() {
                    Some(h) => h.recover().is_ok(),
                    None => false,
                };
                if !ok {
                    debug::print("iwlwifi", "step: ERR recover_before_read_mac");
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
            }
            let mac = {
                let ctx = WIFI_INIT_CTX.lock();
                let health = ctx.health.as_ref();
                IwlWifiDevice::read_mac(mmio, health)
            };
            unsafe {
                core::ptr::write_volatile(mmio.add(CSR_INT_MASK as usize), 0xFFFFFFFFu32);
            }
            {
                let mut ctx = WIFI_INIT_CTX.lock();
                ctx.mac = Some(mac);
            }
            debug::print("iwlwifi", "step: mmio_init_done");
            set_init_phase(WifiInitPhase::DmaAlloc);
        }
        WifiInitPhase::DmaAlloc => {
            let (pci_dev, mmio, driver_ctx, health, mac, hw_rev, tx_dma, rx_dma, tx_bufs, rx_bufs) = {
                let mut ctx = WIFI_INIT_CTX.lock();
                let pci_dev = match ctx.pci_dev.take() {
                    Some(d) => d,
                    None => { set_init_phase(WifiInitPhase::Failed); return; }
                };
                let mmio = ctx.mmio;
                let driver_ctx = match ctx.driver_ctx {
                    Some(c) => c,
                    None => { set_init_phase(WifiInitPhase::Failed); return; }
                };
                let health = match ctx.health.take() {
                    Some(h) => h,
                    None => { set_init_phase(WifiInitPhase::Failed); return; }
                };
                let mac = match ctx.mac {
                    Some(m) => m,
                    None => { set_init_phase(WifiInitPhase::Failed); return; }
                };
                let hw_rev = ctx.hw_rev;
                let mut tx_dma_ring = match DmaRegion::alloc(driver_ctx, core::mem::size_of::<TxDmaDesc>() * TX_QUEUE_SIZE) {
                    Some(r) => r,
                    None => { set_init_phase(WifiInitPhase::Failed); return; }
                };
                if tx_dma_ring.dma_map(driver_ctx, pci_dev.device_id).is_err() {
                    tx_dma_ring.free(driver_ctx);
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
                let mut rx_dma_ring = match DmaRegion::alloc(driver_ctx, core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE) {
                    Some(r) => r,
                    None => { tx_dma_ring.free(driver_ctx); set_init_phase(WifiInitPhase::Failed); return; }
                };
                if rx_dma_ring.dma_map(driver_ctx, pci_dev.device_id).is_err() {
                    rx_dma_ring.free(driver_ctx);
                    tx_dma_ring.free(driver_ctx);
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
                let mut tx_bufs: Vec<DmaRegion> = Vec::new();
                for _ in 0..TX_QUEUE_SIZE {
                    let mut buf = match DmaRegion::alloc(driver_ctx, MAX_FRAME_SIZE) {
                        Some(b) => b,
                        None => { break; }
                    };
                    if buf.dma_map(driver_ctx, pci_dev.device_id).is_err() {
                        buf.free(driver_ctx);
                        break;
                    }
                    tx_bufs.push(buf);
                }
                if tx_bufs.len() < TX_QUEUE_SIZE {
                    for mut b in tx_bufs { b.free(driver_ctx); }
                    tx_dma_ring.free(driver_ctx);
                    rx_dma_ring.free(driver_ctx);
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
                let mut rx_bufs: Vec<DmaRegion> = Vec::new();
                let rx_virt = rx_dma_ring.virt() as *mut RxDmaDesc;
                for i in 0..RX_QUEUE_SIZE {
                    let mut buf = match DmaRegion::alloc(driver_ctx, MAX_FRAME_SIZE) {
                        Some(b) => b,
                        None => { break; }
                    };
                    let dma = match buf.dma_map(driver_ctx, pci_dev.device_id) {
                        Ok(d) => d,
                        Err(_) => { buf.free(driver_ctx); break; }
                    };
                    unsafe {
                        (*rx_virt.add(i)).addr_lo = dma as u32;
                        (*rx_virt.add(i)).addr_hi = (dma >> 32) as u32;
                        (*rx_virt.add(i)).len = MAX_FRAME_SIZE as u16;
                        mmio::cache_flush(rx_virt.add(i) as *const u8);
                    }
                    rx_bufs.push(buf);
                }
                if rx_bufs.len() < RX_QUEUE_SIZE {
                    for mut b in tx_bufs { b.free(driver_ctx); }
                    for mut b in rx_bufs { b.free(driver_ctx); }
                    tx_dma_ring.free(driver_ctx);
                    rx_dma_ring.free(driver_ctx);
                    set_init_phase(WifiInitPhase::Failed);
                    return;
                }
                (pci_dev, mmio, driver_ctx, health, mac, hw_rev,
                 tx_dma_ring, rx_dma_ring, tx_bufs, rx_bufs)
            };
            let rx_phys = rx_dma.dma_iova();
            unsafe {
                core::ptr::write_volatile(mmio.add(FH_TX_CHNL0_WPTR as usize), 0);
                core::ptr::write_volatile(mmio.add(FH_RSCSR_CHNL0_RBDCB_BASE as usize), rx_phys as u32);
                core::ptr::write_volatile(mmio.add(FH_RSCSR_CHNL0_RBDCB_RPTR_REG as usize), 0);
            }
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
                dhcp: None,
                scan_results: Vec::new(),
                scan_channel: 1,
                scan_pending: false,
                tx_queue: alloc::collections::VecDeque::new(),
                rx_queue: alloc::collections::VecDeque::new(),
                tx_dma_ring: tx_dma,
                rx_dma_ring: rx_dma,
                tx_head: 0, tx_tail: 0, rx_head: 0, rx_tail: 0,
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
            debug::print("iwlwifi", "step: dma_alloc_done");
            set_init_phase(WifiInitPhase::FwUpload);
        }
        WifiInitPhase::FwUpload => {
            let (fw_data, fw_name) = {
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
                (fw.data, fw.name)
            };
            log::info!(
                "iwlwifi: step: trying firmware {} ({} bytes)",
                fw_name, fw_data.len()
            );
            let start_result = {
                let mut ctx = WIFI_INIT_CTX.lock();
                let dev = match ctx.mmio_device.as_mut() {
                    Some(d) => d,
                    None => {
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                dev.start_firmware(fw_data)
            };
            match start_result {
                Ok(()) => {
                    log::info!("iwlwifi: step: firmware {} upload complete, waiting for alive", fw_name);
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
            let start_tsc = WIFI_INIT_CTX.lock().alive_start_tsc;
            let alive_result = {
                let mut ctx = WIFI_INIT_CTX.lock();
                let dev = match ctx.mmio_device.as_mut() {
                    Some(d) => d,
                    None => {
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                dev.check_alive_nonblocking(start_tsc)
            };
            match alive_result {
                Ok(true) => {
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
            let result = {
                let mut ctx = WIFI_INIT_CTX.lock();
                let dev = match ctx.mmio_device.as_mut() {
                    Some(d) => d,
                    None => {
                        set_init_phase(WifiInitPhase::Failed);
                        return;
                    }
                };
                dev.send_init_commands()
            };
            match result {
                Ok(()) => {
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
            debug::print("iwlwifi", "step: init_done");
        }
        WifiInitPhase::Failed => {
            let mut ctx = WIFI_INIT_CTX.lock();
            let _ = ctx.mmio_device.take();
            let drv = ctx.driver_ctx;
            for mut buf in ctx.tx_bufs.drain(..) {
                if let Some(c) = drv { buf.free(c); }
            }
            for mut buf in ctx.rx_bufs.drain(..) {
                if let Some(c) = drv { buf.free(c); }
            }
            if let Some(mut ring) = ctx.tx_dma_ring.take() {
                if let Some(c) = drv { ring.free(c); }
            }
            if let Some(mut ring) = ctx.rx_dma_ring.take() {
                if let Some(c) = drv { ring.free(c); }
            }
            drop(ctx);
            WIFI_INIT_COMPLETED.store(true, core::sync::atomic::Ordering::Release);
            debug::print("iwlwifi", "step: init_failed");
        }
    }
}

// ── High-level API ─────────────────

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

    let candidates = select_firmware_list(probe.device_id);
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
            m.ip_address = Some(alloc::format!(
                "{}.{}.{}.{}",
                ip[0], ip[1], ip[2], ip[3]
            ));
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
