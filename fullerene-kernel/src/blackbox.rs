//! Black box: fixed-size boot records kept in reserved RAM that outlives a reboot.
//!
//! Why this exists: the Bramble USB bring-up dies in a way that neither the
//! framebuffer nor a UART can report on this hardware, and what is left is the
//! host's own view of the device. A RAM region the bootloader does not clear
//! gives a second, higher-capacity channel than the Run/Stop readout: records
//! survive the reset, so a later boot - Fullerene again, or Android - can read
//! what the failing boot was doing at the point it stopped.
//!
//! Where, taken from the device rather than from a guess: `tools/fdt_nodes.py`
//! over the 36 stock DTBOs pulled off the phone finds the same nodes in every
//! one of them,
//!
//! ```text
//! /fragment@61/__overlay__/ramoops_region@B7E41000
//!     reg = <0xb7e41000 0x400000>   no-map
//! /fragment@75/__overlay__/ramoops
//!     compatible = "ramoops"  memory-region -> ramoops_region
//!     record-size = 0x20000   console-size = 0x100000   pmsg-size = 0x80000
//! ```
//!
//! and Linux's own layout for those sizes (`fs/pstore/ram.c:1059-1086`, with
//! `ftrace-size` absent so the zone does not exist) is
//!
//! ```text
//! 0x000000  dmesg records  cnt = (0x400000-0x100000-0x80000)/0x20000 = 20
//! 0x280000  console zone  0x100000   -> 0xb80c1000
//! 0x380000  pmsg zone     0x80000    -> 0xb81c1000
//! 0x400000  end                      -> 0xb8241000 = alt_ramoops base
//! ```
//!
//! The end landing exactly on the next reservation in the same DT is the
//! arithmetic checking itself; the header comment in the vendor DTS mentioning
//! a different address is stale, and the DTBOs are what the bootloader uses.
//!
//! There are two logs, because one of them could not do both jobs:
//!
//! * the **record ring** in `ramoops_meta_region`, a *within-boot* instrument. Every region
//!   tried as a cross-boot home failed, this one included, so the ring is read in the boot
//!   that wrote it. See `BLACKBOX_OFFSET` for the placement and the list of rejections.
//! * the **durable text log** in the pmsg zone, one byte per record, which is the part that
//!   survives a reboot and therefore the part a later boot reads to find out what happened.
//!   See `append_text_log`.
//!
//! Safety: the caller passes a base and length that it has verified against the
//! DT (see the call in `arch::aarch64::main`); writes are bounded by that length
//! and never touch another page; the region is `no-map`, so the allocator cannot
//! hand it out. Cache maintenance is explicit because the AP shares this DRAM
//! with the bootloader and, after a reboot, with Android.

// The atomic counter this module started with was replaced by the head field inside the ring
// header, so the import is gone rather than left unused.

/// Where inside the region the record ring starts.
///
/// Fourth home. The first three all failed the same way, and each rejection is a
/// measurement rather than a guess:
///
/// 1. `ramoops_region + 0x380000` (the pmsg zone): cleared by Android's ramoops driver,
///    which re-initialises a zone whose header it does not recognise.
/// 2. `ramdumplog_region`: `session_head` zero on consecutive boots - the header was
///    re-created each boot. Bootloader-owned buffer.
/// 3. `ramoops_region + 0x270000` (the dmesg record tail): three consecutive runs all
///    reported "nothing before this boot", and the magic check agreed that the header
///    present was this boot's own.
/// 4. `ramoops_meta_region` (0xb8641000, 4 KiB), the candidate that is neither a pstore
///    zone nor the ramdump buffer, and where the ring lives now. Capacity is 119 records
///    ((0x1000 - 0x100 - 32) / 32) and the offset is 256 bytes in and 8-byte aligned, which
///    the header's u64 fields need.
///
/// 5. The answer, after a fifth home was tried and a re-write of the same values late in
///    the boot was tried to rule out "the early write is clobbered": *none* of these regions
///    preserves a header across a `fastboot boot` on this device. What does survive is text
///    written into the pmsg zone's data area, which is what the durable log uses. The ring
///    therefore stays here as a within-boot instrument and no longer pretends to be more.
pub const BLACKBOX_OFFSET: u64 = 0x100;

/// The smallest region the ring can be placed in: the offset, the header and one record.
///
/// Exported so the caller's candidate filter and this module's own sizing cannot disagree -
/// they did, and the filter demanded 4 KiB more than the region has, which disabled the ring
/// on every tree whose meta region is the documented 0x1000.
pub const RING_MIN_BYTES: u64 = BLACKBOX_OFFSET + HEADER_BYTES as u64 + RECORD_BYTES as u64;

/// Base of the ramoops region, which is where the console mirror goes.
///
/// Bramble's value is from its memory DTBO (`ramoops_region@B7E41000`). The
/// ANE-LX2J is a different board whose tree carries a ramoops node of its own:
/// `/ramoops` over `/reserved-memory/pstore-mem @ 0x34800000`, size 0x100000,
/// with `record-size = 0x20000`, `console-size = 0x80000`, `pmsg-size = 0x20000`.
#[cfg(not(fullerene_aarch64_ane))]
pub const RAMOOPS_REGION_BASE: u64 = 0xb7e4_1000;
/// See the pair above; the ANE-LX2J window comes from its own device tree.
#[cfg(fullerene_aarch64_ane)]
pub const RAMOOPS_REGION_BASE: u64 = 0x3480_0000;

/// Base of the alternate ramoops region (`alt_ramoops_region@B8241000` in this device's
/// memory DTBO, size 0x400000). Android's ramoops can be switched to it at runtime with the
/// driver's `use_alt` attribute, and then the primary buffer is what gets ignored.
pub const RAMOOPS_ALT_REGION_BASE: u64 = 0xb824_1000;

/// Offset of Linux's console zone inside the same region.
///
/// `ram.c:1059-1086` sizes the dmesg array first (`dump_mem_sz = size -
/// console_size - ftrace_size - pmsg_size`), so with this device's DT values the
/// console zone starts at 20 x 0x20000 = 0x280000. Kept separate from
/// `BLACKBOX_OFFSET` because they serve different readers: the record ring is for
/// our own dump path, this one is what Android's pstore will show.
#[cfg(not(fullerene_aarch64_ane))]
pub const CONSOLE_ZONE_OFFSET: u64 = 0x28_0000;
/// ANE-LX2J: the same formula over its tree's sizes gives
/// `dump_mem_sz = 0x100000 - 0x80000 - 0 - 0x20000 = 0x60000`, i.e. three
/// record slots of 0x20000 precede the console zone.
#[cfg(fullerene_aarch64_ane)]
pub const CONSOLE_ZONE_OFFSET: u64 = 0x6_0000;

/// Linux's `struct persistent_ram_buffer` header: `sig`, `start`, `size`, then
/// data (`fs/pstore/ram_core.c:32-37`). The zone's validity condition is
/// `size <= zone_size && start <= size` (`ram_core.c:556-566`).
const PRZ_SIG_XOR: u32 = 0x4347_4244; // PERSISTENT_RAM_SIG, "DBGC"
const PRZ_HEADER_BYTES: usize = 12;
/// The zone magic the driver passes for the console zone is 0
/// (`ram.c:1067-1068`), so the stored signature is 0 ^ PERSISTENT_RAM_SIG.
const CONSOLE_ZONE_SIG: u32 = PRZ_SIG_XOR;

/// The console zone's size, from this device's DT (`console-size = <0x100000>` in
/// `lito-redbull-common.dtsi`). A region is only mirrored if it can hold the whole
/// zone: `dump_mem_sz = size - console_size - ftrace_size - pmsg_size` puts the
/// console zone at 0x280000, so the region must be at least 0x380000 long.
#[cfg(not(fullerene_aarch64_ane))]
const CONSOLE_ZONE_BYTES: u64 = 0x10_0000;
/// ANE-LX2J: `console-size = <0x80000>` in its tree.
#[cfg(fullerene_aarch64_ane)]
const CONSOLE_ZONE_BYTES: u64 = 0x8_0000;
/// Bytes of the pmsg zone, from this device's DT (`pmsg-size = <0x80000>`).
#[cfg(not(fullerene_aarch64_ane))]
const PMSG_ZONE_BYTES: u64 = 0x8_0000;
/// ANE-LX2J: `pmsg-size = <0x20000>` in its tree.
#[cfg(fullerene_aarch64_ane)]
const PMSG_ZONE_BYTES: u64 = 0x2_0000;

/// The smallest region this module may be handed: it must contain the *whole* pmsg zone,
/// because the durable log is appended through `PMSG_ZONE_OFFSET + PMSG_ZONE_BYTES`. The
/// earlier bound was the zone's *start*, which would have accepted a region one byte past it
/// and then written outside the region the device tree verified - the one thing this module
/// must never do. (`init_pstore` had this as a suggestion from review; it is a memory-integrity
/// bound, so it is enforced here.)
const PSTORE_REGION_MIN: u64 = PMSG_ZONE_OFFSET + PMSG_ZONE_BYTES;

/// Where the Android-readable mirror goes. Both bases are resolved from the tree the
/// bootloader hands us, matched by node name, and are never assumed; each keeps its own
/// readiness flag so a missing node disables that mirror instead of writing somewhere
/// arbitrary.
static mut PSTORE_BASE: u64 = 0;
static mut PSTORE_READY: bool = false;
static mut PSTORE_ALT_BASE: u64 = 0;
static mut PSTORE_ALT_READY: bool = false;

/// Claim the primary ramoops region for the console mirror. Returns whether it was
/// accepted; the caller passes what the device tree actually says, so a renamed or
/// resized region disables the mirror rather than corrupting memory.
pub fn init_pstore(base: u64, length: u64) -> bool {
    // The node name and the address are both checked. The mirror is written into memory
    // another component owns, and a name match alone would not catch a region that moved.
    if base != RAMOOPS_REGION_BASE || length < PSTORE_REGION_MIN {
        return false;
    }
    unsafe {
        PSTORE_BASE = base;
        PSTORE_READY = true;
    }
    true
}

/// Claim the alternate ramoops region. Android can switch ramoops to this buffer at
/// runtime through the driver's `use_alt` attribute, and if it has done so the primary
/// buffer is ignored by the reader, so the mirror goes into both.
pub fn init_pstore_alt(base: u64, length: u64) -> bool {
    if base != RAMOOPS_ALT_REGION_BASE || length < PSTORE_REGION_MIN {
        return false;
    }
    unsafe {
        PSTORE_ALT_BASE = base;
        PSTORE_ALT_READY = true;
    }
    true
}

/// Whether each mirror target is armed, for the readout that distinguishes "the mirror
/// was never written" from "it was written and did not survive".
pub fn pstore_ready() -> (bool, bool) {
    unsafe { (PSTORE_READY, PSTORE_ALT_READY) }
}

/// Magic for the ring header ("FBLX" as a little-endian word plus a version tag).
const MAGIC: u64 = 0x4642_4c58_0000_0001;
const RECORD_BYTES: usize = 32;
const HEADER_BYTES: usize = 32;
// A stamp mechanism lived here for about ten minutes and killed the boot: it read a
// u64 from header offset 20, which is not 8-byte aligned, and an unaligned
// `read_volatile::<u64>` is an alignment fault on aarch64. Every run afterwards
// produced no CCS pulses at all - including `usb2-live-fresh-gates`, which had worked
// all day, and it survived a power-cycle, which is what finally pointed at the code
// rather than the phone. Removing the write but leaving the read was enough to keep it
// broken, which is why the constants are gone as well. Any future handoff between the
// two copies of this module must use an aligned offset and a byte-wise load, or better,
// stay inside the DT-verified region.
/// One 64-byte cache line, the unit the aarch64 clean works in.
// Only the aarch64 copy of `cache_clean` uses this; gating it keeps the host build, where
// that function is the no-op below, from warning about an unused constant.
#[cfg(target_arch = "aarch64")]
const CACHE_LINE: usize = 64;

