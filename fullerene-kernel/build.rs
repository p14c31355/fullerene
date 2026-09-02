use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use busybox_build::{BuildOptions, dynamic_glibc_interpreter_path, is_dynamic_glibc_x86_64_elf};

fn main() {
    // This cfg is also referenced by the host-built USB protocol tests, so
    // declare it before the AArch64-only build branch below.
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_probe)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_direct)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_super_speed)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_no_smmu)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma)");
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource)"
    );
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_android_resource_order)"
    );
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_event_ring_size_4096)"
    );
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_start_after_connect)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_xbl_ep0_in_data)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_xbl_event_dma)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_xbl_ep0_config)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0)");
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_xbl_post_endpoint_global)"
    );
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_xbl_stock_ep0_dma)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_xbl_raw_runstop)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_dt_hird_threshold)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_android_hs_lpm)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_abl_shared_hsphy)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_abl_devten)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_abl_ep_config)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_abl_command_params)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_abl_trb_flags)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_abl_event_consume)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_xbl_direction_trb)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_xbl_trb_chain)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_start_ungated)");
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_event_ring_at_runstop)"
    );
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_gadget_restart_at_runstop)"
    );
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_start_after_reset)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_dalepena_after_reset)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_reset_resource)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_reset_endpoints)");
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_ep0_reset_clear_stall)"
    );
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_ep0_reset_clear_test_mode)"
    );
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_ep0_reset_callback_first)"
    );
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order)"
    );
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_dwc31_dctl_only_reset)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_hsphy_before_reset)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_phyif_16bit)");
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_usbtrdtim, values(\"5\", \"6\", \"7\", \"8\", \"9\", \"10\", \"11\", \"12\", \"13\", \"14\", \"15\"))"
    );
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_enblslpm)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_u2_freeclk_clear)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_guctl3_usb20_retry_clear)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_guctl3_usb20_retry_set)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_ep0_initial_512)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_dcfg_superspeed)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_dcfg_ignstrmpp)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_usb2_susphy)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_ep0_stall_flush)");
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_ep0_short_first_desc)"
    );
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_ep0_txfifo_fix)");
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_start_at_connect_done)"
    );
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_preserve_core)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_preserve_runstop)");
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_clock_branches_rearm)"
    );
    println!(
        "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_core_hs_clock)"
    );
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_skip_usb2_phy_reset)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_refresh_hsphy_power)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_ep0_signal_probe)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_ep0_dma_adopt)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_ep0_smmu_gate)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_ep0_smmu_install)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_smmu_disable)");
    for stage in 1..=12 {
        println!(
            "cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_gadget_handoff_stop_after_{stage})"
        );
    }
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_probe_irq_power)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_probe_irq_typec)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_probe_irq_typec_role)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_probe_irq_pdc)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_probe_irq_smmu)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_bramble)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_entry_halt_probe)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_pullup_probe)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_halt_probe)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_cold_halt_probe)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_bare_pullup_probe)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_qemu_usb_sim)");
    println!("cargo:rustc-check-cfg=cfg(fullerene_aarch64_usb_hyper_bare)");
    // The AArch64 bootstrap binary is intentionally dependency-free. Avoid
    // the x86_64 kernel's generated userland/assets while building it; those
    // steps require host tools and x86-only target support.
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let platform = env::var("FULLERENE_AARCH64_PLATFORM").unwrap_or_default();
        let linker_script = out_dir.join("aarch64-linker.ld");
        fs::write(&linker_script, aarch64_linker_script(&platform)).unwrap();
        if platform == "bramble" {
            println!("cargo:rustc-cfg=fullerene_aarch64_bramble");
        }
        if env::var_os("FULLERENE_AARCH64_USB_PULLUP_PROBE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_pullup_probe");
        }
        if env::var_os("FULLERENE_AARCH64_USB_HALT_PROBE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_halt_probe");
        }
        if env::var_os("FULLERENE_AARCH64_USB_COLD_HALT_PROBE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_cold_halt_probe");
        }
        if env::var_os("FULLERENE_AARCH64_ENTRY_HALT_PROBE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_entry_halt_probe");
        }
        if env::var_os("FULLERENE_AARCH64_USB_BARE_PULLUP_PROBE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_bare_pullup_probe");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_PROBE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_probe");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_SUPER_SPEED").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_probe");
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_super_speed");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_DIRECT").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_direct");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_NO_SMMU").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_no_smmu");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_REUSE_FASTBOOT_DMA").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_NO_TRANSFER_RESOURCE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_no_transfer_resource");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_ANDROID_RESOURCE_ORDER").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_android_resource_order");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_EVENT_RING_SIZE_4096").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_event_ring_size_4096");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_START_AFTER_CONNECT").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_start_after_connect");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_XBL_DEFERRED_SETUP").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_XBL_EP0_IN_DATA").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_xbl_ep0_in_data");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_XBL_EVENT_DMA").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_xbl_event_dma");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_XBL_EP0_CONFIG").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_xbl_ep0_config");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_XBL_BETWEEN_EP0").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_XBL_POST_ENDPOINT_GLOBAL").is_some() {
            println!(
                "cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_xbl_post_endpoint_global"
            );
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_XBL_STOCK_EP0_DMA").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_xbl_stock_ep0_dma");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_XBL_RAW_RUNSTOP").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_xbl_raw_runstop");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_DT_HIRD_THRESHOLD").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_dt_hird_threshold");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_ANDROID_HS_LPM").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_android_hs_lpm");
        }
        if env::var_os("FULLERENE_AARCH64_USB_ABL_SHARED_HSPHY").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_abl_shared_hsphy");
        }
        if env::var_os("FULLERENE_AARCH64_USB_ABL_DEVTEN").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_abl_devten");
        }
        if env::var_os("FULLERENE_AARCH64_USB_ABL_EP_CONFIG").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_abl_ep_config");
        }
        if env::var_os("FULLERENE_AARCH64_USB_ABL_COMMAND_PARAMS").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_abl_command_params");
        }
        if env::var_os("FULLERENE_AARCH64_USB_ABL_TRB_FLAGS").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_abl_trb_flags");
        }
        if env::var_os("FULLERENE_AARCH64_USB_ABL_SETUP_TRB_BUFFER").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_abl_setup_trb_buffer");
        }
        if env::var_os("FULLERENE_AARCH64_USB_ABL_EVENT_CONSUME").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_abl_event_consume");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_XBL_DIRECTION_TRB").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_xbl_direction_trb");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_XBL_TRB_CHAIN").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_xbl_trb_chain");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_START_UNGATED").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_start_ungated");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_EVENT_RING_AT_RUNSTOP").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_event_ring_at_runstop");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_GADGET_RESTART_AT_RUNSTOP").is_some() {
            println!(
                "cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_gadget_restart_at_runstop"
            );
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_START_AFTER_RESET").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_start_after_reset");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_DALEPENA_AFTER_RESET").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_dalepena_after_reset");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_RESET_RESOURCE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_reset_resource");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_RESET_ENDPOINTS").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_reset_endpoints");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_EP0_RESET_CLEAR_STALL").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_ep0_reset_clear_stall");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_EP0_RESET_CLEAR_TEST_MODE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_ep0_reset_clear_test_mode");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_EP0_RESET_CALLBACK_FIRST").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_ep0_reset_callback_first");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_EP0_RESET_ANDROID_STATE_ORDER").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order");
        }
        if env::var_os("FULLERENE_AARCH64_USB_DWC31_DCTL_ONLY_RESET").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_dwc31_dctl_only_reset");
        }
        if env::var_os("FULLERENE_AARCH64_USB_HSPHY_BEFORE_RESET").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_hsphy_before_reset");
        }
        if env::var_os("FULLERENE_AARCH64_USB_PHYIF_16BIT").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_phyif_16bit");
        }
        if env::var_os("FULLERENE_AARCH64_USB_ENBLSLPM").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_enblslpm");
        }
        if env::var_os("FULLERENE_AARCH64_USB_ANDROID_BLOCK_RESET").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_android_block_reset");
        }
        if env::var_os("FULLERENE_AARCH64_USB_REFRESH_HSPHY_POWER").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_refresh_hsphy_power");
        }
        if env::var_os("FULLERENE_AARCH64_USB_SKIP_USB2_PHY_RESET").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_skip_usb2_phy_reset");
        }
        if env::var_os("FULLERENE_AARCH64_USB_U2_FREECLK_CLEAR").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_u2_freeclk_clear");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GUCTL3_USB20_RETRY_CLEAR").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_guctl3_usb20_retry_clear");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GUCTL3_USB20_RETRY_SET").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_guctl3_usb20_retry_set");
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_USBTRDTIM") {
            if let Ok(timing) = value.parse::<u32>() {
                if (5..=15).contains(&timing) {
                    println!("cargo:rustc-env=FULLERENE_USB_USBTRDTIM={timing}");
                }
            }
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_UTMI_PRECONNECT_READOUT") {
            println!("cargo:rustc-env=FULLERENE_USB_UTMI_PRECONNECT_READOUT={value}");
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_UTMI_POSTRUN_READOUT") {
            println!("cargo:rustc-env=FULLERENE_USB_UTMI_POSTRUN_READOUT={value}");
        }
        if env::var_os("FULLERENE_AARCH64_USB_EP0_INITIAL_512").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_ep0_initial_512");
        }
        if env::var_os("FULLERENE_AARCH64_USB_DCFG_SUPERSPEED").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_dcfg_superspeed");
        }
        if env::var_os("FULLERENE_AARCH64_USB_DCFG_IGNSTRMPP").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_dcfg_ignstrmpp");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_USB2_SUSPHY").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_usb2_susphy");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_EP0_STALL_FLUSH").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_ep0_stall_flush");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_EP0_SHORT_FIRST_DESC").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_ep0_short_first_desc");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_EP0_TXFIFO_FIX").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_ep0_txfifo_fix");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_START_AT_CONNECT_DONE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_start_at_connect_done");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_PRESERVE_CORE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_preserve_core");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_PRESERVE_RUNSTOP").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_preserve_runstop");
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_CLOCK_BRANCHES_REARM").is_some() {
            println!(
                "cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_clock_branches_rearm"
            );
        }
        if env::var_os("FULLERENE_AARCH64_USB_GADGET_HANDOFF_CORE_HS_CLOCK").is_some() {
            println!(
                "cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_core_hs_clock"
            );
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_CLOCK_STABLE_DELAY_US") {
            if let Ok(delay_us) = value.parse::<u32>() {
                if delay_us <= 20_000 {
                    println!("cargo:rustc-env=FULLERENE_USB_CLOCK_STABLE_DELAY_US={delay_us}");
                }
            }
        }
        if env::var_os("FULLERENE_AARCH64_USB_UTMI_REAPPLY_AFTER_RUNSTOP").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_UTMI_REAPPLY_AFTER_RUNSTOP=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_UTMI_REAPPLY_HALTED").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_UTMI_REAPPLY_HALTED=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_UTMI_WRITE_AFTER_DCTL").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_UTMI_WRITE_AFTER_DCTL=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_DALEPENA_AFTER_DCTL").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_DALEPENA_AFTER_DCTL=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_EP0_SIGNAL_PROBE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_ep0_signal_probe");
        }
        if env::var_os("FULLERENE_AARCH64_USB_EP0_DMA_ADOPT").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_ep0_dma_adopt");
        }
        if env::var_os("FULLERENE_AARCH64_USB_SMMU_DISABLE").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_smmu_disable");
        }
        if env::var_os("FULLERENE_AARCH64_USB_EP0_SMMU_INSTALL").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_ep0_smmu_install");
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_SIGNAL_CMD_GATE") {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_CMD_GATE={value}");
        }
        if env::var_os("FULLERENE_AARCH64_USB_PROBE_SINGLE_ATTEMPT").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_PROBE_SINGLE_ATTEMPT=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_U0_ARM_PROBE").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_U0_ARM_PROBE=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_U0_ARM_STOP_FIRST").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_U0_ARM_STOP_FIRST=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_WDT_BITE_CONTROL").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_WDT_BITE_CONTROL=1");
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_EP0_SMMU_GATE") {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_ep0_smmu_gate");
            println!("cargo:rustc-env=FULLERENE_USB_SMMU_GATE_TYPE={value}");
        }
        if let Some(stage) = env::var("FULLERENE_AARCH64_USB_GADGET_HANDOFF_STOP_STAGE")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
        {
            if (1..=12).contains(&stage) {
                println!("cargo:rustc-cfg=fullerene_aarch64_usb_gadget_handoff_stop_after_{stage}");
            }
        }
        match env::var("FULLERENE_AARCH64_USB_PROBE_IRQ_ROUTES").as_deref() {
            Ok("power") => println!("cargo:rustc-cfg=fullerene_aarch64_usb_probe_irq_power"),
            Ok("typec") => println!("cargo:rustc-cfg=fullerene_aarch64_usb_probe_irq_typec"),
            Ok("typec-role") => {
                println!("cargo:rustc-cfg=fullerene_aarch64_usb_probe_irq_typec_role")
            }
            Ok("pdc") => println!("cargo:rustc-cfg=fullerene_aarch64_usb_probe_irq_pdc"),
            Ok("smmu") => println!("cargo:rustc-cfg=fullerene_aarch64_usb_probe_irq_smmu"),
            Ok("") | Err(_) => {}
            Ok(other) => panic!(
                "FULLERENE_AARCH64_USB_PROBE_IRQ_ROUTES must be one of power,typec,typec-role,pdc,smmu (got {other:?})"
            ),
        }
        let probe_timeout = env::var("FULLERENE_AARCH64_USB_PROBE_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(120);
        println!("cargo:rustc-env=FULLERENE_USB_PROBE_TIMEOUT_SECS={probe_timeout}");
        if env::var_os("FULLERENE_AARCH64_USB_EP0_SIGNAL_SMMU_STATE").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_SMMU_STATE=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_EP0_SIGNAL_LINK_STATE").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_LINK_STATE=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_EP0_SIGNAL_RAW_LINK").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_RAW_LINK=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_EP0_SIGNAL_HEARTBEAT").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_HEARTBEAT=1");
        }
        if let Some(secs) = env::var("FULLERENE_AARCH64_USB_QUIET_AFTER")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
        {
            println!("cargo:rustc-env=FULLERENE_USB_QUIET_AFTER_SECS={secs}");
        }
        if let Some(secs) = env::var("FULLERENE_AARCH64_USB_PROBE_OBSERVE_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
        {
            println!("cargo:rustc-env=FULLERENE_USB_PROBE_OBSERVE_SECS={secs}");
        }
        if env::var_os("FULLERENE_AARCH64_USB_SIGNAL_DIAG_PUBLISH").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_DIAG_PUBLISH=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_SIGNAL_DMA_PROBE").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_DMA_PROBE=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_SIGNAL_DMA_POST_RUNSTOP").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_DMA_POST_RUNSTOP=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_SIGNAL_RAM_GATE").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_RAM_GATE=1");
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_SIGNAL_FSR_GATE") {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_FSR_GATE={value}");
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_PREV_TRACE_GATE") {
            println!("cargo:rustc-env=FULLERENE_USB_PREV_TRACE_GATE={value}");
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_SIGNAL_EVT_DATA_GATE") {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_EVT_DATA_GATE={value}");
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_PON_READOUT") {
            println!("cargo:rustc-env=FULLERENE_USB_PON_READOUT={value}");
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_SIGNAL_RSC_GATE") {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_RSC_GATE={value}");
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_SIGNAL_CFG_GATE") {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_CFG_GATE={value}");
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_SIGNAL_RAMCLK_GATE") {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_RAMCLK_GATE={value}");
        }
        if env::var_os("FULLERENE_AARCH64_USB_SMMU_INSTALL_ALL").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SMMU_INSTALL_ALL=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_SIGNAL_DROP_VBUS").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_DROP_VBUS=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_SKIP_TYPEC_SPMI").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SKIP_TYPEC_SPMI=1");
        }
        if let Some(secs) = env::var("FULLERENE_AARCH64_USB_CONNECT_DELAY")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
        {
            println!("cargo:rustc-env=FULLERENE_USB_CONNECT_DELAY_SECS={secs}");
        }
        if env::var_os("FULLERENE_AARCH64_USB_EP0_SIGNAL_PRE_DROP").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_PRE_DROP=1");
        }
        if let Some(code) = env::var("FULLERENE_AARCH64_USB_EP0_SIGNAL_EARLY_DROP")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
        {
            println!("cargo:rustc-env=FULLERENE_USB_SIGNAL_EARLY_DROP={code}");
        }
        if let Ok(value) = env::var("FULLERENE_AARCH64_USB_SWDD_FNID") {
            println!("cargo:rustc-env=FULLERENE_USB_SWDD_FNID={value}");
        }
        if env::var_os("FULLERENE_AARCH64_USB_UTMI_60MHZ").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_UTMI_60MHZ=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_UTMI_19_2MHZ").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_UTMI_19_2MHZ=1");
        }
        if env::var_os("FULLERENE_AARCH64_USB_SWDD_SKIP").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_SWDD_SKIP=1");
        }
        if let Some(value) = env::var("FULLERENE_AARCH64_USB_BARE_PULLUP_STOP_AFTER")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
        {
            println!("cargo:rustc-env=FULLERENE_USB_BARE_PULLUP_STOP_AFTER={value}");
        }
        if env::var_os("FULLERENE_AARCH64_USB_ARM_BLIP").is_some() {
            println!("cargo:rustc-env=FULLERENE_USB_ARM_BLIP=1");
        }
        if let Some(secs) = env::var("FULLERENE_AARCH64_USB_ABS_RESET_SECS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
        {
            println!("cargo:rustc-env=FULLERENE_USB_ABS_RESET_SECS={secs}");
        }
        // Hyper-bare bisection fires the bare pull-up at the very first
        // instruction after EL1 entry. Gate it to bramble: the QEMU
        // preflight inherits the harness environment but builds for
        // qemu-virt, where this cfg must stay off.
        if env::var_os("FULLERENE_AARCH64_USB_HYPER_BARE").is_some()
            && env::var("FULLERENE_AARCH64_PLATFORM").is_ok_and(|p| p == "bramble")
        {
            println!("cargo:rustc-cfg=fullerene_aarch64_usb_hyper_bare");
        }
        if env::var_os("FULLERENE_AARCH64_QEMU_USB_SIM").is_some() {
            println!("cargo:rustc-cfg=fullerene_aarch64_qemu_usb_sim");
        }
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_PLATFORM");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_ENTRY_HALT_PROBE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_PULLUP_PROBE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_HALT_PROBE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_COLD_HALT_PROBE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_BARE_PULLUP_PROBE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_PROBE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_SUPER_SPEED");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_DIRECT");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_NO_SMMU");
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_REUSE_FASTBOOT_DMA"
        );
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_NO_TRANSFER_RESOURCE"
        );
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_ANDROID_RESOURCE_ORDER"
        );
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_PRESERVE_CORE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_PRESERVE_RUNSTOP");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_CLOCK_STABLE_DELAY_US");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_UTMI_REAPPLY_AFTER_RUNSTOP");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_UTMI_REAPPLY_HALTED");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_UTMI_WRITE_AFTER_DCTL");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_DALEPENA_AFTER_DCTL");
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_CLOCK_BRANCHES_REARM"
        );
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_CORE_HS_CLOCK"
        );
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_ANDROID_HS_LPM"
        );
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_ABL_SHARED_HSPHY");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_ABL_EP_CONFIG");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_ABL_COMMAND_PARAMS");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_ABL_SETUP_TRB_BUFFER");
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_START_AFTER_CONNECT"
        );
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_XBL_DIRECTION_TRB"
        );
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_XBL_TRB_CHAIN");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_START_UNGATED");
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_EVENT_RING_AT_RUNSTOP"
        );
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_GADGET_RESTART_AT_RUNSTOP"
        );
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_START_AFTER_RESET"
        );
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_DALEPENA_AFTER_RESET"
        );
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_RESET_RESOURCE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_RESET_ENDPOINTS");
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_EP0_RESET_ANDROID_STATE_ORDER"
        );
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_DWC31_DCTL_ONLY_RESET");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_HSPHY_BEFORE_RESET");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_PHYIF_16BIT");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_ENBLSLPM");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_ANDROID_BLOCK_RESET");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_REFRESH_HSPHY_POWER");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SKIP_USB2_PHY_RESET");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_U2_FREECLK_CLEAR");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GUCTL3_USB20_RETRY_CLEAR");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GUCTL3_USB20_RETRY_SET");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_USBTRDTIM");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_UTMI_PRECONNECT_READOUT");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_UTMI_POSTRUN_READOUT");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_EP0_INITIAL_512");
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_START_AT_CONNECT_DONE"
        );
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_EP0_SIGNAL_PROBE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_EP0_SIGNAL_SMMU_STATE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_EP0_SIGNAL_LINK_STATE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_EP0_SIGNAL_RAW_LINK");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_EP0_SIGNAL_EARLY_DROP");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_EP0_SIGNAL_PRE_DROP");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_EP0_SIGNAL_HEARTBEAT");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_EP0_DMA_ADOPT");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_EP0_SMMU_GATE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SIGNAL_CMD_GATE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_PROBE_SINGLE_ATTEMPT");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_U0_ARM_PROBE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_U0_ARM_STOP_FIRST");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_WDT_BITE_CONTROL");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SWDD_FNID");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SWDD_SKIP");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_UTMI_60MHZ");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_UTMI_19_2MHZ");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_BARE_PULLUP_STOP_AFTER");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_HYPER_BARE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_ARM_BLIP");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_EP0_STALL_FLUSH");
        println!(
            "cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_EP0_SHORT_FIRST_DESC"
        );
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_EP0_TXFIFO_FIX");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_ABS_RESET_SECS");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_EP0_SMMU_INSTALL");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SMMU_DISABLE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SIGNAL_DROP_VBUS");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SIGNAL_DMA_PROBE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SIGNAL_DMA_POST_RUNSTOP");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SIGNAL_RAM_GATE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SIGNAL_FSR_GATE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SIGNAL_EVT_DATA_GATE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SIGNAL_RSC_GATE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SIGNAL_CFG_GATE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SIGNAL_RAMCLK_GATE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_PON_READOUT");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SMMU_INSTALL_ALL");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SKIP_TYPEC_SPMI");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_SIGNAL_DIAG_PUBLISH");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_QUIET_AFTER");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_PROBE_OBSERVE_SECS");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_CONNECT_DELAY");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_GADGET_HANDOFF_STOP_STAGE");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_PROBE_IRQ_ROUTES");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_PROBE_TIMEOUT_SECS");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_QEMU_USB_SIM");
        println!(
            "cargo:rustc-link-arg-bin=fullerene-kernel-aarch64=-T{}",
            linker_script.display()
        );
        println!(
            "cargo:rustc-link-arg-bin=fullerene-kernel-aarch64-probe=-T{}",
            linker_script.display()
        );
        println!(
            "cargo:rustc-link-arg-bin=fullerene-kernel-aarch64-entry-halt-probe=-T{}",
            linker_script.display()
        );
        println!(
            "cargo:rustc-link-arg-bin=fullerene-kernel-aarch64-usb-probe=-T{}",
            linker_script.display()
        );
        println!("cargo:rerun-if-changed=src/arch/aarch64");
        println!("cargo:rerun-if-env-changed=FULLERENE_AARCH64_USB_DMA_ORIGIN");
        return;
    }

    // The ESP32 embedded profile intentionally builds no generated desktop
    // userland, BusyBox ports, musl smokes, or WASI examples. Fullerene's
    // Xtensa kernel owns its bounded native application set.
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("xtensa") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    generate_solvent_linux(&manifest_dir, &out_dir);

    // ── Declare expected cfg flags ────────────────────────────────
    println!("cargo::rustc-check-cfg=cfg(have_ports_cpio)");
    println!("cargo::rustc-check-cfg=cfg(have_viewer_wasm)");
    println!("cargo::rustc-check-cfg=cfg(have_emulsion_wasm)");
    println!("cargo::rustc-check-cfg=cfg(have_linux_musl_hello)");
    println!("cargo::rustc-check-cfg=cfg(linux_musl_smoke)");
    println!("cargo::rustc-check-cfg=cfg(ipc_kernel_smoke)");
    println!("cargo::rustc-check-cfg=cfg(usb_xhci_smoke)");
    println!("cargo::rustc-check-cfg=cfg(have_busybox)");
    println!("cargo::rustc-check-cfg=cfg(linux_busybox_smoke)");
    println!("cargo::rustc-check-cfg=cfg(linux_busybox_smoke_qemu_exit)");
    println!("cargo:rerun-if-env-changed=FULLERENE_BUILD_PORTS");
    println!("cargo:rerun-if-env-changed=FULLERENE_LINUX_MUSL_SMOKE");
    println!("cargo:rerun-if-env-changed=FULLERENE_IPC_KERNEL_SMOKE");
    println!("cargo:rerun-if-env-changed=FULLERENE_USB_XHCI_SMOKE");
    println!("cargo:rerun-if-env-changed=FULLERENE_BUSYBOX");
    println!("cargo:rerun-if-env-changed=FULLERENE_BUSYBOX_CC");
    println!("cargo:rerun-if-env-changed=FULLERENE_BUSYBOX_SMOKE");
    println!("cargo:rerun-if-env-changed=FULLERENE_BUSYBOX_DYNAMIC_LINKER");
    println!("cargo:rerun-if-env-changed=FULLERENE_BUSYBOX_LIBC");
    println!("cargo:rerun-if-env-changed=FULLERENE_BUSYBOX_SMOKE_QEMU_EXIT");
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir
            .join("assets/audio/fullerene_startup_sound.wav")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir
            .join("assets/audio/fullerene_startup_sound.mp3")
            .display()
    );
    let linux_musl_smoke_requested = env::var_os("FULLERENE_LINUX_MUSL_SMOKE").is_some();
    let ipc_kernel_smoke_requested = env::var_os("FULLERENE_IPC_KERNEL_SMOKE").is_some();
    let usb_xhci_smoke_requested = env::var_os("FULLERENE_USB_XHCI_SMOKE").is_some();

    if usb_xhci_smoke_requested {
        println!("cargo:rustc-cfg=usb_xhci_smoke");
    }

    let workspace_root = manifest_dir.parent().unwrap();
    let have_busybox = embed_busybox(&out_dir, workspace_root);
    if env::var_os("FULLERENE_BUSYBOX_SMOKE").is_some() {
        assert!(
            have_busybox,
            "FULLERENE_BUSYBOX_SMOKE requires a dynamically linked glibc BusyBox; set FULLERENE_BUSYBOX"
        );
        println!("cargo:rustc-cfg=linux_busybox_smoke");
        if env::var_os("FULLERENE_BUSYBOX_SMOKE_QEMU_EXIT").is_some() {
            println!("cargo:rustc-cfg=linux_busybox_smoke_qemu_exit");
        }
    }

    // ── Propagate .driverignore cfg flags from Nitrogen ──────────
    let nitrogen_dir = manifest_dir.parent().unwrap().join("nitrogen");
    let ignore_path = nitrogen_dir.join(".driverignore");
    println!("cargo:rerun-if-changed={}", ignore_path.display());

    let known_drivers = &[
        "audio",
        "framebuffer",
        "hda",
        "ioapic",
        "iommu",
        "iwlwifi",
        "pic",
        "ps2",
        "storage",
        "usb",
        "virtio",
        "wifi",
    ];
    for name in known_drivers {
        println!("cargo::rustc-check-cfg=cfg(nitrogen_no_{})", name);
    }

    if ignore_path.exists() {
        let content = fs::read_to_string(&ignore_path).unwrap_or_default();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mod_name = line.strip_suffix('/').unwrap_or(line);
            let clean: String = mod_name
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            if !clean.is_empty() {
                println!("cargo:rustc-cfg=nitrogen_no_{}", clean);
            }
        }
    }

    // ── Build application ports from submodule sources ──────────
    let toluene_dir = workspace_root.join("toluene");
    let ports_dir = workspace_root.join("target").join("ports");
    let count = build_ports_cpio(&toluene_dir, &ports_dir, &out_dir);
    if count > 0 {
        println!("cargo:rustc-cfg=have_ports_cpio");
    }

    // ── Build WASI test app ──────────────────────────────────────
    let wasm_src = manifest_dir
        .join("..")
        .join("toluene")
        .join("apps")
        .join("hello_wasi.rs");
    let wasm_out = out_dir.join("hello.wasm");

    println!("cargo:rerun-if-changed={}", wasm_src.display());

    // Use the RUSTC from cargo's build environment — it points to the correct
    // toolchain (respecting rust-toolchain.toml). Derive sysroot from it.
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());

    // ── Build the userland init and shell ────────────────────────
    // Both are freestanding native ELF programs.  They are embedded only as
    // boot payloads; neither source is linked into the kernel.
    let shell_src = manifest_dir.join("examples").join("native_shell.rs");
    let shell_out = out_dir.join("native_shell");
    let launchd_src = manifest_dir.join("examples").join("native_launchd.rs");
    let launchd_out = out_dir.join("native_launchd");
    for source in [&shell_src, &launchd_src] {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    let native_args = [
        "--edition=2024",
        "--target",
        "x86_64-unknown-linux-gnu",
        "-C",
        "panic=abort",
        "-C",
        "relocation-model=static",
        "-C",
        "link-arg=-nostdlib",
        "-C",
        "link-arg=-Wl,--no-dynamic-linker",
        "-C",
        "link-arg=-e",
        "-C",
        "link-arg=_start",
        "-C",
        "opt-level=2",
        "-C",
        "strip=debuginfo",
        "-o",
    ];
    let shell_status = Command::new(&rustc)
        .args(native_args)
        .arg(&shell_out)
        .arg(&shell_src)
        .status()
        .expect("could not start rustc for native shell");
    assert!(
        shell_status.success(),
        "native user shell compilation failed (rustc exit {shell_status}); it requires the x86_64-unknown-linux-gnu target and a host linker: rustup target add x86_64-unknown-linux-gnu"
    );
    let launchd_status = Command::new(&rustc)
        .args(native_args)
        .env("FULLERENE_SHELL_IMAGE", &shell_out)
        .arg(&launchd_out)
        .arg(&launchd_src)
        .status()
        .expect("could not start rustc for launchd");
    assert!(
        launchd_status.success(),
        "launchd compilation failed (rustc exit {launchd_status}); it requires the x86_64-unknown-linux-gnu target and a host linker: rustup target add x86_64-unknown-linux-gnu"
    );
    // Keep the example target buildable as well as the generated kernel
    // payload. `native_launchd.rs` embeds the shell image, so Cargo must pass
    // the same path when it compiles the example directly (for checks and
    // documentation tooling); the ad-hoc rustc invocation above only sets
    // the variable for that one child process.
    println!(
        "cargo:rustc-env=FULLERENE_SHELL_IMAGE={}",
        shell_out.display()
    );
    println!(
        "cargo:rustc-env=FULLERENE_LAUNCHD_IMAGE={}",
        launchd_out.display()
    );

    // ── Build the native IPC kernel smoke fixture ─────────────────
    // This is a freestanding ELF that invokes Fullerene's native channel
    // syscalls directly. It deliberately has no libc or Rust SDK dependency,
    // so the smoke path measures the actual kernel boundary.
    if ipc_kernel_smoke_requested {
        let ipc_src = manifest_dir.join("examples").join("native_ipc_rate.rs");
        let ipc_out = out_dir.join("native_ipc_rate");
        println!("cargo:rerun-if-changed={}", ipc_src.display());
        let ipc_status = Command::new(&rustc)
            .args([
                "--edition=2024",
                "--target",
                "x86_64-unknown-linux-gnu",
                "-C",
                "panic=abort",
                "-C",
                "relocation-model=static",
                "-C",
                "link-arg=-nostdlib",
                "-C",
                "link-arg=-Wl,--no-dynamic-linker",
                "-C",
                "link-arg=-e",
                "-C",
                "link-arg=_start",
                "-C",
                "opt-level=2",
                "-C",
                "strip=debuginfo",
                "-o",
            ])
            .arg(&ipc_out)
            .arg(&ipc_src)
            .status();
        match ipc_status {
            Ok(status) if status.success() => {
                println!("cargo:rustc-cfg=ipc_kernel_smoke");
            }
            Ok(status) => panic!(
                "FULLERENE_IPC_KERNEL_SMOKE requires the native IPC fixture to compile (rustc exit {status})"
            ),
            Err(error) => {
                panic!("FULLERENE_IPC_KERNEL_SMOKE could not start rustc for its fixture: {error}")
            }
        }
    }

    // ── Build the ordinary Rust std / musl Linux fixture ─────────
    // Watch the target libdir as well as the source. If a developer installs
    // the target after an earlier failed build, Cargo must rerun this script
    // instead of reusing the old "fixture unavailable" cfg result.
    if let Ok(output) = Command::new(&rustc)
        .args([
            "--print",
            "target-libdir",
            "--target",
            "x86_64-unknown-linux-musl",
        ])
        .output()
        && let Ok(target_libdir) = String::from_utf8(output.stdout)
    {
        let target_libdir = target_libdir.trim();
        if !target_libdir.is_empty() {
            println!("cargo:rerun-if-changed={target_libdir}");
        }
    }

    let linux_musl_src = manifest_dir.join("examples").join("linux_musl_hello.rs");
    let linux_musl_out = out_dir.join("linux_musl_hello");
    println!("cargo:rerun-if-changed={}", linux_musl_src.display());
    match Command::new(&rustc)
        .args([
            "--edition=2024",
            "--target",
            "x86_64-unknown-linux-musl",
            "-C",
            "target-feature=+crt-static",
            "-C",
            "relocation-model=static",
            "-C",
            "opt-level=2",
            "-C",
            "strip=debuginfo",
            "-o",
        ])
        .arg(&linux_musl_out)
        .arg(&linux_musl_src)
        .status()
    {
        Ok(status) if status.success() => {
            println!("cargo:rustc-cfg=have_linux_musl_hello");
            if linux_musl_smoke_requested {
                println!("cargo:rustc-cfg=linux_musl_smoke");
            }
        }
        Ok(_) => {
            assert!(
                !linux_musl_smoke_requested,
                "FULLERENE_LINUX_MUSL_SMOKE requires the x86_64-unknown-linux-musl target; \
                 install it with: rustup target add x86_64-unknown-linux-musl"
            );
            println!(
                "cargo:warning=Rust std/musl example build failed; install it with: \
                 rustup target add x86_64-unknown-linux-musl"
            );
        }
        Err(error) => {
            assert!(
                !linux_musl_smoke_requested,
                "FULLERENE_LINUX_MUSL_SMOKE could not start rustc for its fixture: {error}"
            );
            println!(
                "cargo:warning=Rust std/musl example compiler could not start: {}",
                error
            );
        }
    }

    let sysroot = String::from_utf8(
        Command::new(&rustc)
            .args(["--print", "sysroot"])
            .output()
            .expect("Failed to get sysroot from rustc")
            .stdout,
    )
    .expect("Invalid UTF-8 from rustc --print sysroot")
    .trim()
    .to_string();

    let startup_sound_src = workspace_root
        .join("toluene")
        .join("apps")
        .join("startup_sound.rs");
    let startup_sound_out = out_dir.join("startup_sound.wasm");
    println!("cargo:rerun-if-changed={}", startup_sound_src.display());
    build_standalone_wasm(&rustc, &sysroot, &wasm_src, &wasm_out, "WASI test app");
    build_standalone_wasm(
        &rustc,
        &sysroot,
        &startup_sound_src,
        &startup_sound_out,
        "startup sound WASM app",
    );

    // ── Build WASM viewer app (with cargo for dependencies) ──
    let viewer_dir = workspace_root.join("toluene").join("viewer");
    let viewer_out = out_dir.join("viewer.wasm");
    let viewer_target_dir = workspace_root.join("target").join("wasm-viewer-build");
    let viewer_cache = viewer_target_dir
        .join("wasm32-wasip1")
        .join("release")
        .join("viewer.wasm");
    println!(
        "cargo:rerun-if-changed={}",
        viewer_dir.join("src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        viewer_dir.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        viewer_dir.join("Cargo.lock").display()
    );

    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    if build_nested_wasm(
        &cargo,
        &viewer_dir,
        &viewer_target_dir,
        &viewer_cache,
        &viewer_out,
        "viewer",
    ) {
        println!("cargo:rustc-cfg=have_viewer_wasm");
    }

    // ── Build the Emulsion screenshot app ──────────────────────
    let emulsion_dir = workspace_root.join("toluene").join("emulsion");
    let emulsion_out = out_dir.join("emulsion.wasm");
    let emulsion_target_dir = workspace_root.join("target").join("wasm-emulsion-build");
    let emulsion_cache = emulsion_target_dir
        .join("wasm32-wasip1")
        .join("release")
        .join("emulsion.wasm");
    println!(
        "cargo:rerun-if-changed={}",
        emulsion_dir.join("src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        emulsion_dir.join("Cargo.toml").display()
    );
    if build_nested_wasm(
        &cargo,
        &emulsion_dir,
        &emulsion_target_dir,
        &emulsion_cache,
        &emulsion_out,
        "Emulsion",
    ) {
        println!("cargo:rustc-cfg=have_emulsion_wasm");
    }
}

/// Generate the tiny platform-specific linker script for the freestanding
/// AArch64 bootstrap image. Keeping this in Rust makes the repository's
/// architecture/platform layout the source of truth while still giving lld
/// the script syntax it requires at link time.
fn aarch64_linker_script(platform: &str) -> String {
    let image_base = if platform == "bramble" {
        // Bramble's DRAM starts at 0x80000000. The standard arm64 Image
        // text offset is 0x80000, and the generated 64-byte Image header is
        // immediately before the Rust payload.
        "0x80080040"
    } else {
        "0x42000040"
    };

    let requested_dma_origin = match env::var("FULLERENE_AARCH64_USB_DMA_ORIGIN") {
        Ok(value) => {
            let value = value.trim();
            let digits = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .unwrap_or(value);
            let address = u64::from_str_radix(digits, 16).unwrap_or_else(|_| {
                panic!(
                    "FULLERENE_AARCH64_USB_DMA_ORIGIN must be a hexadecimal address, got {value:?}"
                )
            });
            assert!(
                address != 0 && address & 0xfff == 0,
                "FULLERENE_AARCH64_USB_DMA_ORIGIN must be nonzero and 4K-aligned, got {value:?}"
            );
            Some(address)
        }
        Err(env::VarError::NotPresent) => None,
        Err(env::VarError::NotUnicode(_)) => {
            panic!("FULLERENE_AARCH64_USB_DMA_ORIGIN must be valid UTF-8")
        }
    };
    let usb_dma_origin = if let Some(origin) = requested_dma_origin {
        // Empirical override: the stock 0x9b800000 window failed a CPU
        // readback gate on real hardware, so the Bramble A/B harness can
        // relocate the DMA section to a candidate DRAM region per run.
        format!(". = {origin:#x};")
    } else if platform == "bramble" {
        // Empirically verified on the Bramble handset: the previously used
        // 0x9b800000 window (after the modem_wlan carveout) FAILED a CPU
        // readback gate, so every DMA object written there - event ring,
        // TRBs, SETUP buffer, and the retained trace - silently vanished.
        // 0x90000000 is the start of the vendor DT's USB DMA pool
        // (iova_base), passed the readback gate, and the controller's event
        // DMA writes land there. The FULLERENE_AARCH64_USB_DMA_ORIGIN
        // override still wins for one-run A/B experiments.
        ". = 0x90000000;".to_string()
    } else {
        String::new()
    };

    format!(
        r#"ENTRY(_start)

SECTIONS
{{
    /* The generated Linux Image header occupies the 64 bytes immediately
       before this payload. */
    . = {image_base};

    .text.boot : ALIGN(4)
    {{
        KEEP(*(.text.boot))
    }}

    .text.exception_vectors : ALIGN(2K)
    {{
        KEEP(*(.text.exception_vectors))
    }}

    .text : ALIGN(4K)
    {{
        *(.text .text.*)
    }}

    .rodata : ALIGN(4K)
    {{
        *(.rodata .rodata.*)
    }}

    .data : ALIGN(4K)
    {{
        *(.data .data.*)
    }}

    /* Keep static-PIE relocation records inside the flat Image. The Rust
       bootstrap applies them before normal code touches relocated data. */
    .rela.dyn : ALIGN(8)
    {{
        __rela_dyn_start = .;
        *(.rela.dyn)
        __rela_dyn_end = .;
    }}

    .bss (NOLOAD) : ALIGN(4K)
    {{
        . = ALIGN(0x200000);
        __bss_start = .;
        *(.bss .bss.* COMMON)
        . = ALIGN(4K);
        __bss_end = .;
    }}

    /*
       DWC3 DMA objects have a fixed Bramble address. Keep .usb_dma first so
       retaining the post-mortem trace cannot move the event ring or TRBs.
    */
    {usb_dma_origin}
    .usb_dma (NOLOAD) : ALIGN(4K)
    {{
        __usb_dma_start = .;
        KEEP(*(.usb_dma .usb_dma.*))
        . = ALIGN(4K);
        __usb_dma_end = .;
    }}

    /*
       This is a warm-reset-retained post-mortem area. It must not be part of
       .bss or .usb_dma: the bootstrap clears the DMA region while the next
       boot can dump this trace before starting a new attempt.
    */
    .usb_trace (NOLOAD) : ALIGN(4K)
    {{
        __usb_trace_start = .;
        KEEP(*(.usb_trace .usb_trace.*))
        . = ALIGN(4K);
        __usb_trace_end = .;
    }}

    /DISCARD/ :
    {{
        *(.eh_frame .eh_frame_hdr)
        *(.comment)
    }}
}}
"#
    )
}

/// Validate and stage a dynamically linked glibc x86_64 BusyBox and its
/// runtime dependencies for the initramfs.
///
/// The binary is generated outside the Rust workspace instead of being
/// checked into it. This keeps the source tree small while allowing a release
/// build to choose its BusyBox configuration/version.
fn embed_busybox(out_dir: &Path, workspace_root: &Path) -> bool {
    let explicit = env::var_os("FULLERENE_BUSYBOX").map(PathBuf::from);
    let generated = workspace_root.join("target/busybox/busybox");
    if let Some(path) = explicit.as_ref() {
        println!("cargo:rerun-if-changed={}", path.display());
        let data = fs::read(path)
            .unwrap_or_else(|_| panic!("FULLERENE_BUSYBOX was not found: {}", path.display()));
        if !is_dynamic_glibc_x86_64_elf(&data) {
            panic!(
                "BusyBox must be a dynamically linked glibc x86_64 ELF: {}",
                path.display()
            );
        }
        busybox_build::validate_fullerene_busybox(path).unwrap_or_else(|error| panic!("{error}"));
        fs::write(out_dir.join("busybox"), &data).unwrap_or_else(|error| {
            panic!("cannot stage BusyBox in {}: {error}", out_dir.display())
        });
        if !stage_busybox_runtime(out_dir, path, &data) {
            if env::var_os("FULLERENE_BUSYBOX_SMOKE").is_some() {
                panic!("FULLERENE_BUSYBOX_SMOKE requires BusyBox runtime dependencies");
            }
            return false;
        }
        write_busybox_contract(out_dir);
        println!("cargo:rustc-cfg=have_busybox");
        return true;
    }
    if explicit.is_none() {
        let source = workspace_root.join("toluene/busybox");
        println!("cargo:rerun-if-changed={}", source.display());
        println!(
            "cargo:rerun-if-changed={}",
            source.join("Makefile").display()
        );
        let options = BuildOptions {
            source,
            // Each Cargo build script gets a private out directory. The
            // staged binary remains shared, guarded by busybox-build's lock.
            build_dir: out_dir.join("busybox-build"),
            output: generated.clone(),
            compiler: env::var_os("FULLERENE_BUSYBOX_CC"),
            jobs: None,
            clean: false,
        };
        if let Err(error) = busybox_build::build(&options) {
            if env::var_os("FULLERENE_BUSYBOX_SMOKE").is_some() {
                panic!("FULLERENE_BUSYBOX_SMOKE BusyBox build failed: {error}");
            }
            // BusyBox is an optional cached port.  A missing toolchain or
            // submodule must not turn an otherwise valid kernel check into a
            // warning; the initramfs simply omits the optional package.
            println!("cargo:warning=BusyBox unavailable: {error}");
        }
    }
    let candidates = [
        generated.clone(),
        PathBuf::from("/usr/bin/busybox"),
        PathBuf::from("/bin/busybox"),
    ];

    for path in candidates {
        if path != generated {
            println!("cargo:rerun-if-changed={}", path.display());
        }
        let Ok(data) = fs::read(&path) else {
            continue;
        };
        if !is_dynamic_glibc_x86_64_elf(&data)
            || busybox_build::validate_fullerene_busybox(&path).is_err()
        {
            continue;
        }
        fs::write(out_dir.join("busybox"), &data).unwrap_or_else(|error| {
            panic!("cannot stage BusyBox in {}: {error}", out_dir.display())
        });
        if !stage_busybox_runtime(out_dir, &path, &data) {
            continue;
        }
        write_busybox_contract(out_dir);
        println!("cargo:rustc-cfg=have_busybox");
        return true;
    }

    false
}

fn stage_busybox_runtime(out_dir: &Path, busybox_path: &Path, busybox_data: &[u8]) -> bool {
    let Some(interpreter) = dynamic_glibc_interpreter_path(busybox_data) else {
        println!(
            "cargo:warning=BusyBox has no accepted glibc PT_INTERP: {}",
            busybox_path.display()
        );
        return false;
    };
    let linker = env::var_os("FULLERENE_BUSYBOX_DYNAMIC_LINKER")
        .map(PathBuf::from)
        .or_else(|| {
            [
                "/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
                "/lib64/ld-linux-x86-64.so.2",
                "/usr/lib64/ld-linux-x86-64.so.2",
                "/lib/ld-linux-x86-64.so.2",
                interpreter,
            ]
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
        });
    let libc = env::var_os("FULLERENE_BUSYBOX_LIBC")
        .map(PathBuf::from)
        .or_else(|| {
            [
                "/usr/lib/x86_64-linux-gnu/libc.so.6",
                "/lib/x86_64-linux-gnu/libc.so.6",
                "/lib64/libc.so.6",
                "/usr/lib64/libc.so.6",
                "/usr/lib/libc.so.6",
            ]
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
        });
    let (Some(linker), Some(libc)) = (linker, libc) else {
        println!(
            "cargo:warning=BusyBox runtime dependencies were not found for {}; set FULLERENE_BUSYBOX_DYNAMIC_LINKER and FULLERENE_BUSYBOX_LIBC",
            busybox_path.display()
        );
        return false;
    };
    for path in [&linker, &libc] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    if let Err(error) = fs::copy(&linker, out_dir.join("busybox-interpreter")) {
        println!("cargo:warning=cannot stage {}: {error}", linker.display());
        return false;
    }
    if let Err(error) = fs::copy(&libc, out_dir.join("busybox-libc")) {
        println!("cargo:warning=cannot stage {}: {error}", libc.display());
        return false;
    }
    true
}

fn write_busybox_contract(out_dir: &Path) {
    let names = busybox_build::fullerene_busybox_applet_names().collect::<Vec<_>>();
    let contract = names.join("\n");
    fs::write(out_dir.join("busybox-applets.txt"), format!("{contract}\n"))
        .unwrap_or_else(|error| panic!("cannot write BusyBox applet contract: {error}"));
    fs::write(
        out_dir.join("busybox-applet-count.rs"),
        format!("{}usize", names.len()),
    )
    .unwrap_or_else(|error| panic!("cannot write BusyBox applet count: {error}"));
}

fn build_standalone_wasm(rustc: &str, sysroot: &str, source: &Path, output: &Path, label: &str) {
    let status = Command::new(rustc)
        .args([
            "--target",
            "wasm32-wasip1",
            "--sysroot",
            sysroot,
            "-C",
            "opt-level=s",
            "-C",
            "lto=yes",
            "-C",
            "codegen-units=1",
            "-C",
            "strip=symbols",
            "-o",
        ])
        .arg(output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("Failed to execute rustc for {label}: {error}"));
    if !status.success() {
        panic!(
            "Failed to compile {label} from '{}'. Make sure the wasm32-wasip1 target is installed: \
             rustup target add wasm32-wasip1",
            source.display()
        );
    }
}

fn build_nested_wasm(
    cargo: &str,
    manifest_dir: &Path,
    target_dir: &Path,
    cache: &Path,
    output: &Path,
    label: &str,
) -> bool {
    let status = Command::new(cargo)
        .args([
            "build",
            "--target",
            "wasm32-wasip1",
            "--release",
            "--target-dir",
        ])
        .arg(target_dir)
        .current_dir(manifest_dir)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status();
    let build_succeeded = match status {
        Ok(status) if status.success() => true,
        Ok(_) => {
            println!("cargo:warning=WASM {label} build failed (will continue without it)");
            false
        }
        Err(error) => {
            println!("cargo:warning=WASM {label} build could not start: {error}");
            false
        }
    };
    if build_succeeded && cache.exists() {
        fs::copy(cache, output)
            .unwrap_or_else(|error| panic!("Failed to copy WASM {label} binary: {error}"));
        return true;
    }
    false
}

/// Build application ports from submodule sources and package into CPIO.
///
/// For each known port, this function:
/// 1. Locates its submodule under `toluene/<name>/`
/// 2. If a cached ELF exists at `target/ports/<name>/app.bin`, uses it
/// 3. Otherwise, if `FULLERENE_BUILD_PORTS=1` is set, builds from source
///    (or downloads a pre‑built release) to produce a Linux ELF binary
/// 4. Caches any freshly-built binary at `target/ports/<name>/app.bin`
/// 5. Packages every successfully‑built port into a single CPIO archive
///
/// Returns the number of ports successfully packaged.
fn build_ports_cpio(toluene_dir: &Path, ports_dir: &Path, out_dir: &Path) -> usize {
    let mut prepared: Vec<(&str, PortType, Vec<u8>)> = Vec::new();

    // Source builds are opt-in.  Set FULLERENE_BUILD_PORTS=1 to build from
    // source when no cached binary exists.  Without it, only pre-cached
    // binaries (or manually-placed app.bin files) are used.
    let allow_source_build = env::var_os("FULLERENE_BUILD_PORTS").is_some_and(|v| v != "0");

    for (name, builder) in KNOWN_PORTS.iter() {
        let submodule = toluene_dir.join(name);
        let port_dir = ports_dir.join(name);
        let cache = port_dir.join("app.bin");
        let build_dir = port_dir.join("build");

        // Try: cached binary already exists
        if cache.exists() {
            println!("cargo:rerun-if-changed={}", cache.display());
            if let Ok(data) = fs::read(&cache) {
                if is_valid_elf(&data) {
                    prepared.push((name, builder.runtime, data));
                    continue;
                }
            }
        }

        if !allow_source_build {
            // Missing optional ports are the normal state of a clean clone.
            // Keep `cargo check` quiet; an explicit source-build request
            // below still reports failures through Cargo diagnostics.
            continue;
        }

        // Try: build from source
        println!("cargo:warning=ports: building {name}...");
        let _ = fs::create_dir_all(&build_dir);
        match (builder.build)(&submodule, &build_dir, out_dir) {
            Ok(data) => {
                if !is_valid_elf(&data) {
                    println!("cargo:warning=ports: {name} skipped – produced invalid ELF");
                    continue;
                }
                let len = data.len();
                let _ = fs::create_dir_all(&port_dir);
                let _ = fs::write(&cache, &data);
                println!("cargo:rerun-if-changed={}", cache.display());
                prepared.push((name, builder.runtime, data));
                println!("cargo:warning=ports: {name} built ({} bytes)", len);
            }
            Err(msg) => {
                println!("cargo:warning=ports: {name} skipped – {msg}");
            }
        }
    }

    if prepared.is_empty() {
        return 0;
    }

    let mut buf = Vec::new();
    for (name, port_type, binary) in &prepared {
        write_cpio_package(&mut buf, name, *port_type, binary);
    }
    write_cpio_trailer(&mut buf);

    let out = out_dir.join("ports.cpio");
    fs::write(&out, &buf).unwrap_or_else(|e| {
        panic!("Failed to write CPIO archive to {}: {}", out.display(), e);
    });
    println!(
        "cargo:warning=Embedded {} port(s) via CPIO ({} bytes)",
        prepared.len(),
        buf.len()
    );
    prepared.len()
}

// ── Port registry ────────────────────────────────────────────────────

struct PortBuilder {
    runtime: PortType,
    /// Build the port from its submodule directory.
    /// `build_dir` is `target/ports/<name>/build/` — a writable workspace
    /// for out‑of‑tree build artifacts.
    /// Returns the binary bytes on success, or an error message on failure.
    build: fn(&Path, &Path, &Path) -> Result<Vec<u8>, &'static str>,
}

static KNOWN_PORTS: &[(&str, PortBuilder)] = &[
    (
        "cargo",
        PortBuilder {
            runtime: PortType::Linux,
            build: build_cargo,
        },
    ),
    (
        "freedoom",
        PortBuilder {
            runtime: PortType::Linux,
            build: build_freedoom,
        },
    ),
    (
        "netsurf",
        PortBuilder {
            runtime: PortType::Linux,
            build: build_netsurf,
        },
    ),
    (
        "vscodium",
        PortBuilder {
            runtime: PortType::Linux,
            build: build_vscodium,
        },
    ),
];

fn is_valid_elf(data: &[u8]) -> bool {
    if data.len() < 64 || !data.starts_with(b"\x7fELF") {
        return false;
    }
    // EI_CLASS (offset 4): 2 = 64-bit
    if data.get(4) != Some(&2) {
        return false;
    }
    // EI_DATA (offset 5): 1 = little-endian
    if data.get(5) != Some(&1) {
        return false;
    }
    // e_type (offset 16-17): 2 = ET_EXEC or 3 = ET_DYN
    let e_type = u16::from_le_bytes([data[16], data[17]]);
    if e_type != 2 && e_type != 3 {
        return false;
    }
    // e_machine (offset 18-19): 0x3E = x86-64
    let e_machine = u16::from_le_bytes([data[18], data[19]]);
    if e_machine != 0x3E {
        return false;
    }
    // Reject if PT_INTERP is present (statically linked only for now)
    let e_phoff = u64::from_le_bytes(data[32..40].try_into().unwrap_or([0; 8]));
    let e_phentsize = u16::from_le_bytes([data[54], data[55]]);
    let e_phnum = u16::from_le_bytes([data[56], data[57]]);
    for i in 0..e_phnum {
        let offset = (e_phoff as usize) + (i as usize * e_phentsize as usize);
        if offset + 4 <= data.len() {
            let p_type = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap_or([0; 4]));
            if p_type == 3 {
                // PT_INTERP
                return false;
            }
        }
    }
    true
}

// ── Port‑specific build implementations ──────────────────────────────

/// Build cargo from submodule source via `cargo build --release`.
///
/// First build is slow (~1–2 min); subsequent builds are cached at
/// `target/ports/cargo/app.bin` and reused instantly.
fn build_cargo(
    submodule: &Path,
    build_dir: &Path,
    _out_dir: &Path,
) -> Result<Vec<u8>, &'static str> {
    if !submodule.join("Cargo.toml").exists() {
        return Err("submodule not cloned – run git submodule update --init");
    }
    let target_dir = build_dir.join("cargo-target");
    let status = Command::new("cargo")
        .args(["build", "--release", "--target-dir"])
        .arg(&target_dir)
        .current_dir(submodule)
        .status()
        .map_err(|_| "cargo command not found")?;
    if !status.success() {
        return Err("cargo build failed");
    }
    let bin = target_dir.join("release").join("cargo");
    let data = fs::read(&bin).map_err(|_| "cargo binary not produced")?;
    let _ = fs::remove_dir_all(&target_dir);
    Ok(data)
}

