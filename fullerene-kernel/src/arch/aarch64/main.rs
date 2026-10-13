extern crate alloc;

use fullerene_abi::boot::{self, BootArchitecture, BootInfo, BootPlatform};

mod allocator;
mod cpu;
#[cfg(fullerene_aarch64_ane)]
pub(crate) mod desktop;
mod devices;
#[cfg(fullerene_aarch64_bramble)]
mod display;
mod elf;
mod entry;
mod exceptions;
mod fdt;
mod fs;
#[cfg(feature = "aarch64-user-launchd")]
mod launchd;
mod linux;
mod mmu;
#[path = "../../platform/mod.rs"]
pub(crate) mod platform;
mod syscall;
mod task;
pub(crate) mod timer;
mod uart;
mod ufs;

/// Pump the ANE fastboot responder from the syscall boundary.
///
/// `launchd::run()` enters EL0 and never returns to the wait loop, so from
/// that point nothing in the kernel drives `ane_usb_dev::service` and every
/// bench read times out while the USB link stays enumerated. The user task
/// crosses the SVC boundary constantly, so the responder gets pumped from
/// here instead (the log hook is a no-op: during user-task execution the
/// ring must carry boot markers, not USB chatter).
#[cfg(fullerene_aarch64_ane)]
fn ane_service_pump() {
    platform::ane_usb_dev::service(|_label: &str, _value: u64| {});
}
// ── Why the USB test/simulation modules are *siblings* of `usb`, not inside it
//
// `usb_protocol`, `usb_regs`, `usb_dwc3_sim`, `usb_qemu_sim` and
// `usb_linux_host_enum` reference each other through `super::` (for example
// `usb_qemu_sim.rs` does `use super::{uart, usb_dwc3_sim::…, usb_protocol::…,
// usb_regs::…}`). An `#[path]`-included module resolves `super::` against the
// crate that included it, and these same files are compiled by *two* different
// roots: this one (`arch::aarch64`) and the host test crate (`src/main.rs`,
// under `cfg(test)`). For `super::X` to name a sibling in both, they must all be
// declared at the *same module level* in each crate.
//
// Making them submodules of `usb` would break that: the host test crate would
// then need a `usb` parent module, which means compiling `usb/mod.rs` for the
// host — and that file depends on `super::platform` and `super::timer`
// (218 references), neither of which exists at the host crate root.
//
// So they stay siblings; only their *file* location differs (they live under
// `usb/` with explicit `#[path]`). Keep the names and the module level intact
// when reorganising. `usb`'s own submodules (trace, config, phy, …) are a
// different case: Rust requires a module's children to live in the directory
// named after it, which is why `usb/` exists at all.
#[cfg(fullerene_aarch64_bramble)]
mod usb;
#[cfg(fullerene_aarch64_qemu_usb_sim)]
#[path = "usb/usb_dwc3_sim.rs"]
mod usb_dwc3_sim;
#[cfg(any(fullerene_aarch64_bramble, fullerene_aarch64_qemu_usb_sim))]
#[path = "usb/usb_protocol.rs"]
mod usb_protocol;
#[cfg(fullerene_aarch64_qemu_usb_sim)]
#[path = "usb/usb_qemu_sim.rs"]
mod usb_qemu_sim;
#[cfg(any(fullerene_aarch64_bramble, fullerene_aarch64_qemu_usb_sim))]
#[path = "usb/usb_regs.rs"]
mod usb_regs;
mod user_memory;
#[cfg(any(feature = "aarch64-user-smoke", feature = "aarch64-user-fault-smoke"))]
mod user_smoke;
mod window;

const MAX_MEMORY_REGIONS: usize = 8;

// The arm64 Linux boot contract supplies a DTB rather than an EFI memory-map
// table. Keep the bounded DT-derived map in the image's identity-mapped BSS
// so the next architecture layer can consume the same pointer-free BootInfo
// contract as the x86_64 loader path.
static mut AARCH64_MEMORY_REGIONS: [fdt::Region; MAX_MEMORY_REGIONS] =
    [fdt::Region { base: 0, size: 0 }; MAX_MEMORY_REGIONS];

fn make_boot_info(platform: BootPlatform, dtb_address: Option<u64>) -> BootInfo {
    let mut info = BootInfo::new(BootArchitecture::Aarch64, platform);
    if let Some(address) = dtb_address {
        info.fdt_address = address;
        if fdt::inspect(address).is_some() {
            info.flags |= boot::flags::FDT;

            // Publish the DT-derived memory map in a stable, identity-mapped
            // buffer. This is deliberately done before the generic runtime
            // exists; a future AArch64 frame allocator will replace the
            // fixed early MMU map but keep this handoff shape.
            let count = unsafe {
                fdt::find_memory_regions(
                    address,
                    core::slice::from_raw_parts_mut(
                        core::ptr::addr_of_mut!(AARCH64_MEMORY_REGIONS).cast::<fdt::Region>(),
                        MAX_MEMORY_REGIONS,
                    ),
                )
            };
            if count != 0 {
                info.flags |= boot::flags::MEMORY_MAP;
                info.memory_map_address = core::ptr::addr_of!(AARCH64_MEMORY_REGIONS) as u64;
                info.memory_map_size = (count * core::mem::size_of::<fdt::Region>()) as u64;
                info.memory_map_descriptor_size = core::mem::size_of::<fdt::Region>() as u64;
            }

            // Claim the black box region for this boot.
            //
            // Base and length come from the tree the bootloader actually handed
            // us, matched by node name, and are never assumed: the box writes
            // across a reboot, and this device's `/reserved-memory` also holds
            // modem, PIL and CDSP windows, so being handed the wrong range would
            // be a silent corruption of memory another component owns. If the
            // name is absent the box simply stays disabled - its readout word
            // reports that, rather than reporting a zero that would read as
            // "nothing was recorded".
            let mut reserved = [fdt::Region { base: 0, size: 0 }; 8];
            // The ring's home is `ramoops_meta_region`, and it is deliberately a
            // *within-boot* instrument: this region does not keep its contents across a
            // reboot. The cross-boot copy of the sequence is the durable text log in the
            // ramoops pmsg zone, which is where the measurement said persistence actually
            // is.
            let found = unsafe {
                fdt::find_reserved_region_named(
                    address,
                    b"ramoops_meta_region",
                    core::slice::from_raw_parts_mut(reserved.as_mut_ptr(), reserved.len()),
                )
            };
            for region in reserved[..found.min(reserved.len())].iter() {
                // The ring needs the header plus at least one record; asking for 4 KiB beyond
                // the offset was wrong and rejected the very region it is pinned to (the meta
                // region is exactly 0x1000, so `0x100 + 4096` never passed). The requirement
                // lives in the module so the two cannot drift apart again.
                if region.base == 0 || region.size < crate::blackbox::RING_MIN_BYTES {
                    continue;
                }
                if crate::blackbox::init(region.base, region.size) {
                    // arg0 carries the boot number this record belongs to, so a
                    // *later* boot can read the ring and tell "this one is mine"
                    // from "this one is the previous boot's". Without that, a
                    // record that survived and a record written twice inside one
                    // boot look identical - which is exactly the confusion the
                    // first device run produced.
                    crate::blackbox::record(
                        crate::blackbox::CODE_BOOT,
                        0,
                        crate::blackbox::boots() as u64,
                        region.base,
                    );
                    crate::blackbox::record(
                        crate::blackbox::CODE_DT_RESERVED_REGION,
                        0,
                        region.size,
                        found as u64,
                    );
                    // Only stop once a region has actually been claimed. The lookup is a prefix
                    // match on the node name, so it returns every candidate and `init` rejects
                    // the ones whose address is not the one this build was told about.
                    break;
                }
            }
        }

        // Arm the Android-readable mirror.
        //
        // Same discipline as the ring: the names are looked up in the tree the bootloader
        // handed us and the base each resolves to is checked against the address this
        // device's memory DTBO documents, so a renamed or moved region disables the mirror
        // instead of writing into memory another component owns. Note that
        // `alt_ramoops_region` contains `ramoops_region` as a substring, so both lookups can
        // return the same region; the address check is what keeps them apart.
        //
        // This is the route that makes the box readable. The ramoops console zone is memory
        // Android's own pstore driver reads after the next boot and presents as a file, and
        // `adb` can read that file. Getting our records out through someone else's reader is
        // what "readable" turned out to mean on this device, since our own ring does not
        // survive the reboot.
        let mut ramoops = [fdt::Region { base: 0, size: 0 }; 4];
        // The reserved-memory node carrying the ramoops window is named per
        // platform: Bramble's trees call it `ramoops_region`, while the
        // ANE-LX2J tree calls it `pstore-mem` and points at it from its
        // `/ramoops` node by phandle. The address check inside `init_pstore`
        // is what still decides whether the region found is the one this
        // build was told about.
        let primary_name: &[u8] = if cfg!(fullerene_aarch64_ane) {
            b"pstore-mem"
        } else {
            b"ramoops_region"
        };
        let primary_found = unsafe {
            fdt::find_reserved_region_named(
                address,
                primary_name,
                core::slice::from_raw_parts_mut(ramoops.as_mut_ptr(), ramoops.len()),
            )
        };
        for region in ramoops[..primary_found.min(ramoops.len())].iter() {
            crate::blackbox::init_pstore(region.base, region.size);
        }
        let mut alternate = [fdt::Region { base: 0, size: 0 }; 4];
        let alt_found = unsafe {
            fdt::find_reserved_region_named(
                address,
                b"alt_ramoops_region",
                core::slice::from_raw_parts_mut(alternate.as_mut_ptr(), alternate.len()),
            )
        };
        for region in alternate[..alt_found.min(alternate.len())].iter() {
            crate::blackbox::init_pstore_alt(region.base, region.size);
        }
        crate::blackbox::publish_console();
    }
    info
}