/// Event codes. Keep them stable: a recorded log outlives the code that wrote it.
pub const CODE_BOOT: u32 = 1;
pub const CODE_DT_RESERVED_REGION: u32 = 2;
/// The ANE-LX2J's first record of a boot, written by `mark_entry` before the
/// console exists. Its letter is 'E': seeing it in the durable log is what
/// separates "the loader entered us and the early bring-up died" from "the
/// loader never entered us", which nothing else on that board can report.
pub const CODE_ANE_ENTRY: u32 = 64;
/// The PMIC bootup-keypoint write (STAGE_BOOTUP_END = 250) that tells the
/// loader's BFM this boot reached the OS - arg0 = readback value.
pub const CODE_BOOT_KEYPOINT: u32 = 65;
pub const CODE_USB_STAGE: u32 = 16;
pub const CODE_USB_PHY_ENTER: u32 = 17;
pub const CODE_USB_PHY_LEAVE: u32 = 18;
pub const CODE_USB_DWC3_RESET: u32 = 19;
pub const CODE_USB_EP0_ARM: u32 = 20;
pub const CODE_USB_EP0_SETUP_DONE: u32 = 21;
pub const CODE_USB_DATA_CMD: u32 = 22;
pub const CODE_USB_DATA_TIMEOUT: u32 = 23;
pub const CODE_USB_POLL_TIMEOUT: u32 = 24;
/// Written where the production data stage's own Start Transfer is issued, on endpoint 1.
///
/// `CODE_USB_DATA_CMD` cannot answer "when was the data stage issued": it is recorded by every
/// endpoint command, including the EP0 setup commands that run *before* the pull-up, so its first
/// occurrence has no timestamp at all. This code exists so the question has one writer and one
/// meaning.
pub const CODE_USB_DATA_STAGE_START: u32 = 25;

// Probe outcomes. These exist because a probe's result cannot be read inside the boot that runs
// it: its field would have to be published after the action at 2.5 s, which is already outside
// the run window (measured twice, as runs that produced no fields at all), and publishing before
// it samples the "not attempted" zero. Carrying the outcome through the durable log and reading
// it in the next boot is the only reading that works, and it is what these codes are for.
pub const CODE_PROBE_RETIRED: u32 = 40;
pub const CODE_PROBE_NOT_RETIRED: u32 = 41;
pub const CODE_PROBE_TRB_CONSUMED: u32 = 42;
pub const CODE_PROBE_TRB_STILL_OWNED: u32 = 43;

/// One record per two entries of the bare-metal l1d USB probe's 24-entry
/// [step, value] trace, carried over before the console path can die.
/// arg0 = (step_a << 32 | value_a), arg1 = (step_b << 32 | value_b).
pub const CODE_L1D_TRACE: u32 = 96;
/// One record per landscape scan-out rotation: arg0 = elapsed microseconds,
/// arg1 = the number of display channels rotated. Exists because the v331m
/// flush's cost was unmeasurable from the outside (the container never
/// enumerated), and a cost that cannot be read is a cost that regresses.
pub const CODE_ANE_ROT_FLUSH: u32 = 97;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Record {
    pub seq: u32,
    pub code: u32,
    pub stamp: u64,
    pub arg0: u64,
    pub arg1: u64,
}

static mut BASE: u64 = 0;
static mut LENGTH: u64 = 0;
static mut READY: bool = false;

/// Record count, read from the ring header in RAM rather than kept in a static.
///
/// The first version kept it in a `static WRITES` and a device run then reported
/// exactly one record where the boot path had written at least two. The likely
/// cause is this project's known hazard: `arch/aarch64/usb_probe.rs` is a second
/// crate root, so any `usb/` static exists twice, and a counter that lives in a
/// static can therefore be read from a different copy than the one that was
/// written. A value stored in the region itself cannot have that failure mode -
/// both copies read the same RAM - so the head in the header is the source of
/// truth and `WRITES` is gone.
fn written_total() -> u32 {
    if !usable() {
        return 0;
    }
    read_u32(header_base() + 16)
}

/// Offset of the head value this boot started from, inside the header spare bytes.
///
/// 24 is the first 8-byte-aligned offset after magic(0), version(8), boots(12) and
/// head(16); offset 20 was tried earlier today and its u64 read was an alignment
/// fault that killed five boots.
const HEADER_SESSION_HEAD: u64 = 24;

/// The record index at which this boot's own records begin.
///
/// Replaces the previous way of finding the previous boot's tail, which compared
/// each `CODE_BOOT`'s `arg0` against the current `boots()` and reported "no such
/// record" (state 2 of `usb2-live-blackbox-why`) even though the ring plainly held
/// records. Whatever that mismatch was - a stale read of the record, or the two
/// crate roots disagreeing about the counter - it is not worth more runs to pin
/// down when the boot can simply *record where it started*. That value cannot
/// disagree with itself: it is written once, in `init()`, before any of this boot's
/// records exist.
pub fn session_head() -> u32 {
    read_u32(header_base() + HEADER_SESSION_HEAD)
}

fn set_written_total(value: u32) {
    write_u32(header_base() + 16, value);
    unsafe { cache_clean((header_base() + 16) as usize, 4) };
}

#[cfg(target_arch = "aarch64")]
pub(crate) unsafe fn cache_clean(address: usize, length: usize) {
    let mut line = address & !(CACHE_LINE - 1);
    let end = address + length;
    while line < end {
        unsafe {
            core::arch::asm!("dc cvac, {0}", in(reg) line, options(nostack, preserves_flags))
        };
        line += CACHE_LINE;
    }
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
}

#[cfg(not(target_arch = "aarch64"))]
pub(crate) unsafe fn cache_clean(_address: usize, _length: usize) {}