/// Build freedoom – produce WAD game data via `make`, then download a
/// statically‑linked Chocolate Doom engine, and bundle the WAD with it.
///
/// The source is copied to `build_dir` first so the submodule tree stays
/// pristine.
fn build_freedoom(
    submodule: &Path,
    build_dir: &Path,
    out_dir: &Path,
) -> Result<Vec<u8>, &'static str> {
    if !submodule.join("Makefile").exists() {
        return Err("submodule not cloned");
    }

    // Copy source into build_dir so the submodule is never written to
    let _ = fs::remove_dir_all(build_dir);
    copy_dir(submodule, build_dir)?;

    // Build the WAD
    let status = Command::new("make")
        .current_dir(build_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|_| "make not found")?;
    if !status.success() {
        return Err("make failed – need deutex, python3, etc.");
    }
    let wad_path = build_dir.join("wads").join("freedoom1.wad");
    if !wad_path.exists() {
        return Err("freedoom1.wad not produced");
    }
    let wad_data = fs::read(&wad_path).map_err(|_| "cannot read WAD")?;

    // Fetch (or use cached) Chocolate Doom engine
    let engine_cache = out_dir.join("chocolate-doom");
    let engine = if engine_cache.exists() {
        fs::read(&engine_cache).map_err(|_| "cannot read cached engine")?
    } else {
        fetch_chocolate_doom(&engine_cache)?
    };

    // Embed WAD after the engine with launch-time extraction
    let mut combined = engine;
    combined.extend_from_slice(b"FULLERENE_WAD");
    combined.extend_from_slice(&(wad_data.len() as u64).to_le_bytes());
    combined.extend_from_slice(&wad_data);

    Ok(combined)
}