#[unsafe(no_mangle)]
extern "C" fn aarch64_rust_entry(boot_context: *const entry::Aarch64BootContext) -> ! {
    // Mark 6: the Rust entry is alive. The bootstrap's own marks stop at 5
    // (EL1 configured); from here the marks follow this function's stages to
    // the early-boot loop at 'c'.
    // The context lives at the top of the bootstrap stack. Copy it before
    // Rust starts using that stack for ordinary locals and call frames.
    let boot = entry::Aarch64BootContext::read(boot_context);
    let fdt_address = boot.x0 as u64;
    let arg1 = boot.x1 as u64;
    let fdt_arg2 = boot.x2 as u64;
    let arg3 = boot.x3 as u64;

    // First durable mark, ahead of every device access this boot performs.
    //
    // The ANE-LX2J has no console an outside observer can read, so the only
    // channel a failed bring-up leaves behind is reserved RAM (its device tree
    // carries a ramoops window; see `crate::blackbox`). Writing the mark before
    // the console's clock gate and UART - the first register writes of the
    // whole boot - is what lets a later boot tell "the loader entered us and
    // the early bring-up died" from "the loader never entered us": an entry
    // record exists, or nothing does.
    #[cfg(fullerene_aarch64_ane)]
    crate::blackbox::mark_entry();

    // Carry the previous boot's bare-metal l1d USB trace into the black box.
    //
    // The l1d probe leaves 24 [step, value] entries at a fixed address in the
    // same reserved window these marks live in. Its boot ends in a loader
    // park, so nothing on the USB side can report what happened - this copy,
    // taken before the console path can die, is what makes the failure point
    // readable at all. Two entries per record: arg0/arg1 hold
    // (step << 32 | value) each, 12 records for the 24 entries. Runs after
    // mark_entry() so both the record ring and the text zone are armed, and
    // before the clock gate/UART so it is on the surviving side of any
    // console-path death.
    #[cfg(fullerene_aarch64_ane)]
    {
        let trace = entry::StepMarks::L1D_TRACE_ADDRESS as *const u32;
        for i in 0..12u32 {
            let base = (i * 4) as usize;
            let s0 = unsafe { core::ptr::read_volatile(trace.add(base)) } as u64;
            let v0 = unsafe { core::ptr::read_volatile(trace.add(base + 1)) } as u64;
            let s1 = unsafe { core::ptr::read_volatile(trace.add(base + 2)) } as u64;
            let v1 = unsafe { core::ptr::read_volatile(trace.add(base + 3)) } as u64;
            crate::blackbox::record(
                crate::blackbox::CODE_L1D_TRACE,
                i as u64,
                (s0 << 32) | v0,
                (s1 << 32) | v1,
            );
        }
    }

    // Tell the loader's BFM this boot reached the OS.
    //
    // The stock kernel writes STAGE_BOOTUP_END (250) to the PMIC bootup
    // keypoint register from hisi_bootup_keypoint.c -
    // BOOTUP_KEYPOINT_OFFSET = PMIC_HRST_REG1_ADDR(0) = PMIC reg 0x18C,
    // byte 0xFFF34630 - as the OS half of the loader's boot chain-of-custody
    // protocol. The loader reads it at its next start: any value but 250
    // counts as an unfinished boot, and three of those escalate to the
    // eRecovery rescue image. Fullerene never wrote it, so every boot looked
    // unfinished to the BFM - the eRecovery landings of v100/v102/v105c came
    // from there, not from any single crash. The PMIC cell is alive from
    // power-on (no clock gate needed), so the write goes first, before the
    // console: a crash later in this boot still leaves the loader a clean
    // marker, and only a boot that dies before this line can count against us.
    #[cfg(fullerene_aarch64_ane)]
    {
        platform::common::pmic::pmic_wr(0x18C, 250);
        let rb = platform::common::pmic::pmic_rd(0x18C) as u64;
        crate::blackbox::record(crate::blackbox::CODE_BOOT_KEYPOINT, 0, rb, 0);
        // v174: feed the inherited loader watchdog before anything slow
        // runs. The timer-IRQ feed only exists once exceptions::install()
        // has run and the timer actually ticks; some boots demonstrably
        // died 11-46 s after enumeration with no tick evidence, and the
        // SP805 (programmed by the loader, counting down through our
        // handoff) is the one always-running suspect. This early kick
        // covers the whole pre-timer boot phase.
        platform::ane::watchdog_tend();
    }

    // Establish a compiled-in console before looking at any bootloader
    // pointer. A vendor trampoline can hand us an invalid or absent DTB;
    // touching that address before VBAR and UART are ready turns a useful
    // handoff failure into an invisible synchronous abort.
    let compiled_bramble = cfg!(fullerene_aarch64_bramble);
    let compiled_ane = cfg!(fullerene_aarch64_ane);
    if compiled_ane {
        // The console's clock gate first: a loader that ran a console has it
        // open already, but nothing guarantees that across a handoff, and a
        // closed gate makes the very next step (the first PL011 write) the
        // last thing this boot does. See platform::ane::open_uart_clock_gate.
        platform::ane::open_uart_clock_gate();
    }
    let early_uart_base = if compiled_bramble {
        platform::bramble::UART_BASE
    } else if compiled_ane {
        platform::ane::UART_BASE
    } else {
        platform::qemu_virt::UART_BASE
    };
    if compiled_bramble {
        uart::init_qcom_geni(early_uart_base as u64);
    } else {
        uart::init_at(early_uart_base as u64);
    }
    exceptions::install();
    // Mark 7: the compiled-in console and the exception vectors are up - the
    // first device accesses (the console's clock gate, then the UART register
    // write) and the fault path all survived. A run whose marks stop at 6 died
    // in exactly that step.
    #[cfg(fullerene_aarch64_bramble)]
    {
        // XBL/ABL's secure watchdog can expire while the normal path is
        // still walking the large merged DTB. Take the same ownership
        // boundary as the standalone USB probe before any DT discovery or
        // resource scan, so a later USB A/B measures the handoff itself.
        let secure_wdt_disabled = if option_env!("FULLERENE_AARCH64_ENTRY_SECURE_WDT") == Some("1")
        {
            usb::secure_wdt_disable()
        } else {
            false
        };
        usb::wdt_pet();
        uart::puts(if secure_wdt_disabled {
            "platform: secure watchdog disabled at Rust entry\n"
        } else if option_env!("FULLERENE_AARCH64_ENTRY_SECURE_WDT") == Some("1") {
            "platform: secure watchdog entry disable unavailable; APSS watchdog extended\n"
        } else {
            "platform: secure watchdog entry SMC skipped; APSS watchdog extended\n"
        });
    }
    uart::puts("fullerene: entered Rust before DTB discovery\n");
    uart::put_hex("boot: x0=", fdt_address);
    uart::put_hex("boot: x1=", arg1);
    uart::put_hex("boot: x2=", fdt_arg2);
    uart::put_hex("boot: x3=", arg3);
    uart::put_hex("boot: currentel=", boot.current_el as u64);
    uart::put_hex("boot: relocation_delta=", boot.relocation_delta as u64);

    // Keep one stricter ordering A/B for a vendor handoff that may expire
    // while the large merged DTB is being walked.  This mode intentionally
    // uses the compiled Bramble USB contract: DT-derived overrides are not
    // available until the scan below completes, so the result isolates the
    // ordering boundary from DT resource selection.
    let mut early_usb = false;
    let mut usb_ready = true;
    #[cfg(fullerene_aarch64_bramble)]
    if option_env!("FULLERENE_AARCH64_USB_EARLY_BEFORE_DTB_SCAN") == Some("1") {
        early_usb = true;
        uart::puts("platform: bramble USB handoff before DTB scan: begin\n");
        usb_ready = init_bramble_usb_handoff();
        usb::set_early_handoff_active(usb_ready);
        uart::puts(if usb_ready {
            "platform: bramble USB handoff before DTB scan: ready\n"
        } else {
            "platform: bramble USB handoff before DTB scan: failed\n"
        });
    }

    // The architectural arm64 boot contract puts the physical DTB address in
    // x0 and requires x1..x3 to be zero.  A vendor fastboot path is allowed
    // to use a different trampoline, however, so accept x2 as a guarded
    // fallback for bring-up.  Never prefer x2 over a valid x0: otherwise a
    // normal Android handoff can be mistaken for a missing DTB.
    // Use a valid boot-provided DTB on both QEMU and Bramble.  The compiled
    // Lito contract remains the fallback for a vendor fastboot trampoline
    // that does not pass x0/x2, but a supplied DTB must be allowed to override
    // physical resources before the USB driver touches them.
    let dtb_address = [fdt_address, fdt_arg2]
        .into_iter()
        .filter(|address| *address != 0 && *address % 8 == 0)
        .find(|address| fdt::inspect(*address).is_some())
        // A compiled-in fallback exists only for QEMU virt, which places the
        // DTB at a fixed address on its direct `-kernel` path. Bramble and the
        // ANE-LX2J LK both supply theirs in x0, and on those a fallback would
        // point into unrelated peripheral space rather than RAM.
        .or_else(|| (!compiled_bramble && !compiled_ane).then_some(platform::qemu_virt::DTB_BASE));
    let qcom_uart = dtb_address.and_then(|address| {
        fdt::find_compatible(address, b"qcom,geni-debug-uart")
            .or_else(|| fdt::find_compatible(address, b"qcom,geni-uart"))
    });
    let pl011_uart = dtb_address.and_then(|address| fdt::find_compatible(address, b"arm,pl011"));
    let gicd_region = dtb_address.and_then(|address| fdt::find_compatible(address, b"arm,gic-v3"));
    let gicr_region =
        dtb_address.and_then(|address| fdt::find_compatible_nth(address, b"arm,gic-v3", 1));
    let bramble = cfg!(fullerene_aarch64_bramble) || qcom_uart.is_some();
    if bramble {
        if let Some(address) = dtb_address {
            // Prefer the nested DWC3 core node: the parent Qualcomm glue
            // advertises a 1 MiB wrapper window while the core's DT resource
            // is the 0xcd00-byte register block consumed by this driver.
            let dwc3 = fdt::find_compatible(address, b"snps,dwc3")
                .or_else(|| fdt::find_compatible(address, b"qcom,dwc-usb3-msm"));
            let hs_phy = fdt::find_compatible(address, b"qcom,usb-hsphy-snps-femto");
            let hs_phy_eud = fdt::find_compatible_nth(address, b"qcom,usb-hsphy-snps-femto", 1);
            let qmp_phy = fdt::find_compatible(address, b"qcom,usb-ssphy-qmp-dp-combo");
            let gcc = fdt::find_compatible(address, b"qcom,gcc-lito");
            let pdc = fdt::find_compatible(address, b"qcom,lito-pdc");
            let usb_node = b"qcom,dwc-usb3-msm";
            let mut contract = platform::bramble::UsbDtContract::empty();
            contract.gdsc =
                fdt::find_phandle_property_region(address, usb_node, b"USB3_GDSC-supply")
                    .map(|region| (region.base, region.size));
            contract.vbus_reg_base = fdt::find_compatible(address, b"qcom,pm8150b-vbus-reg")
                .and_then(|region| (region.base <= u32::MAX as u64).then_some(region.base as u32));
            contract.dma_pool = match (
                fdt::find_compatible_property_u32(
                    address,
                    usb_node,
                    b"qcom,iommu-dma-addr-pool",
                    0,
                ),
                fdt::find_compatible_property_u32(
                    address,
                    usb_node,
                    b"qcom,iommu-dma-addr-pool",
                    1,
                ),
            ) {
                (Some(base), Some(size)) => Some((base as u64, size as u64)),
                _ => None,
            };
            // `iommus = <&apps_smmu SID 0>`: cell 0 is the phandle, cell 1
            // is the stream ID consumed by the SMMU context-bank setup.
            let usb_smmu_phandle =
                fdt::find_compatible_property_u32(address, usb_node, b"iommus", 0);
            contract.stream_id = fdt::find_compatible_property_u32(address, usb_node, b"iommus", 1);
            // Resolve the SMMU through the consumer's phandle. The Lito DT
            // has both KGSL and Apps-SMMU nodes with the same compatible;
            // relying on their source-order would make a valid DT overlay
            // select the wrong SMMU and its capabilities.
            let apps_smmu = usb_smmu_phandle
                .and_then(|phandle| fdt::find_phandle_region(address, phandle))
                .or_else(|| fdt::find_compatible_nth(address, b"qcom,qsmmu-v500", 1))
                .or_else(|| fdt::find_compatible(address, b"qcom,qsmmu-v500"));
            contract.smmu_use_3_level_tables = usb_smmu_phandle
                .and_then(|phandle| {
                    fdt::find_phandle_property_u32(address, phandle, b"qcom,use-3-lvl-tables", 0)
                })
                .map(|_| true);
            // Apps-SMMU lists one global fault SPI followed by the context
            // bank fault SPIs.  Preserve that provider ordering: the SMMU's
            // CBAR.IRPTNDX selects the dense context index, while the GIC
            // numbers themselves are intentionally sparse.
            if let Some(phandle) = usb_smmu_phandle {
                contract.smmu_global_irq =
                    fdt::find_phandle_property_u32(address, phandle, b"interrupts", 0);
                for index in 0..platform::bramble::SMMU_CONTEXT_IRQ_COUNT {
                    contract.smmu_context_irqs[index] = fdt::find_phandle_property_u32(
                        address,
                        phandle,
                        b"interrupts",
                        3 * (index + 1) + 1,
                    );
                }
            }
            // Lito's `interrupts-extended` tuples are
            // (PDC phandle, pin, trigger), (GIC phandle, type, SPI,
            // trigger), then two more PDC tuples.  The child DWC3 node has
            // the usual (type, number, trigger) `interrupts` property.
            contract.irq_numbers[0] =
                fdt::find_compatible_property_u32(address, usb_node, b"interrupts-extended", 1);
            contract.irq_numbers[1] =
                fdt::find_compatible_property_u32(address, usb_node, b"interrupts-extended", 5)
                    .and_then(fdt::gic_spi_to_intid);
            contract.irq_numbers[2] =
                fdt::find_compatible_property_u32(address, usb_node, b"interrupts-extended", 8);
            contract.irq_numbers[3] =
                fdt::find_compatible_property_u32(address, usb_node, b"interrupts-extended", 11);
            contract.irq_numbers[4] =
                fdt::find_compatible_property_u32(address, b"snps,dwc3", b"interrupts", 1)
                    .and_then(fdt::gic_spi_to_intid);
            // The PM8150B Type-C child has no compatible string in the
            // Android PMIC DT. Its first SPMI interrupt is the platform IRQ
            // consumed by qcom-pmic-typec; the SPMI arbiter exposes the
            // corresponding summary as its `periph_irq` GIC SPI.
            for index in 0..4 {
                contract.typec_irq[index] =
                    fdt::find_named_property_u32(address, b"qcom,typec@1500", b"interrupts", index);
            }
            contract.spmi_parent_irq =
                fdt::find_compatible_property_u32(address, b"qcom,spmi-pmic-arb", b"interrupts", 1)
                    .and_then(fdt::gic_spi_to_intid);
            for index in 0..18 {
                contract.qmp_reg_offsets[index] = fdt::find_compatible_property_u32(
                    address,
                    b"qcom,usb-ssphy-qmp-dp-combo",
                    b"qcom,qmp-phy-reg-offset",
                    index,
                );
            }
            // Clock specifiers are <provider-phandle provider-local-id>.
            // Keep the provider ID separate from the MMIO base: GCC's ID to
            // register map is SoC/provider specific and must be validated
            // before a DT-selected provider base is accepted.
            for (slot, cell) in contract
                .controller_clock_ids
                .iter_mut()
                .zip([1usize, 3, 5, 7, 9, 11])
            {
                *slot = fdt::find_compatible_property_u32(address, usb_node, b"clocks", cell);
            }
            for (slot, cell) in contract.qmp_clock_ids.iter_mut().zip([1usize, 3, 5, 7]) {
                *slot = fdt::find_compatible_property_u32(
                    address,
                    b"qcom,usb-ssphy-qmp-dp-combo",
                    b"clocks",
                    cell,
                );
            }
            contract.hs_phy_clock_id = fdt::find_compatible_property_u32(
                address,
                b"qcom,usb-hsphy-snps-femto",
                b"clocks",
                1,
            );
            // The reset arrays are also provider-local specifiers. The QMP
            // binding lists USB3 PHY reset first and USB3 DP PHY reset
            // second, while the active resource table exposes that order.
            contract.reset_ids[0] =
                fdt::find_compatible_property_u32(address, usb_node, b"resets", 1);
            contract.reset_ids[1] = fdt::find_compatible_property_u32(
                address,
                b"qcom,usb-hsphy-snps-femto",
                b"resets",
                1,
            );
            contract.reset_ids[2] = fdt::find_compatible_property_u32(
                address,
                b"qcom,usb-ssphy-qmp-dp-combo",
                b"resets",
                3,
            );
            contract.reset_ids[3] = fdt::find_compatible_property_u32(
                address,
                b"qcom,usb-ssphy-qmp-dp-combo",
                b"resets",
                1,
            );
            let hsphy_observation = fdt::find_compatible_node_property_observation(
                address,
                b"qcom,usb-hsphy-snps-femto",
                b"qcom,param-override-seq",
                0,
            );
            contract.hs_param_override = hsphy_observation
                .map(|item| item.cells)
                .unwrap_or([None; 6]);
            for index in 0..441 {
                contract.qmp_init_seq[index] = fdt::find_compatible_property_u32(
                    address,
                    b"qcom,usb-ssphy-qmp-dp-combo",
                    b"qcom,qmp-phy-init-seq",
                    index,
                );
            }
            for index in 0..3 {
                contract.hs_vdd_voltage_level[index] = fdt::find_compatible_property_u32(
                    address,
                    b"qcom,usb-hsphy-snps-femto",
                    b"qcom,vdd-voltage-level",
                    index,
                );
                contract.qmp_vdd_voltage_level[index] = fdt::find_compatible_property_u32(
                    address,
                    b"qcom,usb-ssphy-qmp-dp-combo",
                    b"qcom,vdd-voltage-level",
                    index,
                );
            }
            contract.qmp_vdd_max_load_ua = fdt::find_compatible_property_u32(
                address,
                b"qcom,usb-ssphy-qmp-dp-combo",
                b"qcom,vdd-max-load-uA",
                0,
            );
            for index in 0..3 {
                contract.qmp_core_voltage_level[index] = fdt::find_compatible_property_u32(
                    address,
                    b"qcom,usb-ssphy-qmp-dp-combo",
                    b"qcom,core-voltage-level",
                    index,
                );
            }
            contract.qmp_core_max_load_ua = fdt::find_compatible_property_u32(
                address,
                b"qcom,usb-ssphy-qmp-dp-combo",
                b"qcom,core-max-load-uA",
                0,
            );
            contract.qmp_vbus_valid_override = qmp_phy.map(|_| {
                fdt::find_compatible_property_u32(
                    address,
                    b"qcom,usb-ssphy-qmp-dp-combo",
                    b"qcom,vbus-valid-override",
                    0,
                )
                .is_some()
            });
            // The PHY nodes carry supply phandles, not RPMh resource IDs.
            // Resolve each phandle back to its regulator-name and only then
            // map the Android PMIC name to a Command DB resource.  A missing
            // or unknown supply remains unset and cannot cause a guessed
            // PMIC vote.
            let supplies = [
                (
                    b"qcom,usb-hsphy-snps-femto".as_slice(),
                    b"vdd-supply".as_slice(),
                ),
                (
                    b"qcom,usb-hsphy-snps-femto".as_slice(),
                    b"vdda18-supply".as_slice(),
                ),
                (
                    b"qcom,usb-hsphy-snps-femto".as_slice(),
                    b"vdda33-supply".as_slice(),
                ),
                (
                    b"qcom,usb-ssphy-qmp-dp-combo".as_slice(),
                    b"vdd-supply".as_slice(),
                ),
                (
                    b"qcom,usb-ssphy-qmp-dp-combo".as_slice(),
                    b"core-supply".as_slice(),
                ),
            ];
            for (slot, (node, property)) in contract.supply_resource_ids.iter_mut().zip(supplies) {
                if let Some(name) =
                    fdt::find_phandle_property_string(address, node, property, 0, b"regulator-name")
                {
                    *slot = platform::bramble::rpmh_resource_id_from_regulator_name(
                        &name.bytes,
                        name.len,
                    );
                }
            }
            contract.core_clk_rate_hz =
                fdt::find_compatible_property_u32(address, usb_node, b"qcom,core-clk-rate", 0);
            contract.core_clk_rate_hs_hz =
                fdt::find_compatible_property_u32(address, usb_node, b"qcom,core-clk-rate-hs", 0);
            contract.gsi_event_buffer_count =
                fdt::find_compatible_property_u32(address, usb_node, b"qcom,num-gsi-evt-buffs", 0);
            for index in 0..6 {
                contract.gsi_reg_offsets[index] = fdt::find_compatible_property_u32(
                    address,
                    usb_node,
                    b"qcom,gsi-reg-offset",
                    index,
                );
            }
            contract.gsi_disable_io_coherency = fdt::find_compatible_property_u32(
                address,
                usb_node,
                b"qcom,gsi-disable-io-coherency",
                0,
            )
            .is_some();
            contract.pm_qos_latency_us =
                fdt::find_compatible_property_u32(address, usb_node, b"qcom,pm-qos-latency", 0);
            contract.bus_mode_count =
                fdt::find_compatible_property_u32(address, usb_node, b"qcom,msm-bus,num-cases", 0);
            contract.bus_path_count =
                fdt::find_compatible_property_u32(address, usb_node, b"qcom,msm-bus,num-paths", 0);
            for flat in 0..12 {
                for field in 0..4 {
                    contract.bus_vectors[flat][field] = fdt::find_compatible_property_u32(
                        address,
                        usb_node,
                        b"qcom,msm-bus,vectors-KBps",
                        flat * 4 + field,
                    );
                }
            }
            let provider_ids_complete = contract
                .controller_clock_ids
                .iter()
                .chain(contract.qmp_clock_ids.iter())
                .chain(contract.reset_ids.iter())
                .chain(core::iter::once(&contract.hs_phy_clock_id))
                .all(Option::is_some);
            if provider_ids_complete
                && platform::bramble::usb_dt_clock_reset_contract_valid(&contract)
            {
                let _ = platform::bramble::install_usb_gcc_base(gcc.map(|r| r.base));
            } else {
                uart::puts(
                    "platform: GCC clock/reset provider IDs absent/mismatch; retaining compiled base\n",
                );
            }
            #[cfg(fullerene_aarch64_bramble)]
            if !usb::install_dt_phy_sequences(contract.hs_param_override, contract.qmp_init_seq) {
                uart::puts(
                    "platform: PHY init properties absent/invalid; retaining compiled tables\n",
                );
            }
            // Record the DT observation after the install attempt: presence
            // and exact byte length come from their own walk, and the raw
            // cells come from the reader the classifier consumed. This
            // closes the round-3 gap where "absent" and "present but the
            // first pair incomplete" were indistinguishable.
            #[cfg(fullerene_aarch64_bramble)]
            {
                usb::record_hs_dt_param_override_observation(
                    hsphy_observation.map(|item| (item.property_present, item.property_length)),
                    hsphy_observation
                        .map(|item| item.cells)
                        .unwrap_or(contract.hs_param_override),
                );
                usb::record_hs_dt_node_identity(
                    hsphy_observation.map(|item| (item.ordinal, item.reg_base)),
                );
            }
            if platform::bramble::install_usb_resource_contract(
                dwc3.map(|r| (r.base, r.size)),
                hs_phy.map(|r| (r.base, r.size)),
                qmp_phy.map(|r| (r.base, r.size)),
                apps_smmu.map(|r| (r.base, r.size)),
                pdc.map(|r| (r.base, r.size)),
                contract,
            ) {
                uart::puts("platform: Bramble USB resources sourced from DTB\n");
            } else {
                uart::puts("platform: Bramble DTB has no usable USB resource overrides\n");
            }
            let _ = platform::bramble::install_usb_hs_phy_eud_base(
                hs_phy_eud.map(|region| region.base),
            );
        } else {
            uart::puts("platform: Bramble DTB unavailable; using compiled Lito resources\n");
        }
    }
    let boot_info = make_boot_info(
        if bramble {
            BootPlatform::Bramble
        } else {
            BootPlatform::QemuVirt
        },
        dtb_address,
    );
    // Mark 8: the bootloader's tree has been walked and the black box (ring
    // plus both pstore mirrors) has been claimed at the addresses it names.
    let gicd_base = gicd_region.map(|region| region.base as usize);
    let gicr_base = gicr_region.map(|region| region.base as usize);
    let uart_region = if bramble { qcom_uart } else { pl011_uart };
    let uart_base = uart_region
        .map(|region| region.base as usize)
        .unwrap_or(if bramble {
            platform::bramble::UART_BASE
        } else {
            platform::qemu_virt::UART_BASE
        });
    if bramble {
        uart::init_qcom_geni(uart_base as u64);
    } else {
        uart::init_at(uart_base as u64);
    }
    uart::puts("hello from fullerene aarch64\n");
    if bramble {
        uart::puts("platform: bramble, uart: qcom-geni\n");
    } else {
        uart::puts("platform: qemu-virt, uart: pl011\n");
    }
    uart::put_hex("uart: base=", uart_base as u64);
    if let Some(region) = uart_region {
        if region.size != 0 {
            uart::put_hex("uart: size=", region.size);
        }
    }
    uart::put_hex("boot: x0=", fdt_address);
    uart::put_hex("boot: x1=", arg1);
    uart::put_hex("boot: x2=", fdt_arg2);
    uart::put_hex("boot: x3=", arg3);
    uart::put_hex("bootinfo: size=", BootInfo::BYTE_SIZE as u64);
    uart::put_hex("bootinfo: flags=", boot_info.flags);
    uart::put_hex("bootinfo: fdt=", boot_info.fdt_address);
    uart::put_hex("bootinfo: memory_map=", boot_info.memory_map_address);
    uart::put_hex("bootinfo: memory_map_size=", boot_info.memory_map_size);
    uart::put_hex(
        "gicd: base=",
        gicd_base.unwrap_or(if bramble {
            platform::bramble::GICD_BASE
        } else {
            platform::qemu_virt::GICD_BASE
        }) as u64,
    );
    uart::put_hex(
        "gicr: base=",
        gicr_base.unwrap_or(if bramble {
            platform::bramble::GICR_BASE
        } else {
            platform::qemu_virt::GICR_BASE
        }) as u64,
    );

    // Keep an opt-in early boundary for Bramble.  The standalone USB probe
    // historically reached a host-visible HS attach before the normal
    // Fullerene path had installed the MMU and allocator, so make that order
    // directly testable without changing the default Android-init boot.
    if bramble && !early_usb && option_env!("FULLERENE_AARCH64_USB_EARLY_HANDOFF") == Some("1") {
        early_usb = true;
        uart::puts("platform: bramble USB handoff before MMU: begin\n");
        usb_ready = init_bramble_usb_handoff();
        #[cfg(fullerene_aarch64_bramble)]
        usb::set_early_handoff_active(usb_ready);
        uart::puts(if usb_ready {
            "platform: bramble USB handoff before MMU: ready\n"
        } else {
            "platform: bramble USB handoff before MMU: failed\n"
        });
    }

    if let Some(address) = dtb_address {
        if let Some(header) = fdt::inspect(address) {
            uart::put_hex("dtb: address=", header.address);
            uart::put_hex("dtb: size=", header.total_size as u64);
            uart::put_hex("dtb: struct_offset=", header.structure_offset as u64);
            uart::put_hex("dtb: strings_offset=", header.strings_offset as u64);
            uart::put_hex("dtb: version=", header.version as u64);
        } else {
            uart::puts("dtb: unavailable or invalid\n");
        }
    } else {
        uart::puts("dtb: not supplied; using compiled platform defaults\n");
    }

    // UFS is described before the USB/GIC handoff, but this stage only reads
    // the merged DTB. No clock, regulator, PHY, interrupt, or MMIO ownership
    // is claimed until the real UFS host driver exists.
    match ufs::init(dtb_address) {
        Some(profile) => {
            uart::put_hex(
                "ufs: controller_regions=",
                profile.controller_regions as u64,
            );
            uart::put_hex("ufs: phy_regions=", profile.phy_regions as u64);
            uart::put_hex("ufs: irq_spi=", profile.irq_spi.unwrap_or(0) as u64);
            uart::put_hex(
                "ufs: missing_platform_mask=",
                profile.missing_required_mask() as u64,
            );
            uart::put_hex("ufs: dt_stage=", ufs::stage() as u64);
            if profile.platform_ready() {
                if let Some(sequence) = ufs::platform_sequence(profile) {
                    uart::put_hex("ufs: platform sequence steps=", sequence.count as u64);
                }
                uart::puts("ufs: platform contract described; HCI probe withheld\n");
            } else {
                uart::puts("ufs: incomplete platform contract; HCI probe withheld\n");
            }
        }
        None => uart::puts("ufs: no active qcom,ufshc platform contract\n"),
    }
    // Mark 9: the UFS contract stage returned (this stage only reads the
    // tree; a stop at 8 means the death was in the DT walk or the black box).
    unsafe { entry::ane_step_byte(b'9') };

    uart::puts("arch: aarch64, exception vectors: ready\n");
    uart::put_hex("currentel: ", exceptions::current_el() as u64);
    // Mark b was retired for stepmarks16: the 9->e splitter proved its point
    // twice (stepmarks13/14), and the record's 16 slots now cover the GIC
    // window (marks o/p). The console output above still exercises the tee
    // path before the MMU is up.

    #[cfg(fullerene_aarch64_qemu_usb_sim)]
    {
        let passed = usb_qemu_sim::run();
        cpu::semihost_exit(passed);
    }

    // Mark c was retired with b (stepmarks16); the in-init q/r/t/s split
    // below localises the flaky 9->e deaths: q = identity tables and user
    // roots built, r = TTBR/MAIR/TCR written and the TLB invalidated,
    // t = the loader's instruction cache dropped, s = SCTLR_EL1 flipped
    // (MMU and instruction cache live, data cache left off on the ANE).
    mmu::init();
    // ANE fingerprint: with our own identity map live, plant a known word at
    // the private block's +0x20 so the next boot's screen probe (its ZK row
    // reads this back) can tell whether the kernel's plain stores reach the
    // region at all. The loader-side probe's own fingerprint already lands
    // (its ZH row shows FTLX/FEED), so a missing KRNL here isolates the
    // failure to the kernel's world.
    #[cfg(fullerene_aarch64_ane)]
    unsafe {
        let p = 0x348d_0020 as *mut u32;
        core::ptr::write_volatile(p, 0x4c4e_524b); // "KRNL" little-endian
        core::ptr::write_volatile(p.add(1), 0x5445_5345); // "ESET"
    }
    // Mark d stays retired: the in-init r/s pair already brackets the
    // SCTLR switch, so the post-return position is covered by e below.
    uart::puts("mmu: identity map and caches ready\n");
    // Mark e: the post-MMU print survived.
    unsafe { entry::ane_step_byte(b'e') };
    // Mark a: the MMU is live (identity map, caches on).
    unsafe { entry::ane_step_byte(b'a') };

    // v278 REVERTED: an epoch mark written here (line ~922, before the
    // blackbox's own init) garbled the responder's descriptors - v279 collected
    // 'Љ'. step_probe is not safe this early in the ANE path, so the trail's
    // per-boot reset stays where the blackbox is already live and the
    // attribution problem is solved by the reader's KEEP_TRAIL instead.
    #[cfg(fullerene_aarch64_ane)]
    if option_env!("FULLERENE_ANE_KEEP_TRAIL").is_none() {
        unsafe {
            core::ptr::write_volatile(0x348d_e800 as *mut u32, 0);
        }
    }

    #[cfg(feature = "aarch64-user-launchd")]
    let mut allocator_ready = false;
    if let Some(mut frames) = allocator::PhysicalFrameAllocator::from_boot_info(&boot_info) {
        uart::put_hex(
            "memory: first free frame=",
            frames.first_available_frame().unwrap_or(0),
        );
        #[cfg(any(feature = "aarch64-user-smoke", feature = "aarch64-user-fault-smoke"))]
        user_smoke::run(&mut frames);
        #[cfg(feature = "aarch64-user-launchd")]
        {
            allocator::install_global(frames);
            fs::init();
            allocator_ready = true;
        }
        #[cfg(not(any(
            feature = "aarch64-user-smoke",
            feature = "aarch64-user-fault-smoke",
            feature = "aarch64-user-launchd"
        )))]
        {
            if frames.next_frame().is_some() {
                uart::puts("memory: DTB map frame allocator ready\n");
            } else {
                uart::puts("memory: DTB map has no free frame\n");
            }
        }
    } else {
        uart::puts("memory: no usable DTB map; frame allocator withheld\n");
    }

    // The allocator-block marks (d, e) used to bracket this section; they are
    // retired while the fine-grained h/q/r/s/t/i marks inside
    // from_boot_info consume the record's 16-byte mark budget.
    allocator::smoke();
    uart::puts("allocator: bump heap ready\n");

    // The ANE's loader can hand EL1 over with the generic counter gated
    // (CNTFRQ_EL0 reads zero, or the count never advances), and the old
    // "refuse to use the timer" path parked the boot right here - observed
    // 2026-10-04 as the first stop after the MMU fix (record: ...9a).
    // Measure when the counter is alive; proceed either way.
    if timer::init() {
        let before = timer::counter();
        timer::delay_ms(10);
        let elapsed = timer::counter().wrapping_sub(before);
        uart::puts("timer: generic counter ready, ticks=");
        uart::put_hex_value(elapsed);
        // Host-timed measurement hook: the run is timed host-side from the
        // flash to the fallback window, so pairing cntfrq + uptime here with
        // the heartbeat lines at the end yields the real counter frequency.
        uart::put_hex(" cntfrq=", timer::frequency());
        uart::put_hex(" up_ms=", timer::uptime_us() / 1_000);
    } else {
        uart::puts("timer: CNTFRQ_EL0 is zero; running without the generic counter\n");
    }
    // Mark b used to sit here; it is temporarily retired in favour of the
    // fine-grained h/i/j marks inside `from_boot_info` - the record's mark
    // block holds 16 bytes and the bring-up is currently stopped well before
    // this point (observed 2026-10-04: the run records end at a, h, or i).
    // Re-add a mark here once the timer stage is being reached.

    // The loader arms the SP805 watchdog for the boot attempt; the OS is
    // expected to take it over. Report its state once (read-only) - the feed
    // itself runs from the timer IRQ.
    #[cfg(fullerene_aarch64_ane)]
    {
        let wd = platform::ane::watchdog_state();
        uart::put_hex("ane: sp805 load=", wd.load as u64);
        uart::put_hex(" value=", wd.value as u64);
        uart::put_hex(" ctrl=", wd.control as u64);
        uart::put_hex(" lock=", wd.lock as u64);
    }

    // The USB bring-up runs once here: the vendor sequence ported into
    // platform::ane, reported on the console the same boot. Cross-boot
    // handoff turned out to be impossible - the loader re-initialises DRAM
    // every boot (measured six times) - so the writer must also be the
    // reporter. DCTL is the critical readback: SftDiscon stayed cleared
    // only if there is a real attach for the host to see.
    /// The ANE console mirror the platform self-reports use (tee -> durable
    /// log), shared by the USB bring-up report and the device-side service.
    #[cfg(fullerene_aarch64_ane)]
    fn ane_log(label: &str, value: u64) {
        uart::put_hex(label, value);
        uart::puts("\n");
    }

    #[cfg(fullerene_aarch64_ane)]
    {
        // Wipe canvas rows inherited from the previous (warm-reset) boot;
        // stale lines must never masquerade as current evidence in a photo.
        platform::ane_fb::clear();
        // v273 ABLATION: every per-tick diagnostic paint goes through
        // ane_fb::flush_row, which already early-returns on IRQ_QUIET. Setting
        // it here removes ALL of the flush_row work (a full-row memcpy plus a
        // cache_clean per channel and window - the cache maintenance is the
        // suspect) without touching the pump, the meter, or the responder, so
        // one collection prices the paints against the DWC2 access. The
        // instruments go blind in this build only.
        #[cfg(fullerene_aarch64_ane)]
        if option_env!("FULLERENE_ANE_IRQ_NO_PAINT").is_some() {
            platform::ane_fb::IRQ_QUIET.store(true, core::sync::atomic::Ordering::Relaxed);
        }
        // v268/v278: the per-boot trail reset lives at the top of the ANE path
        // now (see the '@' epoch mark), because fs::init() is itself a trail
        // writer and used to run before the reset.
        // Readable copy of the bootup-keypoint write for the durable log
        // (the binary ring record was taken at entry, before this console
        // exists). Re-writing is idempotent; the readback proves the value.
        platform::common::pmic::pmic_wr(0x18C, 250);
        uart::put_hex("bkp: rb=", platform::common::pmic::pmic_rd(0x18C) as u64);
        uart::puts("\n");
        // The loader wrote the reset reason into PMIC reg 0x18B (0x01 =
        // bootloader reset, 0x02 = recovery, 0x10 = cold boot; anything else
        // means a watchdog fired). Painting it puts the cause of the LAST
        // reset on the panel where a photo can reach it.
        uart::put_hex(
            "rr: pmic18b=",
            platform::common::pmic::pmic_rd(0x18B) as u64,
        );
        uart::puts("\n");
        platform::ane_fb::RST_REASON.store(
            platform::common::pmic::pmic_rd(0x18B) as u64,
            core::sync::atomic::Ordering::Relaxed,
        );
        platform::ane_fb::paint_hex_at(
            17,
            300,
            b"RST",
            platform::common::pmic::pmic_rd(0x18B) as u64,
        );
        platform::ane_fb::flush_panel();
        let usb = platform::ane::usb_bringup();
        uart::put_hex("usb: abb=", usb.abb as u64);
        uart::put_hex(" ppll0=", usb.ppll0.0 as u64);
        uart::put_hex("->", usb.ppll0.1 as u64);
        uart::puts("\n");
        uart::put_hex("usb: gusbcfg=", usb.core[0] as u64);
        uart::put_hex(" dcfg=", usb.core[1] as u64);
        uart::put_hex(" dctl=", usb.core[2] as u64);
        uart::put_hex(" gintsts=", usb.core[3] as u64);
        uart::puts("\n");
        uart::put_hex("usb: phy0=", usb.phy[0] as u64);
        uart::put_hex(" phy4=", usb.phy[1] as u64);
        uart::put_hex(" phy8=", usb.phy[2] as u64);
        uart::put_hex(" phyc=", usb.phy[3] as u64);
        uart::puts("\n");
        // Previous boot's dying words (the durable block survives reboots):
        // the rows above the tail window show the last panic file/line, the
        // only fault channel that needs no USB to read.
        platform::ane_fb::report_previous_panic();
        // Device-side USB: answer the host's control transfers now that the
        // link is up (EP0 + descriptors; fastboot bulk is the next stage).
        platform::ane_usb_dev::init(ane_log);
        // The host enumerates from the moment the link is up: service the
        // responder once before the storage work, and the storage waits
        // service it too (see ane_dwmmc::spin_usb). Unanswered control
        // requests piling up here were the v49/v52 "one request behind"
        // enumeration break.
        platform::ane_usb_dev::service(ane_log);
        // Storage bring-up: phase A logged the loader's dwmmc1/CRG/PMIC
        // state; phase B opens the clocks, releases the reset, powers the
        // rails, then reads the controller identity (all painted on panel).
        platform::ane_dwmmc::bringup(ane_log);
        platform::ane_usb_dev::service(ane_log);
        // eMMC (Arasan SDHCI @0xFF390000) phase 3: verify the inherited
        // clocks/reset/rail, read the controller identity, run the vendor
        // reset toggle + SDHCI software reset and clock re-init, then the
        // first card commands (CMD0/CMD1).
        platform::ane_emmc::probe(ane_log, 3);
        // v197: attach only now - the whole bring-up above ran with the
        // device soft-disconnected (usb_bringup step 10), so the host's
        // fresh enumeration meets an armed responder and the live
        // timer-IRQ pump instead of racing them. The old flow attached
        // early and ~half the boots enumerated garbled ("Љ") or never.
        // v210: the deferred attach returns to this site (the v208 experiment
        // that moved it after the GIC/timer bring-up is REVERTED: it did not
        // fix the first-read timeouts, and the longer IRQ-less pre-attach
        // window only widened the exposure).
        platform::ane::usb_attach();
        // Do not read DWC2 registers before ENUMDONE: the v184/v185
        // instrument showed that even diagnostic reads can disturb the
        // first enumeration. Keep the pre-desktop snapshot below gated.
        // Keep the attach visible to the timer-side recovery state machine.
        // This is deliberately separate from SETUP_SEEN: a host can be
        // physically connected while the very first descriptor request is
        // lost, and that case must still reach the no-button loader reset.
        platform::ane_usb_dev::mark_attached();
        // v199: stamp the attach tick for the enumeration sentinel.
        platform::ane_usb_dev::ATTACH_BEAT.store(
            exceptions::tick_count(),
            core::sync::atomic::Ordering::Relaxed,
        );
        ane_log("usb: attach=", 1);
        // Do not hold the boot thread in a pre-GIC polling loop here.  The
        // timer/GIC path is the proven responder pump; one immediate pass
        // handles a request already latched, then the normal IRQ service owns
        // later descriptor traffic.  The full-screen panel remains gated on
        // ENUMDONE below so it cannot compete with the first read.
        platform::ane_usb_dev::service(ane_log);
        // If a host reset/ENUMDONE was already latched, the panel is now safe
        // to paint. Otherwise leave the canvas alone until a later boot stage;
        // USB has priority over the diagnostic repaint.
        if platform::ane_usb_dev::usb_enumerated() {
            platform::ane_fb::report(&usb, ane_log);
            platform::ane_usb_dev::service(ane_log);
        }
    }
    // The usb report above is the newest text in the private block and sits
    // behind plain stores; the loader's reset comes long after the last
    // allocator flush, so the cache must drain now or the report dies with
    // the boot. tee_flush is safe to call repeatedly (appends only new bytes).
    #[cfg(fullerene_aarch64_ane)]
    crate::blackbox::tee_flush();

    // The UFS platform/storage backend is a Bramble build opt-in. Flasks
    // enables the read-only attempt for `--android-init` images; the direct
    // Cargo path still requires FULLERENE_AARCH64_UFS_EXECUTE=1. The separate
    // DMA identity assertion prevents a physical transaction until UTP
    // ownership is deliberate.
    #[cfg(fullerene_aarch64_bramble)]
    if option_env!("FULLERENE_AARCH64_UFS_EXECUTE") == Some("1") {
        if let Some(profile) = ufs::profile() {
            let rate_b = option_env!("FULLERENE_AARCH64_UFS_RATE_B") == Some("1");
            match ufs::execute_bramble_read_only(profile, rate_b, dtb_address) {
                Ok((device, geometry)) => {
                    let installed = ufs::install_bramble_block_device(device);
                    uart::put_hex("ufs: block size=", geometry.block_size as u64);
                    uart::put_hex("ufs: block count=", geometry.total_blocks);
                    if installed {
                        uart::puts("ufs: read-only block device registered\n");
                        #[cfg(feature = "aarch64-user-launchd")]
                        fs::mount_bramble_ufs();
                    } else {
                        uart::puts("ufs: read-only block device registration failed\n");
                    }
                }
                Err(ufs::ReadOnlyProbeError::DmaContractUnproven) => {
                    uart::puts("ufs: read-only probe withheld; DMA contract unproven\n")
                }
                Err(_) => uart::puts("ufs: read-only probe failed; block device withheld\n"),
            }
        } else {
            uart::puts("ufs: backend requested without profile\n");
        }
    }

    // Bring up the USB handoff before touching the GIC redistributor.  On a
    // phone boot path the redistributor may still be owned by firmware; USB
    // is polled during this early diagnostic phase and does not depend on it.
    if bramble && !early_usb {
        usb_ready = init_bramble_usb_handoff();
        #[cfg(fullerene_aarch64_bramble)]
        usb::set_early_handoff_active(usb_ready);
    }
    // USB setup itself remains trace-only; emit the compact ring after
    // controller initialization has returned and UART is safe to use again.
    if bramble {
        #[cfg(fullerene_aarch64_bramble)]
        usb::dump_trace();
    }
    #[cfg(feature = "aarch64-user-launchd")]
    devices::init(usb_ready, dtb_address);
    #[cfg(feature = "aarch64-user-launchd")]
    window::init();

    // USB must be handed off before touching the GIC, but the GIC/timer must
    // be live before launchd: native SLEEP and event deadlines are scheduler
    // waits, not architectural-counter spin loops. QEMU follows the same
    // ordering even though it has no physical Bramble handoff to perform.
    // Mark o: about to bring up the interrupt controller. The ANE's is a
    // memory-mapped GIC-400 (`arm,cortex-a9-gic`); the QEMU path's `msr
    // ICC_*` sequence is an undefined instruction on this CPU and was the
    // first post-timer death (stepmarks15: exception ELR at the msr).
    unsafe { entry::ane_step_byte(b'o') };
    if bramble {
        platform::bramble::init_interrupt_controller(gicd_base, gicr_base);
    } else if cfg!(fullerene_aarch64_ane) {
        platform::ane::init_interrupt_controller(gicd_base, gicr_base);
    } else {
        platform::qemu_virt::init_interrupt_controller(gicd_base, gicr_base);
    }
    // Mark p: the interrupt controller came back.
    unsafe { entry::ane_step_byte(b'p') };
    timer::arm_ms(1);
    exceptions::enable_irqs();
    timer::mark_irq_ready();

    #[cfg(feature = "aarch64-user-launchd")]
    if allocator_ready {
        // v229: the desktop (solvent + lattice) takes the canvas before the
        // user task starts. The launchd's VFS work can park for good; the
        // desktop paint makes the screen carry the result either way.
        //
        // v298: the v283-v297 production path (predesktop config wait +
        // enumeration watchdog) killed every unattended boot: the host's
        // first draw runs -110/-71 retries for 20-90 s, the bounded wait
        // abandoned EP0 mid-dance, the watchdog's reconnect tore D+ down
        // under the live host session, and the boot panicked to the loader.
        // The byte-exact v280 reproduction (see BUG_JOURNAL) proved the
        // reliable shape: NO predesktop wait, NO enumeration watchdog —
        // the historical flow attach -> service -> desktop ->
        // usb_reenumerate() -> service -> launchd, where the reenumerate
        // D+ drop/raise is the DESIGNED second enumeration edge and the
        // boot thread parks on its slow DCTL read while the timer-IRQ pump
        // answers the host. Restore that flow for the production path and
        // keep the HISTORICAL gate as the same-sequence alias.
        #[cfg(fullerene_aarch64_ane)]
        // v277: close the responder's duty window. The handoff is behind us,
        // so from here the timer IRQ stops touching DWC2 and the boot thread
        // owns the CPU for the storage probe, the desktop and the launchd -
        // the phases the pump used to starve.
        // v286: only close it when the host actually configured the device.
        // The v285 boot sat in the loader forever: with no host on the other
        // end the bounded wait timed out at 5 s with config=0, the window
        // then closed anyway, and from that point NOTHING serviced the
        // responder (the watchdog disarmed with the closed window and the
        // boot thread never returns to service()) - the device stopped
        // answering fastboot entirely. The window now stays open until the
        // handoff is real; a config=0 timeout keeps the pump running so the
        // device stays answerable to a late host.
        #[cfg(fullerene_aarch64_ane)]
        if option_env!("FULLERENE_ANE_PUMP_WINDOW").is_some()
            && platform::ane_usb_dev::usb_config_was_1()
        {
            platform::ane::usb_pump_window(false);
        }
        #[cfg(fullerene_aarch64_ane)]
        {
            // v300: reorder the production flow around the proven v280 shape.
            // v297 measured: with the desktop ON, the post-DESKTOP
            // reenumerate always lands mid-enumeration of the host's second
            // draw and the garbled-string state sticks for the whole boot.
            // The v280 sequence raises the second edge BEFORE the long CPU
            // work, so the fresh enumeration is answered by the timer pump
            // while nothing else runs. Do the same here: reenumerate FIRST,
            // then hand the canvas to the desktop renderer. The
            // HISTORICAL_NO_RECONNECT / FORCE_POST_RECONNECT knobs keep
            // their A/B roles.
            // v318: stage probes. The boot thread's post-storage narrative
            // never survives in the 2 KB piggyback window (the pump floods
            // EVT/UD/IQ at ~100/s), so the desktop's reachability was
            // unmeasurable. One ring event before/after each tail stage
            // names the last completed stage on the next collection.
            #[cfg(fullerene_aarch64_ane)]
            platform::ane_fb::event(b"STG PRE-RENUM");
            if (option_env!("FULLERENE_ANE_HISTORICAL_USB_SEQUENCE") == Some("1")
                && option_env!("FULLERENE_ANE_HISTORICAL_NO_RECONNECT") != Some("1"))
                || option_env!("FULLERENE_ANE_FORCE_POST_RECONNECT") == Some("1")
                || (option_env!("FULLERENE_ANE_HISTORICAL_USB_SEQUENCE").is_none()
                    && option_env!("FULLERENE_ANE_NO_POST_RECONNECT").is_none())
            {
                platform::ane::usb_reenumerate();
                #[cfg(fullerene_aarch64_ane)]
                platform::ane_fb::event(b"STG RENUM-OK");
            }
            platform::ane_usb_dev::service(ane_log);
            if option_env!("FULLERENE_ANE_SKIP_DESKTOP") != Some("1") {
                desktop::bring_up();
                #[cfg(fullerene_aarch64_ane)]
                platform::ane_fb::event(b"STG DESK-OK");
            }
            platform::ane_usb_dev::service(ane_log);
            if platform::ane_usb_dev::usb_enumerated() {
                ane_log(
                    "usb: dctl-after-desktop=",
                    platform::ane_usb_dev::dctl_snapshot() as u64,
                );
            }
        }
        uart::putc(b'.');
        uart::putc(b'.');
        uart::put_hex32(
            "launchd: pre t=",
            exceptions::TICK_NOW.load(core::sync::atomic::Ordering::Relaxed),
        );
        uart::puts("launchd: calling run\n");
        #[cfg(fullerene_aarch64_ane)]
        platform::ane_fb::event(b"LAUNCH RUN");
        launchd::run();
    } else {
        uart::puts("launchd: allocator unavailable; refusing to start userspace\n");
    }

    // Mark c used to sit here (see the mark-b note above for why the late
    // marks are temporarily retired).
    uart::puts("aarch64 early boot complete; waiting for timer irq / USB events\n");

    // stepmarks18: one line that separates a pre-IRQ abort from an abort
    // inside the IRQ handler in the log tail.
    uart::puts("ane: entering wait loop\n");
    loop {
        // Serve the device-side USB control transfers in the same loop.
        // The wait loop already wakes on the timer IRQ (the watchdog feed),
        // so one poll pass per wake keeps the host's transfers answered.
        #[cfg(fullerene_aarch64_ane)]
        platform::ane_usb_dev::service(ane_log);
        // The panel doubles as the debug channel: repaint the fastboot strip
        // whenever its state changed, so a photo shows the last exchange.
        #[cfg(fullerene_aarch64_ane)]
        platform::ane_fb::draw_fb_diag(&platform::ane_usb_dev::diag());
        // The controller-IRQ A/B gives the DWC3 SPI exclusive ownership of
        // the event ring.  Its exception path calls usb::poll() after the
        // GIC acknowledges that SPI; polling here as well would consume the
        // same GEVNTCOUNT window from a second owner and make the IRQ-vs-poll
        // discriminator meaningless.  The normal direct handoff keeps this
        // polling fallback because firmware-owned GIC routing is not assumed.
        #[cfg(all(
            fullerene_aarch64_bramble,
            not(any(
                fullerene_aarch64_usb_probe_irq_controller,
                fullerene_aarch64_usb_probe_timer_event_poll
            ))
        ))]
        usb::poll();
        // IRQ-owned and timer-owned A/B profiles both sleep here. Their
        // selected interrupt path is the sole event-ring consumer.
        #[cfg(all(
            fullerene_aarch64_bramble,
            any(
                fullerene_aarch64_usb_probe_irq_controller,
                fullerene_aarch64_usb_probe_timer_event_poll
            )
        ))]
        cpu::wait_for_event();
        // QEMU has no hardware USB path here, so it can sleep on the timer as
        // before.
        #[cfg(not(fullerene_aarch64_bramble))]
        cpu::wait_for_event();
    }
}