#[inline]
fn read_u32(address: u64) -> u32 {
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

#[inline]
fn write_u32(address: u64, value: u32) {
    unsafe { core::ptr::write_volatile(address as *mut u32, value) }
}

#[inline]
fn read_u64(address: u64) -> u64 {
    unsafe { core::ptr::read_volatile(address as *const u64) }
}

#[inline]
fn write_u64(address: u64, value: u64) {
    unsafe { core::ptr::write_volatile(address as *mut u64, value) }
}

/// Capacity in records for the region length given at `init`.
fn capacity() -> u32 {
    let length = length();
    if length <= BLACKBOX_OFFSET + HEADER_BYTES as u64 {
        return 0;
    }
    ((length - BLACKBOX_OFFSET - HEADER_BYTES as u64) / RECORD_BYTES as u64) as u32
}

/// Fallback base for copies of this module that never saw the DT.
///
/// Why a constant is needed at all: `arch/aarch64/usb_probe.rs` is a second crate
/// root, so this module is compiled twice and its statics exist twice. The boot
/// path's copy resolves the region from the device tree and stores it in `BASE`;
/// the drive's copy then reads `BASE` as zero and computes addresses from
/// nothing. That is what produced "exactly one record" on the device - the reader
/// was looking at 0x380000, not at the region.
///
/// The value is not assumed to be correct: it is the address verified against the
/// device's own DTBOs (`ramoops_region@B7E41000`, all 36 of them agree) and it is
/// only used when the boot path has already written this module's magic at
/// `BLACKBOX_OFFSET`. If the DT does not name the region, no magic is there, the
/// drive's copy refuses to write, and the safety property - nothing is written
/// anywhere unverified - is preserved rather than weakened.
/// Base of the ring's home: `ramoops_meta_region@0xB8641000`, 4 KiB.
///
/// The ring is a *within-boot* instrument. It was moved to the ramoops pmsg zone and then
/// back, because the measurement said the zone keeps a text written by a word at about one
/// second but not our ring header written at `+0x100` in the first tenth of a second - with
/// and without a late re-write of the same values. Rather than keep guessing at that
/// asymmetry, the cross-boot copy of the sequence is carried by the text log (see
/// `append_text_log`), which sits in the part of the zone that *is* measured to survive, and
/// the ring stays here where its 4 KiB are enough for one boot's records.
const FALLBACK_REGION_BASE: u64 = 0xb864_1000;
const FALLBACK_REGION_LEN: u64 = 0x1000;

/// The region `init` is handed, and what the tree must say for the ring to be placed in it.
const RING_REGION_BASE: u64 = 0xb864_1000;
const RING_REGION_MIN: u64 = 0x1000;

/// The region base.
///
/// Always the constant, never the `BASE` static. The two crate roots this module is
/// compiled into each have their own `BASE`, and a device run finally showed the
/// consequence: `usb2-live-blackbox-state` reported `BASE != 0` in the copy the drive
/// runs, yet that copy's reads did not see what the boot's writes had put down - head
/// zero, `session_head` zero, records present but no `CODE_BOOT` matching this boot.
/// A static that is not zero-initialised in that image explains every one of those at
/// once, and the fix is not to guess which copy is which but to make the address
/// identical in both: a constant, with `init()` refusing to write unless the tree the
/// bootloader handed over names exactly this region. The verification still happens at
/// boot, so nothing unverified is ever written; it just no longer travels through a
/// static that can arrive uninitialised.
fn base() -> u64 {
    FALLBACK_REGION_BASE
}

/// The region length. Constant for the same reason as `base()`.
fn length() -> u64 {
    FALLBACK_REGION_LEN
}

/// Whether the region this boot verified is the one the constant names.
///
/// Called by `init()` before anything is written, so a device whose DT places the
/// region elsewhere gets a disabled black box rather than writes into memory that
/// nothing verified.
fn verified(resolved_base: u64, resolved_length: u64) -> bool {
    resolved_base == RING_REGION_BASE && resolved_length >= RING_REGION_MIN
}

/// Whether this copy may write: either it is the copy that claimed the region, or
/// the claim is visible at the fallback address, which can only be true if a boot
/// verified the region against the DT first.
fn usable() -> bool {
    if unsafe { READY } {
        return true;
    }
    read_u64(header_base()) == MAGIC
}

/// Whether this boot claimed a ring at all.
///
/// Use this, not `usable()`, on any path that runs before a region is known: `usable()`
/// probes the header address to answer its question, and on a machine whose tree has no
/// matching node - the QEMU preflight is exactly that - the probe reads an address that is
/// not memory and the boot dies with a synchronous fault. `publish_console()` is now called
/// unconditionally from the boot path, so it needs the question that cannot fault.
pub fn armed() -> bool {
    unsafe { READY }
}

fn header_base() -> u64 {
    base() + BLACKBOX_OFFSET
}

fn record_base() -> u64 {
    header_base() + HEADER_BYTES as u64
}

fn record_address(index: u32) -> u64 {
    record_base() + index as u64 * RECORD_BYTES as u64
}

/// Claim `length` bytes at `base` as the black box, continuing a previous
/// session when the header is already ours.
///
/// Returns false when the region is too small, which leaves the box disabled
/// rather than writing somewhere unverified.
pub fn init(base: u64, length: u64) -> bool {
    if !verified(base, length) {
        // The tree names a different region than the constant. Refuse, rather than
        // write somewhere this build has never been told about.
        return false;
    }
    unsafe {
        BASE = base;
        LENGTH = length;
    }
    let magic = read_u64(header_base());
    let boots = if magic == MAGIC {
        read_u32(header_base() + 12).wrapping_add(1)
    } else {
        write_u64(header_base(), MAGIC);
        write_u32(header_base() + 8, 0); // version
        write_u32(header_base() + 12, 1); // first boot for this ring
        write_u32(header_base() + 16, 0); // head
        write_u32(header_base() + HEADER_SESSION_HEAD, 0); // nothing before this boot
        1
    };
    write_u32(header_base() + 12, boots);
    // Where this boot's records begin. For a ring we just created that is zero; for
    // a continuing ring it is the head left by the previous boot, captured before
    // any record of this boot exists.
    if magic != MAGIC {
        write_u32(header_base() + HEADER_SESSION_HEAD, 0);
    } else {
        write_u32(
            header_base() + HEADER_SESSION_HEAD,
            read_u32(header_base() + 16),
        );
    }
    unsafe {
        cache_clean(header_base() as usize, HEADER_BYTES);
        READY = true;
    }
    // REJECTED, kept as a note so the next attempt does not repeat it: writing the
    // resolved address into the *fallback* region's header, so that a copy which
    // cannot see `BASE` could still find the truth. It was implemented and then
    // removed unused, because on this device the fallback address is exactly what
    // is in doubt - if the DT the bootloader hands over places the region
    // elsewhere, the stamp is a write to an address nothing verified, which is the
    // one thing this module must never do. What is verified at boot stays inside
    // the verified region; anything intended for the other copy has to be
    // discoverable without writing outside it.
    true
}

pub fn ready() -> bool {
    unsafe { READY }
}

/// Ring capacity for the region as initialised.
pub fn slots() -> u32 {
    capacity()
}

/// How many times this ring has been started (this boot included).
pub fn boots() -> u32 {
    if !ready() {
        return 0;
    }
    read_u32(header_base() + 12)
}

/// How many records have been written into the ring, saturating at capacity.
pub fn count() -> u32 {
    if !ready() {
        return 0;
    }
    written_total().min(capacity())
}

pub fn stamp_of_code(code: u32) -> Option<u64> {
    if !usable() {
        return None;
    }
    let capacity = capacity();
    if capacity == 0 {
        return None;
    }
    let written = written_total();
    let first = written.saturating_sub(capacity);
    let mut index = first;
    while index < written {
        let address = record_address(index % capacity);
        let entry_code = read_u32(address + 4);
        if entry_code == code {
            return Some(read_u64(address + 8));
        }
        index += 1;
    }
    None
}

/// Append one record. Cheap and allocation-free, so it is usable from early
/// boot and from interrupt context.
/// The stamp of the first record carrying `code`, if this boot wrote one.
///
/// The ring is this boot's instrument and does not survive a reset, but it is a *timeline*: every
/// record now carries when it happened, so asking one code for its stamp turns "where did it stop"
/// into "when did each step happen". The first occurrence is the interesting one for a stage that
/// runs once; a code that repeats (a timeout) is better asked for with its own word.
pub fn record(code: u32, stamp: u64, arg0: u64, arg1: u64) -> u32 {
    if !usable() {
        return 0;
    }
    let capacity = capacity();
    if capacity == 0 {
        return 0;
    }
    let index = written_total();
    let slot = index % capacity;
    let address = record_address(slot);
    // Every record carries when it happened, on the one clock the host shares with us. Callers pass
    // 0 because none of them can read that clock; filling it here means the timeline is a property
    // of the log rather than something each call site has to remember. `0` is not a meaningful
    // stamp for any caller today (all of them pass 0 and none distinguishes it from "unset").
    let stamp = if stamp == 0 {
        elapsed_since_connect().map(|ms| ms as u64).unwrap_or(0)
    } else {
        stamp
    };
    let entry = Record {
        seq: index,
        code,
        stamp,
        arg0,
        arg1,
    };
    write_u32(address, entry.seq);
    write_u32(address + 4, entry.code);
    write_u64(address + 8, entry.stamp);
    write_u64(address + 16, entry.arg0);
    write_u64(address + 24, entry.arg1);
    // Values first, then the index a reader uses to believe them: publish the
    // record itself before the head that points at it.
    unsafe { cache_clean(address as usize, RECORD_BYTES) };
    set_written_total(index + 1);
    unsafe { cache_clean(header_base() as usize, 20) };
    // Refresh the Android-readable mirror at a bounded rate. Not on every
    // record: `start_transfer()` is called from a poll loop that can run
    // thousands of times, and rewriting a kilobyte there would change the timing
    // of the very thing being measured.
    if index % 4 == 3 {
        publish_console();
    }
    // Durable copy: every record appends one byte, and the boot's own first record opens its
    // segment. Keyed on the *record* and not on the ring's head, which was the first version
    // and was wrong: a boot whose ring header did survive would have `index != 0` and would
    // therefore write no segment marker at all, merging itself into the previous boot's
    // segment. The durable log must not depend on the ring, which is exactly the thing that
    // does not survive here.
    if code == CODE_BOOT {
        open_text_segment();
    }
    append_text_log(code);
    index
}

/// Read back a record by ring index, for a dump path that runs after the boot
/// that wrote it.
pub fn read(index: u32) -> Option<Record> {
    if !usable() {
        return None;
    }
    let capacity = capacity();
    if capacity == 0 {
        return None;
    }
    if index >= written_total() {
        return None;
    }
    let address = record_address(index % capacity);
    Some(Record {
        seq: read_u32(address),
        code: read_u32(address + 4),
        stamp: read_u64(address + 8),
        arg0: read_u64(address + 16),
        arg1: read_u64(address + 24),
    })
}

/// The most recent record written in this boot, if any.
pub fn last() -> Option<Record> {
    let written = written_total();
    if written == 0 {
        return None;
    }
    read(written - 1)
}

/// The `back`-th most recent record written by a *previous* boot.
///
/// This reads the *ring*, which does not survive a reboot on this device, so in practice it
/// answers from `session_head()`-based bookkeeping inside this boot and not across boots. The
/// cross-boot answer is `previous_segment_*`, which reads the durable log in the pmsg zone.
///
/// `recent()` on its own cannot answer "where did the failing boot stop": by the
/// time a later boot reads the ring, its own boot and region records are the
/// newest entries and they bury the tail of the boot that actually failed. This
/// walks back past every record of the current boot by finding the newest
/// `CODE_BOOT` whose `arg0` is this boot's own number - the same field that makes
/// the survival test meaningful - and then indexes downwards from there.
pub fn previous_boot_recent(back: u32) -> Option<Record> {
    if !usable() {
        return None;
    }
    // Boundary from the recorded session start, not from matching boot numbers.
    let start = session_head();
    if start == 0 {
        return None;
    }
    let target = (start as usize).checked_sub(1 + back as usize)?;
    read(target as u32)
}

/// Kept for the boundary's own diagnostics; the boot-number comparison is only used
/// here now, to say *why* a read failed rather than to place the boundary. `allow`
/// because nothing calls it: it is the instrument that answered "no CODE_BOOT
/// carries this boot's number" (state 2), and deleting it would delete the only
/// place that question can be asked again if the mismatch ever needs pinning down.
#[allow(dead_code)]
fn legacy_previous_boot_recent(back: u32) -> Option<Record> {
    if !usable() {
        return None;
    }
    let written = written_total() as usize;
    let mine = boots() as u64;
    // Scan *forward* from the oldest surviving record for the first CODE_BOOT that
    // carries this boot's number. Scanning backwards finds the newest one, and
    // when a run enters the image twice there are two, so the "previous" boot's
    // tail came back as this boot's own second boot record - code 1, measured on
    // the first attempt at this word.
    let mut first_of_mine = None;
    let mut index = first_index() as usize;
    while index < written {
        if let Some(entry) = read(index as u32) {
            if entry.code == CODE_BOOT && entry.arg0 == mine {
                first_of_mine = Some(index);
                break;
            }
        }
        index += 1;
    }
    let first_of_mine = first_of_mine?;
    let target = first_of_mine.checked_sub(1 + back as usize)?;
    read(target as u32)
}

/// The `back`-th most recent record, where 0 is the newest.
///
/// This is the accessor the *useful* readout is built on. The width-carrying
/// transport turned out not to be denser in practice (`usb2-live-pulse-cal`: the
/// host merges pulses that are 120 ms apart, so a symbol still costs about a
/// second), so the box is not read out in full. What identifies a failure is the
/// *sequence of stage codes*, and a handful of those fit in one run's fields:
/// four codes at roughly 2.5 bits each is about four runs for the last sixteen
/// stages, which is the whole question "where did it stop".
pub fn recent(back: u32) -> Option<Record> {
    if !usable() {
        return None;
    }
    let written = written_total();
    if back >= written || back >= capacity() {
        return None;
    }
    read(written - 1 - back)
}

/// The boot number recorded by the oldest surviving `CODE_BOOT` record.
///
/// This is the survival proof `boots()` alone cannot give. `boots()` counts how
/// many times a header has been started, and that number can also grow because a
/// run boots the image twice; a record whose *own* boot number differs from the
/// current boot's can only exist if the ring was written by an earlier boot and
/// then read back after a reset, because every record this boot writes carries
/// the same number.
pub fn oldest_boot_record_number() -> Option<u64> {
    if !usable() {
        return None;
    }
    let written = written_total();
    if written == 0 {
        return None;
    }
    let mut index = first_index();
    while index < written {
        if let Some(entry) = read(index) {
            if entry.code == CODE_BOOT {
                return Some(entry.arg0);
            }
        }
        index += 1;
    }
    None
}

/// Whether any record in the ring carries `code`.
pub fn contains(code: u32) -> bool {
    let written = written_total();
    let capacity = capacity();
    let first = written.saturating_sub(capacity);
    for index in first..written {
        if let Some(entry) = read(index) {
            if entry.code == code {
                return true;
            }
        }
    }
    false
}

/// First record in the ring, which on a continuing session is the oldest one
/// still present rather than the first ever written.
pub fn first_index() -> u32 {
    let written = written_total();
    written.saturating_sub(capacity())
}

/// Mirror the ring into Linux's console zone so the log is readable from Android
/// as `/sys/fs/pstore/console-ramoops-0`.
///
/// The point of the mirror is that it needs no tooling of ours: if the phone
/// comes back up in Android, `adb shell` (or `bugreport`) can retrieve what
/// Fullerene recorded, provided the file is readable on a production build -
/// which is exactly what the plan asks to determine experimentally rather than
/// assume.
///
/// Format, from `fs/pstore/ram.c`: the zone holds a separator line of the form
/// `====<sec>.<nsec>` followed by text, and `ramoops_pstore_read()` parses that
/// header (`ram.c:42,187-206`). The timestamp is 0 because there is no clock this
/// early; a zero timestamp is a value the parser accepts, and the boot counter
/// carries the ordering instead.
///
/// This is called at a bounded rate, not per record: the record ring is written
/// from paths that can run thousands of times (the EP0 command poll), and a
/// 1 KiB rewrite per record would change the timing of the very thing being
/// measured.
/// The clock the device and the host share, installed by the arch layer.
///
/// The host's own timeline starts when it sees the pull-up, so "milliseconds since that pull-up" is
/// the one axis both sides can be plotted on. Without it, a host capture and a device record can
/// only be read side by side and guessed at; with it, "the host gave up 775 ms in" and "the device
/// answered at 1020 ms" are directly comparable.
///
/// A hook rather than a call into the arch layer, because this file is shared with x86_64 and must
/// not name aarch64 items. `None` before it is installed, which renders as time zero.
static mut CLOCK_HOOK: Option<fn() -> Option<u32>> = None;

/// Install the shared clock. Called by the arch layer when the pull-up goes out.
pub fn install_clock(hook: fn() -> Option<u32>) {
    unsafe { CLOCK_HOOK = Some(hook) };
}

/// Milliseconds since the pull-up, or `None` when no clock has been installed yet.
fn elapsed_since_connect() -> Option<u32> {
    unsafe { CLOCK_HOOK.map(|hook| hook()).flatten() }
}

/// The pstore dmesg prefix for a stamp in milliseconds: `====<sec>.<nsec>`.
///
/// The zone's reader parses this line and the log appended to is the same zone, so the prefix has to
/// keep its shape - and it has to carry the stamp. The seconds field alone would quantise a
/// sub-second timeline into uselessness, which is exactly the range this instrument exists for.
pub fn stamp_prefix(elapsed: Option<u32>, out: &mut [u8]) -> usize {
    let mut cursor = write_text(out, 0, b"====");
    let milliseconds = elapsed.unwrap_or(0);
    cursor += write_number(out, cursor, milliseconds / 1000);
    cursor += write_text(out, cursor, b".");
    let fraction = (milliseconds % 1000) * 1_000_000;
    // Fixed width: a reader splitting on '.' must not have to guess where the fraction starts.
    let mut digits = [b'0'; 9];
    let mut value = fraction;
    let mut index = 9;
    while index > 0 {
        index -= 1;
        digits[index] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    cursor += write_text(out, cursor, &digits);
    cursor + write_text(out, cursor, b"\n")
}

pub fn publish_console() {
    // `armed()`, not `usable()`: this runs from the boot path before anything is claimed on
    // machines whose tree has no matching node, and `usable()` answers by reading the header
    // address - which is not memory there. That fault is what the QEMU preflight caught.
    if !armed() {
        return;
    }
    // The guard here used to be `length() >= CONSOLE_ZONE_OFFSET + 0x1000`, i.e. the length
    // of the *ring's* region rather than the ramoops region's. The ring lives in a 4 KiB
    // reserved region, so that condition was false on every boot and this mirror has never
    // once been written - which is the whole reason Android's pstore showed nothing and the
    // route looked closed. The two regions are now tracked separately.
    //
    // Known coupling, left in place deliberately: the body below lists the ring, so the mirror
    // still needs a ring to have anything to say, and a boot where the pstore region validates
    // but the meta region does not writes no mirror. It is not worth splitting the header from
    // the body while the mirror is unreadable on this device anyway - Android's pstore is
    // root-only here, so nothing has ever read what this writes.
    let (primary, alt) = pstore_ready();
    if !primary && !alt {
        return;
    }
    let mut data = [0u8; 768];
    let mut cursor = stamp_prefix(elapsed_since_connect(), &mut data);
    cursor += dump_into(&mut data[cursor..]);
    if primary {
        write_pstore_zone(
            (unsafe { PSTORE_BASE }) + CONSOLE_ZONE_OFFSET,
            &data[..cursor],
        );
    }
    if alt {
        write_pstore_zone(
            (unsafe { PSTORE_ALT_BASE }) + CONSOLE_ZONE_OFFSET,
            &data[..cursor],
        );
    }
}

// REJECTED, kept as a record of the experiment: `reassert()`, which re-wrote the ring's
// header values about 900 ms into the boot on the theory that the write made in the first
// tenth of a second was later clobbered by the kernel's own start-up. It was implemented and
// run on the device three times; the boot counter still came back as 1 in the next boot every
// time, so the theory is wrong and the function is gone. What the measurement established is
// the pair of facts the durable text log below is built on: the ring header never survives
// here, at any offset tried, and a text written into the pmsg zone's data area does.

/// The cross-boot copy of the sequence: one byte per record, appended to the ramoops pmsg
/// zone straight after Linux's zone header.
///
/// Why the sequence needs a second home at all: the ring does not survive a reboot here. It
/// was measured, twice, with and without a late re-write of the same values - the boot
/// counter came back as 1 in the next boot every time. What *does* survive is a text written
/// into this zone's data area by a word at about one second, which was still readable two
/// boots later. So the durable copy is text, at the one offset proven to persist, and it is
/// appended one byte at a time so that appending cannot disturb what is already there.
///
/// Layout: `\n` then a decimal boot ordinal, then one character per record. Codes are mapped
/// to letters so that the log reads as text in a dump: 1 -> 'b' (boot), 2 -> 'r' (region),
/// 16..24 -> '0'..'8' by subtracting 16, and anything above that to 'z'.
const TEXT_LOG_OFFSET: u64 = PRZ_HEADER_BYTES as u64;
const TEXT_MARKER: u8 = b'\n';
const TEXT_LOG_MAX: u32 = 0x7_0000;

/// Offset of Linux's pmsg zone: the console zone (0x280000) plus its size (0x100000).
///
/// This is where the durable text log lives, and it is here because it is the only place
/// measured to keep a write across a reboot on this device. It is also a good neighbour:
/// nothing on either side writes it during an ordinary boot (it is a channel for userspace,
/// `/dev/pmsg0`, and Android's ramoops only reads it), so our bytes are not overwritten by
/// someone else's.
#[cfg(not(fullerene_aarch64_ane))]
pub const PMSG_ZONE_OFFSET: u64 = 0x38_0000;
/// ANE-LX2J: the console zone (0x60000) plus its size (0x80000). The zone is
/// again the last in the window, and again 0x100000 - 0xE0000 = 0x20000 wide.
#[cfg(fullerene_aarch64_ane)]
pub const PMSG_ZONE_OFFSET: u64 = 0xE_0000;

/// Panic-time durable note: plain stores only (no locks, no formatting
/// machinery beyond digit division), so it survives whatever state the
/// panic interrupted and lands in the same `oem log` stream. Renders as
/// "PANIC L=<line> C=<col>".
pub fn panic_text(line: u64, col: u64) {
    for &b in b"PANIC L=" {
        text_byte(b);
    }
    let mut div: u64 = 1;
    while line / div >= 10 {
        div *= 10;
    }
    let mut v = line;
    loop {
        text_byte(b'0' + (v / div) as u8);
        v %= div;
        if div == 1 {
            break;
        }
        div /= 10;
    }
    for &b in b" C=" {
        text_byte(b);
    }
    let mut div2: u64 = 1;
    while col / div2 >= 10 {
        div2 *= 10;
    }
    let mut v2 = col;
    loop {
        text_byte(b'0' + (v2 / div2) as u8);
        v2 %= div2;
        if div2 == 1 {
            break;
        }
        div2 /= 10;
    }
    text_byte(b'\n');
}

/// Record a panic's source location in the durable block.
///
/// The marker line is what `ane_fb::report_previous_panic` scans for, and
/// nothing ever wrote it: the reader has answered "none found" on every
/// boot since it was written. File on its own line first, then the
/// line/column as decimal, so the next boot can copy the path onto a row
/// and read back the two numbers without a formatter.
pub fn panic_record(file: &str, line: u64, col: u64) {
    text_puts("FULLERENE AARCH64 PANIC\n");
    text_puts("FILE ");
    text_puts(file);
    text_puts("\n");
    panic_text(line, col);
}

/// Append one record to the durable text log, and keep the zone header consistent with it.
///
/// The zone's `size` is the length of this log and `start` stays 0, so the driver's validity
/// tests (`size <= zone_size`, `start <= size`) hold at every point, and Android's ramoops
/// keeps the zone instead of re-initialising it. That is what the earlier attempt got wrong:
/// it wrote the ring into this zone without a valid header, and the zone was reset on every
/// Android boot because of it.
pub fn append_text_log(code: u32) {
    let (primary, alt) = pstore_ready();
    if !primary && !alt {
        return;
    }
    let byte = code_letter(code);
    if primary {
        append_text_byte((unsafe { PSTORE_BASE }) + PMSG_ZONE_OFFSET, byte);
    }
    if alt {
        append_text_byte((unsafe { PSTORE_ALT_BASE }) + PMSG_ZONE_OFFSET, byte);
    }
}

/// Boot-time RAM tee: every console byte lands here until the first flush.
///
/// Plain RAM stores only - no cache maintenance, no pstore header juggling -
/// so nothing in the tee path can fault before the kernel's own MMU is up.
/// The ANE bring-up measured a regression at the 9->a mark (the first UART
/// write after `mmu::init`) that appeared only once the tee wrote straight
/// into the pmsg zone; this buffer keeps the log available without touching
/// the pstore machinery pre-MMU.
static mut TEE_RAM: [u8; TEE_CAP] = [0; TEE_CAP];
static mut TEE_LEN: usize = 0;

/// Capacity of the boot tee; the renderer's window is far smaller.
const TEE_CAP: usize = 4096;

// ── Forward-progress watchdog ──────────────────────────────────────

/// Tracks the boot thread's forward progress so a silent stall cannot hang
/// the handset forever.
///
/// Every kernel-console write from non-IRQ context counts as progress: the
/// boot thread narrates what it does, so its own logging is the heartbeat.
/// The timer IRQ compares the counter against its last observation - after
/// `WARN_US` without progress it writes a stall note into the durable ring
/// (the unmanned collector reads it), and after `RESET_US` it panics, which
/// lands the device in the loader's fastboot where the armed engine flashes
/// the next image. This is the ANE counterpart of the x86 watchdog NMI that
/// draws the `SCHEDULER STALLED` / `PCIe MMIO HANG` diagnostics.
///
/// Lives in this module because both the console writer (the heartbeat
/// source) and the exception layer (the check) already depend on it, and it
/// must exist in every crate root that compiles the UART.
pub struct ProgressWatchdog {
    count: core::sync::atomic::AtomicU32,
    last_seen: core::sync::atomic::AtomicU32,
    stalled_since_us: AtomicU64,
    warned: core::sync::atomic::AtomicBool,
    in_irq: core::sync::atomic::AtomicBool,
}

/// v325: mirrored IRQ context for the ANE responder's ADB defer gate.
pub static ANE_IRQ_CONTEXT: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// The single watchdog instance.
static PROGRESS_WATCHDOG: ProgressWatchdog = ProgressWatchdog {
    count: core::sync::atomic::AtomicU32::new(0),
    last_seen: core::sync::atomic::AtomicU32::new(0),
    stalled_since_us: AtomicU64::new(0),
    warned: core::sync::atomic::AtomicBool::new(false),
    in_irq: core::sync::atomic::AtomicBool::new(false),
};

// A 64-bit counter with the widest storage the target can back.
//
// The watchdog is shared by every architecture this kernel builds for, but not
// every target has 64-bit atomics: Xtensa (ESP32) has none, so
// `core::sync::atomic::AtomicU64` does not exist there and the ESP32 build used
// to stop here. The ESP32 port runs a cooperative scheduler on one core and the
// watchdog only ever does relaxed loads and stores, so a volatile cell is the
// correct mechanism for that target - there is no concurrent writer to lose
// against. Targets with real 64-bit atomics keep them.
#[cfg(not(target_arch = "xtensa"))]
type AtomicU64 = core::sync::atomic::AtomicU64;

#[cfg(target_arch = "xtensa")]
struct AtomicU64(core::cell::UnsafeCell<u64>);

#[cfg(target_arch = "xtensa")]
unsafe impl Sync for AtomicU64 {}

#[cfg(target_arch = "xtensa")]
impl AtomicU64 {
    const fn new(value: u64) -> Self {
        Self(core::cell::UnsafeCell::new(value))
    }

    fn load(&self, _ordering: core::sync::atomic::Ordering) -> u64 {
        unsafe { core::ptr::read_volatile(self.0.get()) }
    }

    fn store(&self, value: u64, _ordering: core::sync::atomic::Ordering) {
        unsafe { core::ptr::write_volatile(self.0.get(), value) }
    }
}

impl ProgressWatchdog {
    /// Stall thresholds, chosen well above the slowest legitimate stall (an
    /// ELF load over the 48 KB/s eMMC path runs tens of seconds).
    const WARN_US: u64 = 20_000_000;
    // v169: raised from 120 s. The measured uptime runs ~2.5x real time on
    // this board and the boot's storage phase crawls at 48 KB/s (400 kHz
    // eMMC), so a legitimate VFS/initramfs load easily crosses three
    // minutes of real time with no console output - the old threshold
    // turned that into a false panic and the reset loop (the "probabilistic
    // progress" seen by hand). 900 s of uptime is ~6 minutes real: roomy
    // for the slow storage but still a fail-safe.
    //
    // v179 EXPERIMENT: the boots keep dying ~+12 s after enumeration with
    // no interaction, and the stall WARNING shows up on-screen within
    // single-digit seconds of real time (photo 13576: S=8 ticks, warning
    // already at the ring tail). That says the uptime clock races far
    // faster than the 2.5x the earlier measurement suggested - possibly
    // only during the early boot phase - which would turn this reset into
    // a ~12 real-second false panic. Disable the RESET (keep the warning,
    // keep the fail-safe story via the SP805 feed) and watch whether the
    // short lives vanish. If they do, the real fix is the uptime source
    // (timer base) or a tick-count-based stall metric; if they do not,
    // this was never the killer.
    const RESET_US: u64 = 0x4000_0000_0000_0000;

    /// Count one unit of forward progress (boot thread only).
    pub fn note(&self) {
        // v227: the PC instrument found the boot thread parked on the LL/SC
        // retry of this fetch_add (text_byte+0x20, 0x58AE04) in two boots,
        // and inside the console path in all twelve v226 collections. The
        // counter is only ever compared for inequality by the timer-side
        // check, and the kernel is single-core - a plain load/store cannot
        // spin and carries the same signal.
        let c = self.count.load(core::sync::atomic::Ordering::Relaxed);
        self.count
            .store(c.wrapping_add(1), core::sync::atomic::Ordering::Relaxed);
    }

    /// Mark IRQ-handler context: handler diagnostics are not boot progress.
    pub fn set_irq_context(&self, v: bool) {
        self.in_irq.store(v, core::sync::atomic::Ordering::Relaxed);
    }

    /// Called from the console funnel on every write.
    pub fn note_from_console(&self) {
        if !self.in_irq.load(core::sync::atomic::Ordering::Relaxed) {
            self.note();
        }
    }

    /// Timer-side check: warn once, finally reset, if progress has stopped.
    pub fn check(&self, now_us: u64) {
        use core::sync::atomic::Ordering;
        // A hold is a deliberate stop, not a stall. Letting this lane run
        // during one flooded the ring with a "WD: BOOT PROGRESS STALLED" line
        // every second, which pushed the twelve lines the hold exists to show
        // out of the tail, and then reset the board on top of that.
        if panel_hold() {
            self.stalled_since_us.store(now_us, Ordering::Relaxed);
            self.warned.store(false, Ordering::Relaxed);
            return;
        }
        let count = self.count.load(Ordering::Relaxed);
        if count != self.last_seen.load(Ordering::Relaxed) {
            self.last_seen.store(count, Ordering::Relaxed);
            self.stalled_since_us.store(now_us, Ordering::Relaxed);
            self.warned.store(false, Ordering::Relaxed);
            return;
        }
        let since = self.stalled_since_us.load(Ordering::Relaxed);
        if since == 0 {
            self.stalled_since_us.store(now_us, Ordering::Relaxed);
            return;
        }
        let stalled_us = now_us.saturating_sub(since);
        if stalled_us > Self::WARN_US && !self.warned.swap(true, Ordering::Relaxed) {
            crate::blackbox::text_puts("WD: BOOT PROGRESS STALLED\n");
        }
        if stalled_us > Self::RESET_US {
            panic!("boot progress watchdog: no forward progress");
        }
    }
}

/// Console-funnel hook: count progress when the boot thread is running.
pub fn watchdog_note() {
    PROGRESS_WATCHDOG.note_from_console();
}

/// Deadline (process uptime, us) for a deliberate reset requested by the ADB
/// cycle magic. The reply that precedes the request must reach the host
/// before the link drops, so the reset is scheduled ~2 s out and fired from
/// the timer tick instead of panicking inline.
static RESET_DEADLINE_US: AtomicU64 = AtomicU64::new(0);

/// Schedule a deliberate panic+reset at the given uptime (us). 0 disarms.
pub fn schedule_reset_at(deadline_us: u64) {
    RESET_DEADLINE_US.store(deadline_us, core::sync::atomic::Ordering::Relaxed);
}

/// Timer-tick check for a scheduled deliberate reset.
pub fn check_scheduled_reset(now_us: u64) {
    let d = RESET_DEADLINE_US.load(core::sync::atomic::Ordering::Relaxed);
    if d != 0 && now_us >= d {
        RESET_DEADLINE_US.store(0, core::sync::atomic::Ordering::Relaxed);
        panic!("cycle reset: deliberate");
    }
}

/// IRQ-context flag for the watchdog (set by the exception layer).
pub fn set_irq_context(v: bool) {
    // v325: the ANE responder's ADB defer gate mirrors the IRQ context
    // through this blackbox-owned atomic (blackbox is platform-independent;
    // ane_usb_dev reads it via crate::blackbox::ANE_IRQ_CONTEXT).
    ANE_IRQ_CONTEXT.store(v, core::sync::atomic::Ordering::Relaxed);
    PROGRESS_WATCHDOG.set_irq_context(v);
}

/// True while the exception layer is inside the IRQ handler.
pub fn in_irq_context() -> bool {
    PROGRESS_WATCHDOG
        .in_irq
        .load(core::sync::atomic::Ordering::Relaxed)
}

/// Timer-side watchdog check.
pub fn watchdog_check(now_us: u64) {
    PROGRESS_WATCHDOG.check(now_us);
}

/// Append a plain string to the durable text ring (upper-cased on the way in,
/// like every other console byte).
/// True while a deliberate panel hold owns this boot. Every watchdog in the
/// tree asks this before doing anything drastic: a hold exists so that
/// evidence no other mechanism can show stays on the panel long enough to be
/// photographed, and a reset to the loader is the one thing that destroys it.
pub static PANEL_HOLD: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn set_panel_hold(active: bool) {
    PANEL_HOLD.store(active, core::sync::atomic::Ordering::Relaxed);
}

pub fn panel_hold() -> bool {
    PANEL_HOLD.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn text_puts(message: &str) {
    for &b in message.as_bytes() {
        text_byte(b);
    }
}

/// v225: append a step byte to the ANE's private VFS trail block
/// (0x348de800: count u32 @+0, bytes @+4, cap 16). A shared helper so both
/// fs.rs and the contexts layer can mark progress around a suspected lock -
/// plain stores survive even when the ring path is wedged.
#[cfg(fullerene_aarch64_ane)]
pub fn step_probe(byte: u8) {
    unsafe {
        const BASE: usize = 0x348d_e800;
        let cnt = core::ptr::read_volatile(BASE as *const u32);
        if cnt < 16 {
            core::ptr::write_volatile((BASE + 4 + cnt as usize) as *mut u8, byte);
            core::ptr::write_volatile(BASE as *mut u32, cnt + 1);
        }
    }
}

/// v226: record a u64 (normally a static's address) into a probe slot; the
/// IRQ's 256-beat re-stamp prints it back as "Vx:<16 hex>" once the +8
/// marker byte is set. Used to compare the VFS static's identity between
/// the init site and the read site when they disagree about being Some.
#[cfg(fullerene_aarch64_ane)]
pub fn addr_probe(slot: usize, value: u64) {
    unsafe {
        core::ptr::write_volatile(slot as *mut u64, value);
        core::ptr::write_volatile((slot + 8) as *mut u8, 1);
    }
}

pub fn text_byte(byte: u8) {
    // Heartbeat for the progress watchdog: any console write from non-IRQ
    // context is forward progress.
    watchdog_note();
    // Upper-case on the way in: the read-back font carries digits, A-F and
    // the punctuation subset only, so folding letters keeps every log line
    // renderable on the device's own screen console.
    let byte = if byte.is_ascii_lowercase() {
        byte - 32
    } else {
        byte
    };
    unsafe {
        let len = core::ptr::addr_of!(TEE_LEN).read();
        if len < TEE_CAP {
            let buf = core::ptr::addr_of_mut!(TEE_RAM) as *mut u8;
            buf.add(len).write(byte);
            core::ptr::addr_of_mut!(TEE_LEN).write(len + 1);
        }
    }
    // Mirror into the private durable block as well. Plain stores at a fixed
    // absolute address, so this works before the MMU (the loader's fallback
    // reset then leaves the text behind) and needs no pstore machinery.
    #[cfg(fullerene_aarch64_ane)]
    unsafe {
        // Header-independent second copy at a fixed offset: whatever state
        // the block header is in (the loader tramples magic and size between
        // boots), this path appends linearly from a per-boot counter, so a
        // reader always finds this boot's log at a known address.
        let n = core::ptr::addr_of!(FIXED_LOG_LEN).read();
        if n < FIXED_LOG_CAP {
            core::ptr::write_volatile(
                (TEXT_BLOCK_BASE + FIXED_LOG_OFF + n as u64) as *mut u8,
                byte,
            );
            core::ptr::addr_of_mut!(FIXED_LOG_LEN).write(n + 1);
        }
        let raw = core::ptr::read_volatile((TEXT_BLOCK_BASE + 4) as *const u32);
        // The loader tramples the block header between boots (measured:
        // magic and size both change run to run), so a garbage size would
        // point this boot's append outside the log window - the log then
        // survives nowhere a reader can see. Treat any out-of-range value
        // as an empty block.
        let size = if raw < TEXT_BLOCK_CAP { raw } else { 0 };
        if size < TEXT_BLOCK_CAP {
            core::ptr::write_volatile((TEXT_BLOCK_BASE + 12 + size as u64) as *mut u8, byte);
            core::ptr::write_volatile((TEXT_BLOCK_BASE + 4) as *mut u32, size + 1);
        }
    }
}

static mut TEE_FLUSHED: usize = 0;

/// Copy the RAM tee's unflushed tail into every ready durable text log. Call
/// after the kernel's MMU and caches are up; the allocator's entry is the
/// natural first point. Safe to call repeatedly - each call appends only the
/// bytes written since the previous flush.
pub fn tee_flush() {
    let len = unsafe { core::ptr::addr_of!(TEE_LEN).read() };
    let start = unsafe { core::ptr::addr_of!(TEE_FLUSHED).read() };
    let (primary, alt) = pstore_ready();
    for i in start..len {
        let b = unsafe { (core::ptr::addr_of!(TEE_RAM) as *const u8).add(i).read() };
        if primary {
            append_text_byte((unsafe { PSTORE_BASE }) + PMSG_ZONE_OFFSET, b);
        }
        if alt {
            append_text_byte((unsafe { PSTORE_ALT_BASE }) + PMSG_ZONE_OFFSET, b);
        }
    }
    unsafe { core::ptr::addr_of_mut!(TEE_FLUSHED).write(len) };
    // The private block's bytes were written plain; make the new range durable
    // so the loader's fallback reset finishes with them already in RAM. Only
    // the range written since the last flush is cleaned (BSS zeroes the
    // watermark every boot, so this is a full clean exactly after the first
    // flush that closes a pre-MMU stretch).
    #[cfg(fullerene_aarch64_ane)]
    unsafe {
        let size = core::ptr::read_volatile((TEXT_BLOCK_BASE + 4) as *const u32);
        let cleaned = core::ptr::addr_of!(TEXT_BLOCK_CLEANED).read();
        if size > cleaned {
            cache_clean(
                (TEXT_BLOCK_BASE + 12 + cleaned as u64) as usize,
                (size - cleaned) as usize,
            );
            core::ptr::addr_of_mut!(TEXT_BLOCK_CLEANED).write(size);
        }
        // The header-independent copy at FIXED_LOG_OFF needs the same drain
        // while the data cache is on; it is what the ZB/ZK probes read.
        let fixed = core::ptr::addr_of!(FIXED_LOG_LEN).read();
        let fixed_cleaned = core::ptr::addr_of!(FIXED_LOG_CLEANED).read();
        if fixed > fixed_cleaned {
            cache_clean(
                (TEXT_BLOCK_BASE + FIXED_LOG_OFF + fixed_cleaned as u64) as usize,
                fixed - fixed_cleaned,
            );
            core::ptr::addr_of_mut!(FIXED_LOG_CLEANED).write(fixed);
        }
    }
}

/// Bytes of the fixed log copy already cleaned by `tee_flush` this boot.
#[cfg(fullerene_aarch64_ane)]
static mut FIXED_LOG_CLEANED: usize = 0;

/// Bytes of the private text block already cleaned by `tee_flush`. BSS zeroes
/// it every boot; it is only ever read and written post-relocation.
#[cfg(fullerene_aarch64_ane)]
static mut TEXT_BLOCK_CLEANED: u32 = 0;

/// Mark the start of a new boot's segment, so a reader can tell the previous boot's records
/// from this one's. The ordinal is the number of segments already in the log plus one, which
/// survives a reboot because the log does.
pub fn open_text_segment() -> u32 {
    let (primary, alt) = pstore_ready();
    if !primary && !alt {
        return 0;
    }
    let ordinal = text_segment_count() + 1;
    if primary {
        let zone = (unsafe { PSTORE_BASE }) + PMSG_ZONE_OFFSET;
        append_text_byte(zone, TEXT_MARKER);
        for digit in decimal_digits(ordinal) {
            append_text_byte(zone, digit);
        }
        append_text_byte(zone, b':');
    }
    if alt {
        let zone = (unsafe { PSTORE_ALT_BASE }) + PMSG_ZONE_OFFSET;
        append_text_byte(zone, TEXT_MARKER);
        for digit in decimal_digits(ordinal) {
            append_text_byte(zone, digit);
        }
        append_text_byte(zone, b':');
    }
    ordinal
}

/// The ANE-LX2J pstore window as its device tree declares it: three dmesg
/// record slots of 0x20000, a 0x80000 console zone and a 0x20000 pmsg zone,
/// end to end inside the 0x100000 reservation.
#[cfg(fullerene_aarch64_ane)]
const ANE_PSTORE_WINDOW: u64 = 0x10_0000;

/// The private durable text block: the one-photo console's log source.
///
/// The pmsg zone cannot serve this: the Huawei-side fallback boots between
/// ours zero its header (`start`/`size` reset; measured 2026-10-04 as the ZH
/// row reading `sig ok / start 0 / size 0`, which also made the screen reader
/// show the zone from its start and look frozen). This block lives in the
/// ramoops console zone's unused tail - the same argument as the step-mark
/// scratch 4 KiB above it: no pstore reader interprets the tail (the console
/// mirror writes at most a kilobyte at the zone's start), and nothing on
/// either side touches it during an ordinary boot. 56 KiB of text, ending
/// 4 KiB below the marks so the two can never meet.
pub const TEXT_BLOCK_BASE: u64 = 0x348d_0000;
/// "FTLX" little-endian: the first four bytes of a live block.
#[cfg(fullerene_aarch64_ane)]
pub const TEXT_BLOCK_MAGIC: u32 = 0x584c_5446;
/// Text capacity of the block; header is magic/size/cap (12 bytes).
pub const TEXT_BLOCK_CAP: u32 = 0xe000;
/// Offset of the header-independent log copy inside the block. The header
/// (magic/size/cap) is trampled by loader boots, so this copy appends from a
/// per-boot counter instead and stays findable at a fixed address.
#[cfg(fullerene_aarch64_ane)]
pub(crate) const FIXED_LOG_OFF: u64 = 0x4000;
/// Capacity of the fixed copy (the renderer's window is far smaller).
#[cfg(fullerene_aarch64_ane)]
const FIXED_LOG_CAP: usize = 0x1c00;
/// Bytes written to the fixed copy this boot (BSS zeroes it every boot).
#[cfg(fullerene_aarch64_ane)]
static mut FIXED_LOG_LEN: usize = 0;

/// Bytes this boot has written into the fixed copy. The copy spans more than
/// a single boot's lines, and DRAM keeps older content, so a reader must stop
/// at this length - otherwise it streams the previous kernels' stale tail.
#[cfg(fullerene_aarch64_ane)]
pub(crate) fn fixed_log_len() -> usize {
    unsafe { core::ptr::addr_of!(FIXED_LOG_LEN).read() }
}

/// The ANE-LX2J's first durable mark of a boot, written before any device
/// register is touched.
///
/// It takes the compiled-in window rather than a tree lookup on purpose: this
/// runs before the DTB is parsed, so a boot that dies during early bring-up -
/// the first UART write is the first device access of the whole boot - still
/// leaves an entry record behind for the next boot to read. `init_pstore`
/// still enforces the window, so a build for another board cannot write here
/// by accident.
#[cfg(fullerene_aarch64_ane)]
pub fn mark_entry() {
    if init_pstore(RAMOOPS_REGION_BASE, ANE_PSTORE_WINDOW) {
        open_text_segment();
        append_text_log(CODE_ANE_ENTRY);
    }
}

/// The durable log's bytes, or `None` when the zone is not armed or its length is nonsense.
fn text_log_data() -> Option<&'static [u8]> {
    let (primary, _alt) = pstore_ready();
    if !primary {
        return None;
    }
    let zone = (unsafe { PSTORE_BASE }) + PMSG_ZONE_OFFSET;
    let size = read_u32(zone + 8) as usize;
    if size == 0 || size > TEXT_LOG_MAX as usize {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts((zone + TEXT_LOG_OFFSET) as *const u8, size) })
}

/// How many segment markers the log holds.
fn text_segment_count() -> u32 {
    text_log_data().map(segment_count).unwrap_or(0)
}

/// The k-th record code of the segment that precedes this boot's, or 0 when there is none.
///
/// This is the readout that makes the whole exercise worth doing: it returns a stage code
/// recorded by the boot *before* this one.
pub fn previous_segment_byte(index: u32) -> u32 {
    let Some(data) = text_log_data() else {
        return 0;
    };
    let Some((start, end)) = previous_segment_range(data) else {
        return 0;
    };
    let mut seen = 0u32;
    for byte in segment_record_bytes(&data[start..end]) {
        if seen == index {
            return letter_code(byte);
        }
        seen += 1;
    }
    0
}

/// Whether the previous boot's segment contains a record with this code.
///
/// The single-field reader words are built on this: "did the probe retire" is answered once the
/// outcome is carried across a boot, which is the only way it can be answered at all here.
pub fn previous_segment_contains(code: u32) -> bool {
    let Some(data) = text_log_data() else {
        return false;
    };
    let Some((start, end)) = previous_segment_range(data) else {
        return false;
    };
    segment_record_bytes(&data[start..end]).any(|byte| letter_code(byte) == code)
}

/// How many record bytes the previous boot's segment holds, or 0 when there is none.
pub fn previous_segment_len() -> u32 {
    let Some(data) = text_log_data() else {
        return 0;
    };
    match previous_segment_range(data) {
        Some((start, end)) => segment_record_bytes(&data[start..end]).count() as u32,
        None => 0,
    }
}

/// The byte range of the segment before this boot's, or `None` when there is not one yet.
///
/// Segments run from one marker to the next and the last one belongs to this boot, so the one
/// before it is the boot whose records are wanted. Pulled out as a pure function with tests
/// below, because this is the part that silently reported "no record" for a whole run: a reader
/// that has genuinely found nothing and a reader that has lost its own place look identical
/// from the outside.
fn previous_segment_range(data: &[u8]) -> Option<(usize, usize)> {
    // The last two markers, kept as the scan goes: an earlier version remembered only the
    // first eight and then indexed the last two of *those*, so from the ninth boot onwards
    // "the previous boot" silently pointed at an ancient segment near the start of the log.
    let mut last: Option<usize> = None;
    let mut previous: Option<usize> = None;
    for (offset, byte) in data.iter().enumerate() {
        if *byte == TEXT_MARKER {
            previous = last;
            last = Some(offset);
        }
    }
    match (previous, last) {
        (Some(start), Some(end)) => Some((start, end)),
        _ => None,
    }
}

/// The record bytes inside a segment, with the opening marker and ordinal skipped.
///
/// The segment opens with the marker, a decimal ordinal and a colon, and everything after that
/// colon is a record byte. The ordinal is located *by position* and not by discarding digits:
/// stage codes are stored as the characters `0`..`8`, so a digit filter would silently delete
/// exactly the codes this log exists to carry. A test caught that; the arrangement of the same
/// test data is what makes it visible.
fn segment_record_bytes(segment: &[u8]) -> impl Iterator<Item = u8> + '_ {
    // A segment without a colon holds no record bytes, so it starts at its own end: one
    // return type, no early exit, because this is an `impl Iterator`.
    let start = match segment.iter().position(|byte| *byte == b':') {
        Some(colon) => colon + 1,
        None => segment.len(),
    };
    segment[start..]
        .iter()
        .copied()
        .filter(|byte| *byte != TEXT_MARKER && *byte != b':')
}

