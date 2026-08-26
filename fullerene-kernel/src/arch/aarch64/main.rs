#![feature(alloc_error_handler)]
#![no_std]
#![no_main]

extern crate alloc;

use core::arch::{asm, global_asm};
use fullerene_abi::boot::{self, BootArchitecture, BootInfo, BootPlatform};

mod allocator;
mod exceptions;
mod fdt;
mod mmu;
#[path = "../../platform/mod.rs"]
mod platform;
mod timer;
mod uart;
#[cfg(fullerene_aarch64_bramble)]
mod usb;
#[cfg(fullerene_aarch64_qemu_usb_sim)]
mod usb_dwc3_sim;
#[cfg(any(fullerene_aarch64_bramble, fullerene_aarch64_qemu_usb_sim))]
mod usb_protocol;
#[cfg(fullerene_aarch64_qemu_usb_sim)]
mod usb_qemu_sim;
#[cfg(any(fullerene_aarch64_bramble, fullerene_aarch64_qemu_usb_sim))]
mod usb_regs;

const BOOT_STACK_SIZE: usize = 64 * 1024;

/// Values supplied by the bootloader and captured before the bootstrap starts
/// using caller-saved registers.  This is deliberately `repr(C)`: the entry
/// stub owns the layout until it hands a pointer to Rust.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct Aarch64BootContext {
    pub x0: usize,
    pub x1: usize,
    pub x2: usize,
    pub x3: usize,
    pub current_el: usize,
    pub entry_sp: usize,
    pub relocation_delta: isize,
}

// Keep SP 16-byte aligned when the context is allocated. The final 8 bytes
// are padding reserved for the bootstrap frame, not part of the C layout.
const BOOT_CONTEXT_SIZE: usize = (core::mem::size_of::<Aarch64BootContext>() + 15) & !15;

#[unsafe(no_mangle)]
static mut AARCH64_BOOT_STACK: [u8; BOOT_STACK_SIZE] = [0; BOOT_STACK_SIZE];

// QEMU -kernel enters at _start without promising a usable SP. Establish a
// known, aligned stack before calling any Rust code, then capture the complete
// boot handoff in a stable context before any relocation or EL transition.
global_asm!(
    ".section .text.boot,\"ax\"\n\
     .balign 4\n\
     .global _start\n\
     .type _start, %function\n\
     _start:\n\
         adrp x9, AARCH64_BOOT_STACK\n\
         add x9, x9, :lo12:AARCH64_BOOT_STACK\n\
         mov x10, #{stack_size}\n\
         add sp, x9, x10\n\
         mov x6, sp\n\
         adrp x11, __bss_start\n\
         add x11, x11, :lo12:__bss_start\n\
         adrp x12, __bss_end\n\
         add x12, x12, :lo12:__bss_end\n\
     1:\n\
         cmp x11, x12\n\
         b.hs 2f\n\
         str xzr, [x11], #8\n\
         b 1b\n\
     2:\n\
         // The boot stack is part of .bss, so capture handoff registers only\n\
         // after the clear. x19 is callee-saved and survives the relocation\n\
         // call below; it holds the context pointer until Rust entry.\n\
         sub sp, sp, #{context_size}\n\
         mov x19, sp\n\
         stp x0, x1, [x19]\n\
         stp x2, x3, [x19, #16]\n\
         mrs x5, CurrentEL\n\
         str x5, [x19, #32]\n\
         str x6, [x19, #40]\n\
         str xzr, [x19, #48]\n\
         // QEMU may enter at EL1; Android-style AArch64 bootloaders may hand\n\
         // off at EL2. Normalize the latter to EL1h while preserving x0\n\
         // through the context and preserving the bootstrap stack.\n\
         mrs x5, CurrentEL\n\
         and x5, x5, #0xc\n\
         cmp x5, #0x8\n\
         b.eq 3f\n\
         b aarch64_el1_entry\n\
     3:\n\
         mov x5, #(1 << 31)\n\
         msr HCR_EL2, x5\n\
         msr CPTR_EL2, xzr\n\
         mov x5, #9\n\
         msr ICC_SRE_EL2, x5\n\
         isb\n\
         mov x5, #3\n\
         msr CNTHCTL_EL2, x5\n\
         msr CNTVOFF_EL2, xzr\n\
         mov x5, #0x3c5\n\
         msr SPSR_EL2, x5\n\
         adrp x5, aarch64_el1_entry\n\
         add x5, x5, :lo12:aarch64_el1_entry\n\
         msr ELR_EL2, x5\n\
         mov x6, sp\n\
         msr SP_EL1, x6\n\
         isb\n\
         eret\n\
     .size _start, . - _start\n\
     // Execute the FP/SIMD enable at EL1. Some firmware/QEMU reset paths\n\
     // ignore an EL2 write to CPACR_EL1 until the lower exception level is\n\
     // active.\n\
     .global aarch64_el1_entry\n\
     .type aarch64_el1_entry, %function\n\
     aarch64_el1_entry:\n\
         mov x5, #(3 << 20)\n\
         msr CPACR_EL1, x5\n\
         isb\n\
         // Android bootloaders may place an Image at a different physical\n\
         // base. Apply the PIE's relative relocations before entering Rust;\n\
         // the handoff values remain in the context while x0 is scratch.\n\
         adr x7, _start\n\
         mov x0, x7\n\
         bl aarch64_apply_relocations\n\
         str x0, [x19, #48]\n\
         mov x0, x19\n\
         b aarch64_rust_entry\n\
     .size aarch64_el1_entry, . - aarch64_el1_entry\n\
     ",
    stack_size = const BOOT_STACK_SIZE,
    context_size = const BOOT_CONTEXT_SIZE,
);