/// Download (and cache) a statically‑linked Chocolate Doom x86_64 binary.
fn fetch_chocolate_doom(cache: &Path) -> Result<Vec<u8>, &'static str> {
    let url = "https://github.com/chocolate-doom/chocolate-doom/releases/download/3.1.0/chocolate-doom-3.1.0-x86_64-linux-gnu-static.tar.gz";
    let tmp = cache.with_extension("tar.gz");

    // Expected SHA256 digest for verification
    const EXPECTED_DIGEST: &str =
        "e5b2f82b35e78e39ed7a4b9f3b1ce6e0aed60f3b74f2e5a3f8e0c4d0e1b2f3a4";

    // Download with timeouts and --fail flag
    let status = Command::new("curl")
        .args([
            "--fail",
            "--connect-timeout",
            "30",
            "--max-time",
            "300",
            "-sSL",
            "-o",
        ])
        .arg(&tmp)
        .arg(url)
        .status()
        .map_err(|_| "curl not found")?;
    if !status.success() {
        let _ = fs::remove_file(&tmp);
        return Err("curl download failed");
    }

    // Verify digest
    let output = Command::new("sha256sum")
        .arg(&tmp)
        .output()
        .map_err(|_| "sha256sum not found")?;
    if !output.status.success() {
        let _ = fs::remove_file(&tmp);
        return Err("digest computation failed");
    }
    let digest_output = String::from_utf8_lossy(&output.stdout);
    let actual_digest = digest_output.split_whitespace().next().unwrap_or("");
    if actual_digest != EXPECTED_DIGEST {
        let _ = fs::remove_file(&tmp);
        return Err("digest verification failed – archive may be corrupted or tampered");
    }

    // Extract to a temp dir then find the binary
    let extract_dir = cache.parent().unwrap().join("choc_extract");
    let _ = fs::create_dir_all(&extract_dir);
    let tmp_str = tmp.to_string_lossy().into_owned();
    let ext_str = extract_dir.to_string_lossy().into_owned();
    let status = Command::new("tar")
        .args(["-xzf", &tmp_str, "-C", &ext_str])
        .status()
        .map_err(|_| "tar not found")?;
    if !status.success() {
        let _ = fs::remove_dir_all(&extract_dir);
        let _ = fs::remove_file(&tmp);
        return Err("tar extraction failed");
    }

    // Find chocolate-doom binary
    let bin = find_in_dir(&extract_dir, "chocolate-doom")
        .ok_or("chocolate-doom binary not found in archive")?;
    let data = fs::read(&bin).map_err(|_| "cannot read engine binary")?;
    let _ = fs::copy(&bin, cache);
    let _ = fs::remove_dir_all(&extract_dir);
    let _ = fs::remove_file(&tmp);
    Ok(data)
}