/// Run the bounded Bramble USB handoff sequence at the selected boot-order
/// boundary.  This remains a no-op on QEMU so the normal software runtime has
/// one shared call site without introducing a second platform implementation.
fn init_bramble_usb_handoff() -> bool {
    #[cfg(fullerene_aarch64_bramble)]
    usb::set_early_handoff_in_progress(true);
    #[cfg(not(fullerene_aarch64_bramble))]
    {
        true
    }

    #[cfg(fullerene_aarch64_bramble)]
    {
        // The standalone probe pets the APSS watchdog immediately before
        // entering the USB handoff.  The normal Android-init path may spend
        // time walking the DTB and describing UFS between its entry pet and
        // this point; refresh the same boundary so a post-DTB handoff does
        // not measure watchdog age instead of USB ownership.
        usb::wdt_pet();
        usb::dump_trace();
        // The standalone gadget-handoff probe deliberately leaves the
        // Fastboot-owned event ring and DMA objects intact until its
        // stop/reset ownership boundary.  Clearing them here would erase the
        // very handoff state that the attach-reaching probe reuses, while
        // `init_usb2_gadget_reuse_fastboot_ep0()` clears the region after the
        // controller has been stopped.  Keep the normal non-probe path's
        // early clear, but make the Android-init probe entry match the known
        // attach-reaching standalone ordering.
        #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
        usb::clear_dma_memory();
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        // After the previous-record dump, initialize the retained trace
        // before appending this boot's records. The probe path intentionally
        // defers the DMA clear until after the Fastboot controller is
        // stopped, so it cannot rely on clear_dma_memory() to seed the trace
        // header here. Keep the standalone probe's reset ordering without
        // touching any controller or DMA ownership state.
        usb::trace_probe_begin();
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        uart::puts("platform: preserving Fastboot DMA until USB handoff reset\n");
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        usb::trace_reset_head_for_boot();
        usb::trace_marker(usb::TRACE_BOOT_USB_ENTRY, 0);
        usb::trace_marker(usb::TRACE_TYPEC_BEGIN, 0);
        usb::note_platform_powered();
        let typec = if option_env!("FULLERENE_USB_SKIP_TYPEC_SPMI") == Some("1") {
            // Match the standalone probe's skip-Type-C A/B: Fastboot has
            // already established the device session, so avoid reopening
            // the PMIC/SPMI path before the early DWC3 handoff.
            uart::puts("platform: Type-C SPMI observation skipped\n");
            None
        } else {
            unsafe { platform::bramble::observe_usb_device_role() }
        };
        if let Some(typec) = typec {
            usb::install_typec_state(typec);
            usb::set_typec_orientation(typec.orientation_reverse);
            usb::note_typec_attached(typec.attached);
            // The PMIC child IRQ remains owned by the Fastboot handoff.
            // Reusing its live parent/child session would retrigger role
            // teardown before the new gadget is ready.
            uart::puts("platform: Type-C state observed; handoff writes skipped\n");
            usb::trace_marker(
                usb::TRACE_TYPEC_DONE,
                (typec.sink_mode_written as u32)
                    | ((typec.attached as u32) << 1)
                    | ((typec.attach_settled as u32) << 2)
                    | ((typec.misc_status as u32) << 8),
            );
            uart::put_hex("platform: PMIC arbiter=", typec.arbiter_version as u64);
            uart::put_hex("platform: Type-C status=", typec.misc_status as u64);
            uart::put_hex("platform: Type-C mode=", typec.mode as u64);
            uart::put_hex(
                "platform: Type-C orientation=",
                typec.orientation_reverse as u64,
            );
            uart::put_hex("platform: Type-C attached=", typec.attached as u64);
            uart::put_hex("platform: Type-C role=", typec.role as u64);
            uart::put_hex(
                "platform: Type-C attach-settled=",
                typec.attach_settled as u64,
            );
            uart::put_hex("platform: Type-C phase=", typec.phase as u64);
            if typec.sink_mode_written {
                uart::puts("platform: Type-C sink-only selected\n");
            }
        } else {
            usb::trace_marker(usb::TRACE_TYPEC_DONE, 0xffff_ffff);
            uart::puts("platform: Type-C SPMI state unavailable\n");
        }
        usb::trace_marker(usb::TRACE_USB_HANDOFF_BEGIN, 0);
        if usb::init_usb2_handoff() {
            uart::puts("platform: bramble USB2 gadget handoff: ready\n");
            usb::set_early_handoff_in_progress(false);
            // The standalone probe queues the optional host-visible
            // Run/Stop differential after its initializer returns. Android-
            // init enters through this path instead, so make the same
            // source-backed A/B available here; the helper is a no-op unless
            // FULLERENE_USB_ARM_BLIP was compiled into the image.
            usb::arm_blip_queue();
            true
        } else {
            uart::puts("platform: bramble USB2 gadget handoff: failed\n");
            if option_env!("FULLERENE_USB_SIGNAL_DMA_POST_RUNSTOP") == Some("1") {
                // A post-Run/Stop event-DMA diagnostic must not be masked by
                // the ordinary cold fallback: the attach would no longer
                // identify the tested handoff result.
                uart::puts("platform: post-Run/Stop DMA diagnostic: no cold fallback\n");
                usb::set_early_handoff_in_progress(false);
                false
            } else if usb::init_usb2_only() {
                // `fastboot boot` may jump through a vendor trampoline that
                // tears down Fastboot before entering the image.  In that
                // case preserving the handoff state cannot work.
                uart::puts("platform: bramble USB2 cold fallback: ready\n");
                // Keep the host-visible arm-stage diagnostic active for the
                // cold fallback too. The fallback can be the path that
                // actually publishes the HS pull-up when the preserved
                // handoff rejects an endpoint command.
                usb::arm_blip_queue();
                usb::set_early_handoff_in_progress(false);
                true
            } else {
                uart::puts("platform: bramble USB2 cold fallback: failed\n");
                usb::set_early_handoff_in_progress(false);
                false
            }
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    // Durable note FIRST: if the console write below ever blocks, the log
    // still carries the panic's source location.
    if let Some(location) = info.location() {
        crate::blackbox::panic_text(location.line() as u64, location.column() as u64);
    }
    uart::puts("fullerene aarch64 panic\n");
    if let Some(location) = info.location() {
        uart::puts("panic: file=");
        uart::puts(location.file());
        uart::put_hex("panic: line=", location.line() as u64);
        uart::put_hex("panic: column=", location.column() as u64);
    }
    // Do not park: reset straight into the loader's fastboot (reason
    // BOOTLOADER). Parking lets the watchdog reset us without a reason,
    // which feeds the loader's boot-fail counter and eventually its
    // recovery fallback; landing in fastboot instead lets the window
    // catcher reflash the device with no key combo. The catcher has an
    // 8-hour window armed during the work sessions.
    uart::puts("panic: resetting into loader fastboot\n");
    platform::ane_usb_dev::panic_reset_to_bootloader();
}