/// How many segment markers a zone's data holds.
fn segment_count(data: &[u8]) -> u32 {
    data.iter().filter(|byte| **byte == TEXT_MARKER).count() as u32
}

/// Digits describing the pmsg zone, which carries the durable log:
///
///   0  segments in the log: 2 when there are none yet, otherwise the count capped at 6
///   1  the Linux zone header: 1 the signature is valid (so Android keeps the zone rather than
///      re-initialising it), 2 it is not
///   2  whether the log holds any bytes: 1 yes, 2 no
///   3  which zone is in use: 1 the primary, 2 the alternate
///
/// Field 0 reports 2 rather than 1 for an empty log on purpose: this field was once read as
/// "one segment" when its value had actually been lost to an absorbed mark, and a readout that
/// cannot be told from a lie is worse than no readout.
pub fn text_log_digits() -> [u32; 4] {
    let (primary, alt) = pstore_ready();
    if !primary && !alt {
        return [2, 2, 2, 2];
    }
    let zone = if primary {
        (unsafe { PSTORE_BASE }) + PMSG_ZONE_OFFSET
    } else {
        (unsafe { PSTORE_ALT_BASE }) + PMSG_ZONE_OFFSET
    };
    let count = text_segment_count();
    [
        if count == 0 { 2 } else { count.min(6) },
        if read_u32(zone) == CONSOLE_ZONE_SIG {
            1
        } else {
            2
        },
        if read_u32(zone + 8) > 0 { 1 } else { 2 },
        if primary { 1 } else { 2 },
    ]
}