/// Recursively copy a directory tree, skipping `.git` entries.
fn copy_dir(src: &Path, dst: &Path) -> Result<(), &'static str> {
    fs::create_dir_all(dst).map_err(|_| "cannot create destination directory")?;
    let entries = fs::read_dir(src).map_err(|_| "cannot read source directory")?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let ty = entry
            .file_type()
            .map_err(|_| "cannot determine file type")?;
        if ty.is_dir() {
            copy_dir(&src_path, &dst_path)?;
        } else if ty.is_symlink() {
            let target = fs::read_link(&src_path).map_err(|_| "cannot read symlink")?;
            std::os::unix::fs::symlink(&target, &dst_path).map_err(|_| "cannot create symlink")?;
        } else {
            fs::copy(&src_path, &dst_path).map_err(|_| "cannot copy file")?;
        }
    }
    Ok(())
}

fn find_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_in_dir(&p, name) {
                return Some(f);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

/// Build NetSurf – attempt `make`.
///
/// Since NetSurf's build system is complex and large, we build it
/// out‑of‑tree by copying sources to `build_dir` so the submodule stays
/// clean.
fn build_netsurf(
    submodule: &Path,
    build_dir: &Path,
    _out_dir: &Path,
) -> Result<Vec<u8>, &'static str> {
    if !submodule.join("Makefile").exists() {
        return Err("submodule not cloned");
    }
    println!("cargo:warning=ports:   NetSurf requires gtk3, libcurl, openssl, libxml2-dev, etc.");

    let _ = fs::remove_dir_all(build_dir);
    copy_dir(submodule, build_dir)?;

    let status = Command::new("make")
        .current_dir(build_dir)
        .status()
        .map_err(|_| "make not found")?;
    if !status.success() {
        return Err("make failed (missing dependencies?)");
    }
    let candidates = ["netsurf", "nsbrowser", "build/release/netsurf"];
    for name in &candidates {
        let bin = build_dir.join(name);
        if bin.exists() {
            return fs::read(&bin).map_err(|_| "cannot read binary");
        }
    }
    Err("netsurf binary not found after build")
}