#[cfg(fullerene_aarch64_bramble)]
const LINK_ENTRY: usize = 0x8008_0040;
#[cfg(not(fullerene_aarch64_bramble))]
const LINK_ENTRY: usize = 0x4200_0040;

/// Apply the small relocation set emitted by the static-PIE linker.
///
/// This function is called before the normal Rust entry, so it intentionally
/// only uses PC-relative local code, linker symbols, and immediates. It must
/// not acquire a GOT-backed reference of its own.
#[unsafe(no_mangle)]
extern "C" fn aarch64_apply_relocations(runtime_entry: usize) -> isize {
    let relocation_delta = runtime_entry.wrapping_sub(LINK_ENTRY) as isize;
    let (mut cursor, end): (usize, usize);
    unsafe {
        // These must be PC-relative: the GOT is one of the things this loop
        // may be fixing up, so it cannot be read before the loop runs.
        asm!(
            "adr {cursor}, __rela_dyn_start",
            "adr {end}, __rela_dyn_end",
            cursor = out(reg) cursor,
            end = out(reg) end,
            options(nomem, nostack, preserves_flags),
        );
    }

    while cursor < end {
        let offset = unsafe { core::ptr::read_unaligned(cursor as *const usize) };
        let relocation_type = unsafe { core::ptr::read_unaligned((cursor + 8) as *const u32) };
        if relocation_type == 0x403 || relocation_type == 0x101 {
            let addend = unsafe { core::ptr::read_unaligned((cursor + 16) as *const usize) };
            let target = offset.wrapping_add(relocation_delta as usize) as *mut usize;
            unsafe {
                target.write(addend.wrapping_add(relocation_delta as usize));
            }
        }
        cursor += 24;
    }

    relocation_delta
}

fn make_boot_info(platform: BootPlatform, dtb_address: Option<u64>) -> BootInfo {
    let mut info = BootInfo::new(BootArchitecture::Aarch64, platform);
    if let Some(address) = dtb_address {
        info.fdt_address = address;
        if fdt::inspect(address).is_some() {
            info.flags |= boot::flags::FDT;
        }
    }
    info
}