/// Digits describing the pmsg zone, which carries the durable log:
///
///   0  the Linux zone header *of the pmsg zone*: 1 the signature is valid, so both Android's
///      ramoops and our reader keep the zone instead of re-initialising it; 2 it is not. This
///      field previously tested the signature at the ring's base, whose only writer had been
///      deleted, so it read 2 forever - i.e. it claimed the zone was being discarded
///   1  the ring's magic at its own offset: 1 present, 2 not. With the ring now a
///      within-boot instrument this is always 1, and it is here so a reader can tell
///      "no record" apart from "nothing is mapped"
///   2  how many boots the ring's own counter has seen, capped at 6. This field is the
///      measurement that ended the search for a durable ring home: it reads 1 in every boot
///      because the header never survives, which is why the durable copy is the text log
///      and not the ring
///   3  how many records the ring holds, in thousands
pub fn pmsg_probe_digits() -> [u32; 4] {
    let (primary, _alt) = pstore_ready();
    if !primary {
        // Nothing to read. Answering with a digit rather than probing an address is the point:
        // a readout must not touch memory this boot has not been told about.
        return [2, 2, 2, 2];
    }
    let header_valid = if read_u32((unsafe { PSTORE_BASE }) + PMSG_ZONE_OFFSET) == CONSOLE_ZONE_SIG
    {
        1
    } else {
        2
    };
    let magic_present = if read_u64(header_base()) == MAGIC {
        1
    } else {
        2
    };
    let boots = boots().clamp(1, 6);
    let thousands = (written_total() / 1000).clamp(0, 5) + 1;
    [header_valid, magic_present, boots, thousands]
}