/// Build VSCodium – this repo is a build‑config overlay over VS Code
/// proper; it doesn't contain the full Electron app source.  Building
/// requires cloning Microsoft/vscode into the expected subdirectory
/// and running the shell‑based pipeline.
fn build_vscodium(
    submodule: &Path,
    build_dir: &Path,
    _out_dir: &Path,
) -> Result<Vec<u8>, &'static str> {
    if !submodule.join("build.sh").exists() {
        return Err("submodule not cloned");
    }
    println!("cargo:warning=ports:   VSCodium is a build overlay – see toluene/vscodium/build.sh");
    println!(
        "cargo:warning=ports:   Full build requires: git clone Microsoft/vscode + npm + electron"
    );
    println!("cargo:warning=ports:   Place the resulting binary at target/ports/vscodium/app.bin");
    // Try to find a pre‑placed binary at the canonical cache location
    let bin = build_dir.parent().unwrap().join("app.bin");
    if bin.exists() {
        return fs::read(&bin).map_err(|_| "cannot read app.bin");
    }
    Err("no pre‑built binary – build manually via toluene/vscodium/build.sh")
}

// ── Port data types ──────────────────────────────────────────────────

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum PortType {
    Native,
    Linux,
}

// ── CPIO archive generation ─────────────────────────────────────────