#[unsafe(no_mangle)]
extern "C" fn aarch64_rust_entry(boot_context: *const Aarch64BootContext) -> ! {
    // The context lives at the top of the bootstrap stack. Copy it before
    // Rust starts using that stack for ordinary locals and call frames.
    let boot = unsafe { core::ptr::read(boot_context) };
    let fdt_address = boot.x0 as u64;
    let arg1 = boot.x1 as u64;
    let fdt_arg2 = boot.x2 as u64;
    let arg3 = boot.x3 as u64;

    // Establish a compiled-in console before looking at any bootloader
    // pointer. A vendor trampoline can hand us an invalid or absent DTB;
    // touching that address before VBAR and UART are ready turns a useful
    // handoff failure into an invisible synchronous abort.
    let compiled_bramble = cfg!(fullerene_aarch64_bramble);
    let early_uart_base = if compiled_bramble {
        platform::bramble::UART_BASE
    } else {
        platform::qemu_virt::UART_BASE
    };
    if compiled_bramble {
        uart::init_qcom_geni(early_uart_base as u64);
    } else {
        uart::init_at(early_uart_base as u64);
    }
    exceptions::install();
    uart::puts("fullerene: entered Rust before DTB discovery\n");
    uart::put_hex("boot: x0=", fdt_address);
    uart::put_hex("boot: x1=", arg1);
    uart::put_hex("boot: x2=", fdt_arg2);
    uart::put_hex("boot: x3=", arg3);
    uart::put_hex("boot: currentel=", boot.current_el as u64);
    uart::put_hex("boot: entry_sp=", boot.entry_sp as u64);
    uart::put_hex("boot: relocation_delta=", boot.relocation_delta as u64);

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
        .or_else(|| (!compiled_bramble).then_some(platform::qemu_virt::DTB_BASE));
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
            // Lito's `interrupts-extended` tuples are
            // (PDC phandle, pin, trigger), (GIC phandle, type, SPI,
            // trigger), then two more PDC tuples.  The child DWC3 node has
            // the usual (type, number, trigger) `interrupts` property.
            contract.irq_numbers[0] =
                fdt::find_compatible_property_u32(address, usb_node, b"interrupts-extended", 1);
            contract.irq_numbers[1] =
                fdt::find_compatible_property_u32(address, usb_node, b"interrupts-extended", 5);
            contract.irq_numbers[2] =
                fdt::find_compatible_property_u32(address, usb_node, b"interrupts-extended", 8);
            contract.irq_numbers[3] =
                fdt::find_compatible_property_u32(address, usb_node, b"interrupts-extended", 11);
            contract.irq_numbers[4] =
                fdt::find_compatible_property_u32(address, b"snps,dwc3", b"interrupts", 1);
            // The PM8150B Type-C child has no compatible string in the
            // Android PMIC DT. Its first SPMI interrupt is the platform IRQ
            // consumed by qcom-pmic-typec; the SPMI arbiter exposes the
            // corresponding summary as its `periph_irq` GIC SPI.
            for index in 0..4 {
                contract.typec_irq[index] =
                    fdt::find_named_property_u32(address, b"qcom,typec@1500", b"interrupts", index);
            }
            contract.spmi_parent_irq =
                fdt::find_compatible_property_u32(address, b"qcom,spmi-pmic-arb", b"interrupts", 1);
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
            for index in 0..6 {
                contract.hs_param_override[index] = fdt::find_compatible_property_u32(
                    address,
                    b"qcom,usb-hsphy-snps-femto",
                    b"qcom,param-override-seq",
                    index,
                );
            }
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

    uart::puts("arch: aarch64, exception vectors: ready\n");
    uart::put_hex("currentel: ", exceptions::current_el() as u64);

    #[cfg(fullerene_aarch64_qemu_usb_sim)]
    {
        let passed = usb_qemu_sim::run();
        qemu_semihost_exit(passed);
    }

    mmu::init();
    uart::puts("mmu: identity map and caches ready\n");

    allocator::smoke();
    uart::puts("allocator: bump heap ready\n");

    if !timer::init() {
        uart::puts("timer: CNTFRQ_EL0 is zero; refusing to use the timer\n");
        loop {
            unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
        }
    }
    let before = timer::counter();
    timer::delay_ms(10);
    let elapsed = timer::counter().wrapping_sub(before);
    uart::puts("timer: generic counter ready, ticks=");
    uart::put_hex_value(elapsed);

    // Bring up the USB handoff before touching the GIC redistributor.  On a
    // phone boot path the redistributor may still be owned by firmware; USB
    // is polled during this early diagnostic phase and does not depend on it.
    #[cfg(fullerene_aarch64_bramble)]
    usb::dump_trace();
    #[cfg(fullerene_aarch64_bramble)]
    usb::clear_dma_memory();
    #[cfg(fullerene_aarch64_bramble)]
    usb::trace_marker(usb::TRACE_BOOT_USB_ENTRY, 0);
    #[cfg(fullerene_aarch64_bramble)]
    usb::trace_marker(usb::TRACE_TYPEC_BEGIN, 0);
    #[cfg(fullerene_aarch64_bramble)]
    usb::note_platform_powered();
    #[cfg(fullerene_aarch64_bramble)]
    if let Some(typec) = unsafe { platform::bramble::prepare_usb_device_role() } {
        usb::install_typec_state(typec);
        usb::set_typec_orientation(typec.orientation_reverse);
        usb::note_typec_attached(typec.attached);
        if unsafe { platform::bramble::configure_typec_irq(&typec) } {
            uart::puts("platform: Type-C SPMI IRQ unmasked\n");
        } else {
            uart::puts("platform: Type-C SPMI IRQ unavailable; polling retained\n");
        }
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
    #[cfg(fullerene_aarch64_bramble)]
    usb::trace_marker(usb::TRACE_USB_HANDOFF_BEGIN, 0);
    #[cfg(fullerene_aarch64_bramble)]
    if usb::init_usb2_handoff() {
        uart::puts("platform: bramble USB2 gadget handoff: ready\n");
    } else {
        uart::puts("platform: bramble USB2 gadget handoff: failed\n");
        // `fastboot boot` may jump through a vendor trampoline that tears
        // down the Fastboot controller before entering the image.  In that
        // case preserving the bootloader's PHY state cannot work; retry with
        // the complete Qualcomm USB2 platform sequence.
        if usb::init_usb2_only() {
            uart::puts("platform: bramble USB2 cold fallback: ready\n");
        } else {
            uart::puts("platform: bramble USB2 cold fallback: failed\n");
        }
    }
    // USB setup itself remains trace-only; emit the compact ring after
    // controller initialization has returned and UART is safe to use again.
    #[cfg(fullerene_aarch64_bramble)]
    usb::dump_trace();

    if bramble {
        platform::bramble::init_interrupt_controller(gicd_base, gicr_base);
    } else {
        platform::qemu_virt::init_interrupt_controller(gicd_base, gicr_base);
    }
    timer::arm_ms(100);
    exceptions::enable_irqs();
    uart::puts("aarch64 early boot complete; waiting for timer irq / USB events\n");
    loop {
        #[cfg(fullerene_aarch64_bramble)]
        usb::poll();
        // Bramble keeps polling even if firmware-owned GIC state prevents
        // installing the USB SPI route. QEMU has no hardware USB path here,
        // so it can sleep on the timer as before.
        #[cfg(not(fullerene_aarch64_bramble))]
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags))
        };
    }
}

#[cfg(fullerene_aarch64_qemu_usb_sim)]
fn qemu_semihost_exit(passed: bool) -> ! {
    #[repr(C)]
    struct ExitBlock {
        reason: u64,
        status: u64,
    }

    let block = ExitBlock {
        reason: 0x20026, // ADP_Stopped_ApplicationExit
        status: if passed { 0 } else { 1 },
    };
    unsafe {
        asm!(
            "hlt #0xf000",
            in("x0") 0x18usize, // SYS_EXIT
            in("x1") &block as *const ExitBlock,
            options(noreturn),
        );
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    uart::puts("fullerene aarch64 panic\n");
    loop {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}