/// Write one ramoops zone: Linux's `struct persistent_ram_buffer` header
/// (`sig`, `start`, `size`) then the text.
///
/// Header last, so a reader that arrives mid-write sees either the previous consistent
/// state or this one, never a `size` that points at bytes not yet written. `size` is the
/// byte count of the text and `start` stays 0, which satisfies the validity test the
/// driver applies (`size <= buffer_size && start <= size`, where `buffer_size` is the
/// zone's size minus the header - `ram_core.c:521,562-563`); the stored
/// signature is the value the driver passes for these zones, 0, xored with
/// `PERSISTENT_RAM_SIG` (`ram_core.c:553`, `ram.c:1067`).
fn write_pstore_zone(zone: u64, data: &[u8]) {
    // The mirror is held to the same capacity bound as the durable log, so the two writers
    // cannot drift apart: `size` counts data bytes and must not exceed the zone's capacity,
    // or the driver calls the zone invalid and zaps it (`ram_core.c:562-563`). Every caller
    // today passes a buffer far smaller than a zone, which is exactly why the check belongs
    // here rather than in the caller's arithmetic.
    if data.len() as u64 + PRZ_HEADER_BYTES as u64 > CONSOLE_ZONE_BYTES {
        return;
    }
    unsafe {
        for (offset, byte) in data.iter().enumerate() {
            core::ptr::write_volatile(
                (zone + PRZ_HEADER_BYTES as u64 + offset as u64) as *mut u8,
                *byte,
            );
        }
        write_u32(zone + 4, 0);
        write_u32(zone + 8, data.len() as u32);
        write_u32(zone, CONSOLE_ZONE_SIG);
        cache_clean(zone as usize, PRZ_HEADER_BYTES + data.len());
    }
}