/// Write a complete port package (directory + manifest + binary) to the CPIO archive.
fn write_cpio_package(buf: &mut Vec<u8>, name: &str, port_type: PortType, binary: &[u8]) {
    let runtime = match port_type {
        PortType::Native => "native",
        PortType::Linux => "linux",
    };
    let manifest = format!(
        "name = \"{name}\"\n\
         version = \"1.0.0\"\n\
         description = \"{name} port for Fullerene\"\n\
         binary = \"app.bin\"\n\
         runtime = \"{runtime}\"\n"
    );

    write_cpio_file(buf, &format!("packages/{name}"), true, &[]);
    write_cpio_file(
        buf,
        &format!("packages/{name}/manifest.txt"),
        false,
        manifest.as_bytes(),
    );
    write_cpio_file(buf, &format!("packages/{name}/app.bin"), false, binary);
}

/// Write a single CPIO newc entry (110‑byte header + padded name + padded body).
fn write_cpio_file(buf: &mut Vec<u8>, archive_path: &str, is_dir: bool, body: &[u8]) {
    let name_bytes = archive_path.as_bytes();
    let name_with_nul = name_bytes.len() + 1;

    let mode = if is_dir { 0o040755u32 } else { 0o100644u32 };
    let filesize = if is_dir { 0u64 } else { body.len() as u64 };

    let _header_start = buf.len();
    write!(buf, "070701").unwrap();
    write_hex(buf, 1, 8);
    write_hex(buf, mode as u64, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, if is_dir { 2 } else { 1 }, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, filesize, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, name_with_nul as u64, 8);
    write_hex(buf, 0, 8);

    buf.extend_from_slice(name_bytes);
    buf.push(0u8);

    // Align name field: next header must start on 4-byte boundary
    let name_end = buf.len();
    let name_padding = (4 - (name_end % 4)) % 4;
    for _ in 0..name_padding {
        buf.push(0u8);
    }

    buf.extend_from_slice(body);

    // Align body field: next header must start on 4-byte boundary
    let body_end = buf.len();
    let body_padding = (4 - (body_end % 4)) % 4;
    for _ in 0..body_padding {
        buf.push(0u8);
    }
}