/// Render the black box as text, for whichever channel is carrying it to a host.
///
/// Two channels share this now: the pstore console mirror (Android's, root-only on this device) and
/// the CDC-ACM serial port Linux binds to `/dev/ttyACM0`. One renderer means the two cannot
/// disagree about what the box contains, and it is pure enough to be reasoned about off the device.
///
/// Returns the number of bytes written, and 0 when the ring was never claimed - there is nothing to
/// say then, and an empty body would read as "no events happened".
pub fn dump_into(out: &mut [u8]) -> usize {
    let written = written_total();
    let capacity = capacity();
    let first = written.saturating_sub(capacity);
    let mut cursor = 0usize;
    cursor += write_text(out, cursor, b"FULLERENE-BLACKBOX\n");
    cursor += write_text(out, cursor, b"Fullerene black box: ");
    cursor += write_number(out, cursor, boots());
    cursor += write_text(out, cursor, b" boot(s), ");
    cursor += write_number(out, cursor, written);
    cursor += write_text(out, cursor, b" record(s) written\n");
    let mut index = first;
    while index < written && cursor + 64 < out.len() {
        if let Some(entry) = read(index) {
            cursor += write_text(out, cursor, b"seq ");
            cursor += write_number(out, cursor, entry.seq);
            cursor += write_text(out, cursor, b" code ");
            cursor += write_number(out, cursor, entry.code);
            cursor += write_text(out, cursor, b" a0 ");
            cursor += write_number(out, cursor, entry.arg0 as u32);
            cursor += write_text(out, cursor, b" a1 ");
            cursor += write_number(out, cursor, entry.arg1 as u32);
            cursor += write_text(out, cursor, b"\n");
        }
        index += 1;
    }
    cursor
}

fn write_text(out: &mut [u8], mut cursor: usize, text: &[u8]) -> usize {
    let start = cursor;
    for byte in text {
        if cursor < out.len() {
            out[cursor] = *byte;
            cursor += 1;
        }
    }
    cursor - start
}

/// Minimal decimal writer: no allocator and no formatting machinery on the
/// black box path.
fn write_number(out: &mut [u8], cursor: usize, mut value: u32) -> usize {
    let mut digits = [0u8; 10];
    let mut count = 0usize;
    loop {
        digits[count] = b'0' + (value % 10) as u8;
        value /= 10;
        count += 1;
        if value == 0 || count == digits.len() {
            break;
        }
    }
    let mut written = 0usize;
    while count > 0 {
        count -= 1;
        if cursor + written < out.len() {
            out[cursor + written] = digits[count];
            written += 1;
        }
    }
    written
}

/// Diagnostic digits for `blackbox_state`: which parts of this copy's state are
/// set.
///
/// Written because "exactly one record" survived two fixes and the value in doubt
/// is which of four things is true *in the copy the drive is running*: whether
/// `init()` ran here, whether a resolved base is visible here, whether this copy
/// considers the region usable at all, and whether any record is visible. Those
/// four separate a stale static from a wrong address from a genuine empty ring,
/// and none of them is guessed at by reading the others.
pub fn state_digits() -> [u32; 4] {
    [
        if unsafe { READY } { 1 } else { 2 },
        // Was "a resolved base is visible here", which was the wrong question: the
        // base no longer travels through a static. The useful state now is whether
        // the header this build writes is already present, i.e. whether a previous
        // boot claimed this region.
        if read_u64(header_base()) == MAGIC {
            1
        } else {
            2
        },
        if usable() { 1 } else { 2 },
        if written_total() > 0 { 1 } else { 2 },
    ]
}

/// Threshold digits for `blackbox_count`: how many records the ring holds.
///
/// The count is the one measurement that keeps disagreeing with expectation, and
/// a single bit ("more than zero") cannot show by how much. Four thresholds at
/// once say whether the ring holds the two records the boot path writes, the five
/// a fully instrumented boot writes, or something else entirely - and the answer
/// decides whether the missing writes are in the recorder or in the reader.
pub fn count_thresholds() -> [u32; 4] {
    let written = written_total();
    [
        if written >= 1 { 1 } else { 2 },
        if written >= 2 { 1 } else { 2 },
        if written >= 5 { 1 } else { 2 },
        if written >= 20 { 1 } else { 2 },
    ]
}

/// Threshold digits for `blackbox_probe`: which internal value is wrong.
///
/// `blackbox_count` reported five or more records while `blackbox_recent` reported
/// that the oldest surviving entry does not exist - two answers that cannot both be
/// right when they share `written_total()`. The difference has to be in the values
/// they do *not* share, so this publishes those directly: the length the region was
/// claimed with, the ring capacity derived from it, the count itself as a range
/// (five-or-more is satisfied by a sane count and by a garbage one, which is why
/// the last field separates them), and whether the oldest entry is readable.
pub fn probe_digits() -> [u32; 4] {
    let length = length();
    let capacity = capacity();
    let written = written_total();
    [
        if length >= 0x40_0000 { 1 } else { 2 },
        if capacity >= 1000 { 1 } else { 2 },
        if written >= 5 { 1 } else { 2 },
        if written < 1000 { 1 } else { 2 },
    ]
}

/// The code of the `back`-th most recent record of the previous boot, shifted into
/// `2..=6`, or 1 when there is no such record.
///
/// One code per call, on purpose. `usb2-live-blackbox-stage` published several codes
/// in one run and the host's field grouping then disagreed with itself: two fields
/// that call the same function reported "a record exists" and "no record" in the
/// same run, which is only possible if one field's blips were absorbed and every
/// later field was read one slot early. A single field per run cannot be shifted,
/// so the ambiguity is removed by giving up the packing rather than by adding more
/// instrumentation to detect it.
pub fn previous_boot_code(back: u32) -> u32 {
    match previous_boot_recent(back) {
        // An ordinal ladder, not the raw code. A blip run carries at most six, so
        // there are five usable slots (2..=6) for eight codes - and the first
        // version collapsed them with `code % 5`, which maps 1, 6, 11, 16 and 21
        // onto the same digit and therefore cannot tell the boot record from a
        // stage marker. The question the box exists to answer is "how deep did the
        // previous boot get", so the ladder is ordered by depth instead:
        //
        //   1  no such record
        //   2  boot record (1) or reserved region (2)
        //   3  a stage marker (16)
        //   4  DWC3 device reset (19)
        //   5  EP0 arm (20)
        //   6  setup complete (21) or anything later (22-24)
        Some(entry) => match entry.code {
            1 | 2 => 2,
            16 => 3,
            19 => 4,
            20 => 5,
            _ => 6,
        },
        None => 1,
    }
}

/// Threshold digits for `blackbox_boots`: how many boots this ring has seen.
///
/// This is the clean persistence test and it took much longer to arrive at than it
/// should have. Record-count thresholds cannot show persistence: one boot writes
/// about five records by itself, so "five or more" is satisfied by a ring that was
/// re-initialised this very boot - which is exactly how a freshly re-initialised
/// ring got read as a surviving one, twice. The boot counter has no such defect. It
/// starts at one and can only grow if the header it lives in is still there on the
/// next boot, so a value above one is persistence and a value stuck at one is not,
/// regardless of how many records happen to be present.
pub fn boots_thresholds() -> [u32; 4] {
    let seen = boots();
    [
        if seen >= 1 { 1 } else { 2 },
        if seen >= 2 { 1 } else { 2 },
        if seen >= 5 { 1 } else { 2 },
        if seen >= 10 { 1 } else { 2 },
    ]
}

/// The boot counter modulo six, in one digit (`(boots % 6) + 1`).
///
/// Thresholds were the wrong shape for this and cost two runs: "five or more" and
/// "still five or more" cannot show whether the counter moved. One digit per run
/// can. Consecutive runs advance `boots` by the number of boots the harness performs
/// (one or two), so the digit after a run differs from the digit before it whenever
/// the header is surviving, and is identical when it is not - which is the
/// discrimination the thresholds could not give. Reading it modulo six is enough
/// because a run advances the counter by fewer than six.
pub fn boots_digit() -> u32 {
    (boots() % 6) + 1
}

/// Why `previous_boot_recent()` returns nothing, as one digit.
///
/// Guessing at this from a single bit has cost several runs: "no such record" can
/// mean the ring is empty, or that this boot's own `CODE_BOOT` was never found, or
/// that it *is* the oldest record in the ring - three different faults with three
/// different fixes, all reported identically as `None`. This separates them:
///
///   1  the ring holds no records at all
///   2  nothing was written to this ring before this boot (the ring header names no earlier
///      head, i.e. it was created here)
///   3  there was an earlier head, but no record before this boot's own can be read
///   4  a record from an earlier boot is readable
///
/// Returns 5 for "usable() says this copy may not touch the region", which is its
/// own fault and was previously invisible.
pub fn previous_boot_status() -> u32 {
    if !usable() {
        return 5;
    }
    let written = written_total();
    if written == 0 {
        return 1;
    }
    let start = session_head();
    if start == 0 {
        // nothing before this boot: either a fresh ring or the first boot on it
        return 2;
    }
    if previous_boot_recent(0).is_some() {
        4
    } else {
        3
    }
}