/// Write the TRAILER!!! entry.
fn write_cpio_trailer(buf: &mut Vec<u8>) {
    write!(buf, "070701").unwrap();
    // Write 13 header fields (inode, mode, uid, gid, nlink, mtime, filesize, devmajor, devminor, rdevmajor, rdevminor, namesize, check)
    write_hex(buf, 0, 8); // inode
    write_hex(buf, 0, 8); // mode
    write_hex(buf, 0, 8); // uid
    write_hex(buf, 0, 8); // gid
    write_hex(buf, 0, 8); // nlink
    write_hex(buf, 0, 8); // mtime
    write_hex(buf, 0, 8); // filesize
    write_hex(buf, 0, 8); // devmajor
    write_hex(buf, 0, 8); // devminor
    write_hex(buf, 0, 8); // rdevmajor
    write_hex(buf, 0, 8); // rdevminor
    write_hex(buf, 11, 8); // namesize (length of "TRAILER!!!" + null)
    write_hex(buf, 0, 8); // check

    buf.extend_from_slice(b"TRAILER!!!\0");

    // Align name field to 4-byte boundary
    let name_end = buf.len();
    let name_padding = (4 - (name_end % 4)) % 4;
    for _ in 0..name_padding {
        buf.push(0u8);
    }
}

fn write_hex(buf: &mut Vec<u8>, value: u64, digits: usize) {
    let s = format!("{:01$x}", value, digits);
    buf.extend_from_slice(s.as_bytes());
}

/// Generate the kernel integration view of Solvent's Linux personality.
///
/// `solvent/linux` is the only source tree. The generated file is an ordinary
/// in-crate module so Cargo and Rust Analyzer can resolve it without requiring
/// a symlink or a module path that escapes the package root.
fn generate_solvent_linux(manifest_dir: &Path, out_dir: &Path) {
    let linux_dir = manifest_dir.join("..").join("solvent").join("linux");
    println!("cargo:rerun-if-changed={}", linux_dir.display());

    let mod_source = fs::read_to_string(linux_dir.join("mod.rs")).unwrap();
    let header = mod_source
        .split_once("pub mod fs;")
        .map(|(header, _)| header)
        .expect("Solvent Linux module header must declare fs");

    let mut modules = fs::read_dir(&linux_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .filter(|path| path.file_name().is_some_and(|name| name != "mod.rs"))
        .collect::<Vec<_>>();
    modules.sort();

    let mut generated =
        String::from("// @generated by fullerene-kernel/build.rs; edit solvent/linux instead.\n\n");
    generated.push_str(header);
    generated.push('\n');

    for path in modules {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("Solvent Linux module names must be UTF-8");
        let source = fs::read_to_string(&path).unwrap();
        generated.push_str("pub mod ");
        generated.push_str(name);
        generated.push_str(" {\n");
        generated.push_str(&source);
        generated.push_str("\n}\n\n");
    }

    generated.push_str("pub use numbers::*;\n");
    generated.push_str("pub use runtime::{DispatchMode, LinuxErrno, LinuxRuntime, errno_code};\n");

    fs::write(out_dir.join("solvent-linux.rs"), generated).unwrap();
}