/// The largest `size` a pmsg zone of `zone_bytes` may hold: its data capacity,
/// `zone_size - sizeof(struct persistent_ram_buffer)` (`fs/pstore/ram_core.c:521`),
/// with `TEXT_LOG_MAX` as the tighter cap on Bramble's larger zone.
///
/// Named so both geometries can be tested from one host build: this build's own
/// constants bind on the device, and the *other* platform's zone size can be
/// passed in a unit test even though only one set of constants is compiled.
const fn text_log_capacity(zone_bytes: u64) -> u32 {
    let capacity = zone_bytes - PRZ_HEADER_BYTES as u64;
    if capacity < TEXT_LOG_MAX as u64 {
        capacity as u32
    } else {
        TEXT_LOG_MAX
    }
}

/// Append one byte to the log and keep the zone's Linux header consistent with it.
///
/// The zone's `size` is the length of this log and `start` stays 0, so the driver's validity
/// tests hold at every point (`size <= buffer_size` and `start <= size`, where `buffer_size` is
/// the zone's size minus the header - `fs/pstore/ram_core.c:521,562-563`) and Android's ramoops
/// keeps the zone instead of re-initialising it - which is what the earlier attempt got wrong
/// when it wrote the ring here without a valid header.
fn append_text_byte(zone: u64, byte: u8) {
    let size = read_u32(zone + 8);
    // The bound that matters is the zone's *data capacity*, not its nominal size: the append
    // writes at `header + size`, and Linux calls the buffer invalid as soon as `size` exceeds
    // `zone_size - sizeof(struct persistent_ram_buffer)` (`ram_core.c:521`, checked at
    // `:562-563`). Twelve bytes past that the write both leaves the zone - and the pmsg zone is
    // the *last* one in the window, so it would leave the reserved region itself - and reads as
    // invalid to Android, which zaps precisely the log this exists to preserve.
    if size >= text_log_capacity(PMSG_ZONE_BYTES) {
        return;
    }
    unsafe {
        core::ptr::write_volatile((zone + TEXT_LOG_OFFSET + size as u64) as *mut u8, byte);
        write_u32(zone + 4, 0);
        write_u32(zone + 8, size + 1);
        write_u32(zone, CONSOLE_ZONE_SIG);
        // Clean the header line and the byte just written, not the whole log: this runs once
        // per record, including from the EP0 poll loop that can run thousands of times, and
        // cleaning up to 448 KiB per append is exactly the timing perturbation this module
        // works to avoid everywhere else. The header is what a reader believes, so it is
        // cleaned with the byte that justifies it.
        cache_clean(zone as usize, PRZ_HEADER_BYTES);
        cache_clean((zone + TEXT_LOG_OFFSET + size as u64) as usize, 1);
    }
}

/// A segment's ordinal, always three digits so that its length is fixed.
fn decimal_digits(mut value: u32) -> [u8; 3] {
    let mut out = [b'0'; 3];
    for slot in (0..3).rev() {
        out[slot] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    out
}

/// Map a record code to the one byte the durable log stores for it.
///
/// Known limit: every code outside the set below maps to `z` and decodes back as 0, so the
/// durable log cannot tell those codes apart. The codes this driver records are 1, 2 and the
/// 16..24 stage range, and a check built on this mapping (`prev_stage`) must therefore ask
/// about that range and not about "any code above 15".
fn code_letter(code: u32) -> u8 {
    match code {
        1 => b'b',
        2 => b'r',
        16..=24 => b'0' + (code - 16) as u8,
        40 => b'R',
        41 => b'F',
        42 => b'C',
        43 => b'H',
        64 => b'E',
        _ => b'z',
    }
}

/// The inverse of `code_letter` for the codes it covers; 0 for anything it does not.
fn letter_code(letter: u8) -> u32 {
    match letter {
        b'b' => 1,
        b'r' => 2,
        b'0'..=b'8' => 16 + (letter - b'0') as u32,
        b'R' => 40,
        b'F' => 41,
        b'C' => 42,
        b'H' => 43,
        b'E' => 64,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the durable log's framing.
    //!
    //! These exist because of one specific failure: a reader reported "no record" for a whole
    //! run and it was impossible to tell from the outside whether the log was empty or the
    //! reader had lost its place. The framing is now pure functions over a byte slice, so the
    //! cases that were ambiguous on the device can be asserted here instead.

    use super::{
        PMSG_ZONE_BYTES, PRZ_HEADER_BYTES, TEXT_LOG_MAX, append_text_byte, code_letter,
        letter_code, previous_segment_range, read_u32, segment_count, segment_record_bytes,
        stamp_prefix, text_log_capacity, write_number, write_text, write_u32,
    };

    /// The previous segment's record bytes, copied into a fixed array: this crate is no_std,
    /// so the tests cannot reach for `Vec`.
    fn records_of(log: &[u8]) -> ([u8; 16], usize) {
        let (start, end) = previous_segment_range(log).expect("a previous segment");
        let mut out = [0u8; 16];
        let mut len = 0usize;
        for byte in segment_record_bytes(&log[start..end]) {
            out[len] = byte;
            len += 1;
        }
        (out, len)
    }

    #[test]
    fn write_number_renders_decimal_and_refuses_to_overrun() {
        let mut out = [0u8; 24];
        assert_eq!(write_number(&mut out, 0, 0), 1);
        assert_eq!(&out[..1], b"0");
        assert_eq!(write_number(&mut out, 0, 4242), 4);
        assert_eq!(&out[..4], b"4242");
        assert!(write_number(&mut out, 0, u32::MAX) <= 10, "ten-digit cap");
        let mut tiny = [0u8; 2];
        assert_eq!(
            write_number(&mut tiny, 0, 12345),
            2,
            "truncates, never panics"
        );
    }

    #[test]
    fn the_stamp_prefix_keeps_the_pstore_shape_and_carries_milliseconds() {
        let mut out = [0u8; 32];
        let length = stamp_prefix(Some(1234), &mut out);
        assert_eq!(&out[..length], b"====1.234000000\n");
        let length = stamp_prefix(Some(7), &mut out);
        assert_eq!(&out[..length], b"====0.007000000\n");
        let length = stamp_prefix(None, &mut out);
        assert_eq!(&out[..length], b"====0.000000000\n");
        let mut small = [0u8; 8];
        let length = stamp_prefix(Some(600000), &mut small);
        assert!(length <= small.len(), "must truncate, never overrun");
    }

    #[test]
    fn write_text_truncates_at_the_end_of_the_buffer() {
        let mut out = [0u8; 4];
        assert_eq!(write_text(&mut out, 0, b"abcdef"), 4);
        assert_eq!(&out, b"abcd");
        assert_eq!(write_text(&mut out, 3, b"xy"), 1);
    }

    /// The capacity each platform's pmsg zone gets, checked from one host build.
    #[test]
    fn the_text_log_capacity_matches_each_platform_zone() {
        // The ANE-LX2J's `/ramoops` pmsg-size is 0x20000, well under TEXT_LOG_MAX, so the zone's
        // own capacity is what binds there.
        assert_eq!(
            text_log_capacity(0x2_0000),
            0x2_0000 - PRZ_HEADER_BYTES as u32,
            "ANE-LX2J pmsg zone"
        );
        // Bramble's is 0x80000, where TEXT_LOG_MAX is the tighter cap.
        assert_eq!(
            text_log_capacity(0x8_0000),
            TEXT_LOG_MAX,
            "Bramble pmsg zone"
        );
    }

    /// `append_text_byte` must refuse to grow the log past the zone's data capacity.
    ///
    /// The driver's validity test is `size <= buffer_size` where `buffer_size` is the zone's
    /// size minus the header (`fs/pstore/ram_core.c:521`, checked at `:562-563`). A size past
    /// that is declared invalid and the zone is zapped, and because the append writes at
    /// `header + size`, it also leaves the zone - the last one in the window, so it would leave
    /// the reserved region.
    #[test]
    fn the_append_stops_at_the_pmsg_zone_capacity() {
        // The zone header is read and written as u32s, so the mock buffer has to be at least
        // 4-byte aligned - a plain `[u8; N]` static is 1-byte aligned and the volatile writes
        // would violate their alignment precondition (measured: the test aborted).
        #[repr(align(8))]
        struct AlignedZone([u8; PMSG_ZONE_BYTES as usize]);
        static mut ZONE: AlignedZone = AlignedZone([0; PMSG_ZONE_BYTES as usize]);
        let zone = unsafe { core::ptr::addr_of_mut!(ZONE.0) } as u64;
        let cap = text_log_capacity(PMSG_ZONE_BYTES);
        let last = zone + PRZ_HEADER_BYTES as u64 + cap as u64 - 1;

        // One byte below the cap: the append is taken and lands on the last usable byte.
        write_u32(zone + 8, cap - 1);
        append_text_byte(zone, b'x');
        unsafe {
            assert_eq!(read_u32(zone + 8), cap, "a byte of room must be used");
            assert_eq!(
                core::ptr::read_volatile(last as *const u8),
                b'x',
                "the byte lands on the last usable byte"
            );
        }

        // At the cap: refused, and nothing moves.
        append_text_byte(zone, b'y');
        unsafe {
            assert_eq!(read_u32(zone + 8), cap, "a full zone must not grow");
            assert_eq!(
                core::ptr::read_volatile(last as *const u8),
                b'x',
                "the refused byte must not have landed"
            );
        }
    }

    #[test]
    fn empty_log_has_no_previous_segment() {
        assert_eq!(segment_count(b""), 0);
        assert_eq!(previous_segment_range(b""), None);
    }

    #[test]
    fn a_log_with_only_this_boots_segment_has_no_previous_segment() {
        let log = b"\n001:br";
        assert_eq!(segment_count(log), 1);
        assert_eq!(previous_segment_range(log), None);
    }

    #[test]
    fn the_segment_before_this_boot_is_the_previous_one() {
        let log = b"\n001:br12\n002:bb";
        assert_eq!(segment_count(log), 2);
        assert_eq!(previous_segment_range(log), Some((0, 9)));
        let (records, len) = records_of(log);
        assert_eq!(&records[..len], b"br12");
    }

    #[test]
    fn with_three_segments_the_middle_one_is_previous() {
        let log = b"\n001:br\n002:rrr\n003:b";
        let (records, len) = records_of(log);
        assert_eq!(&records[..len], b"rrr");
    }

    #[test]
    fn multi_digit_ordinals_are_not_read_as_records() {
        let log = b"\n009:br\n010:br";
        let (records, len) = records_of(log);
        assert_eq!(&records[..len], b"br");
    }

    #[test]
    fn codes_round_trip_through_the_letters() {
        for code in [1u32, 2, 16, 17, 21, 24, 40, 41, 42, 43] {
            assert_eq!(letter_code(code_letter(code)), code, "code {code}");
        }
    }

    #[test]
    fn unknown_codes_collapse_to_the_same_letter() {
        // Documented limit, asserted so it cannot be forgotten: the durable log cannot tell
        // these apart, so a reader must not ask a question that needs it to.
        assert_eq!(code_letter(25), code_letter(99));
        assert_eq!(letter_code(code_letter(25)), 0);
    }
}
