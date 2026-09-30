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
pub const RAMOOPS_REGION_BASE: u64 = 0xb7e4_1000;

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
pub const CONSOLE_ZONE_OFFSET: u64 = 0x28_0000;

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
const CONSOLE_ZONE_BYTES: u64 = 0x10_0000;
/// Bytes of the pmsg zone, from this device's DT (`pmsg-size = <0x80000>`).
const PMSG_ZONE_BYTES: u64 = 0x8_0000;

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
pub const CODE_USB_STAGE: u32 = 16;
pub const CODE_USB_PHY_ENTER: u32 = 17;
pub const CODE_USB_PHY_LEAVE: u32 = 18;
pub const CODE_USB_DWC3_RESET: u32 = 19;
pub const CODE_USB_EP0_ARM: u32 = 20;
pub const CODE_USB_EP0_SETUP_DONE: u32 = 21;
pub const CODE_USB_DATA_CMD: u32 = 22;
pub const CODE_USB_DATA_TIMEOUT: u32 = 23;
pub const CODE_USB_POLL_TIMEOUT: u32 = 24;

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
unsafe fn cache_clean(address: usize, length: usize) {
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
unsafe fn cache_clean(_address: usize, _length: usize) {}

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

/// Append one record. Cheap and allocation-free, so it is usable from early
/// boot and from interrupt context.
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
    let written = written_total();
    let capacity = capacity();
    let first = written.saturating_sub(capacity);
    let mut data = [0u8; 768];
    let mut cursor = 0usize;
    cursor += write_text(&mut data, cursor, b"====0.0\n");
    cursor += write_text(&mut data, cursor, b"FULLERENE-BLACKBOX\n");
    cursor += write_text(&mut data, cursor, b"Fullerene black box: ");
    cursor += write_number(&mut data, cursor, boots());
    cursor += write_text(&mut data, cursor, b" boot(s), ");
    cursor += write_number(&mut data, cursor, written);
    cursor += write_text(&mut data, cursor, b" record(s) written\n");
    let mut index = first;
    while index < written && cursor + 64 < data.len() {
        if let Some(entry) = read(index) {
            cursor += write_text(&mut data, cursor, b"seq ");
            cursor += write_number(&mut data, cursor, entry.seq);
            cursor += write_text(&mut data, cursor, b" code ");
            cursor += write_number(&mut data, cursor, entry.code);
            cursor += write_text(&mut data, cursor, b" a0 ");
            cursor += write_number(&mut data, cursor, entry.arg0 as u32);
            cursor += write_text(&mut data, cursor, b" a1 ");
            cursor += write_number(&mut data, cursor, entry.arg1 as u32);
            cursor += write_text(&mut data, cursor, b"\n");
        }
        index += 1;
    }
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
pub const PMSG_ZONE_OFFSET: u64 = 0x38_0000;

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
/// driver applies (`size <= zone_size && start <= size`, `ram_core.c:556-566`); the stored
/// signature is the value the driver passes for these zones, 0, xored with
/// `PERSISTENT_RAM_SIG` (`ram_core.c:553`, `ram.c:1067`).
fn write_pstore_zone(zone: u64, data: &[u8]) {
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

/// Append one byte to the log and keep the zone's Linux header consistent with it.
///
/// The zone's `size` is the length of this log and `start` stays 0, so the driver's validity
/// tests (`size <= zone_size`, `start <= limit`) hold at every point and Android's ramoops
/// keeps the zone instead of re-initialising it - which is what the earlier attempt got wrong
/// when it wrote the ring here without a valid header.
fn append_text_byte(zone: u64, byte: u8) {
    let size = read_u32(zone + 8);
    // Bounded by the zone's own size, which `init_pstore` refuses to accept unless the region
    // covers all of it, and by TEXT_LOG_MAX which is smaller still.
    if size >= TEXT_LOG_MAX || (PMSG_ZONE_BYTES as u32) < size + 1 {
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
        _ => b'z',
    }
}

/// The inverse of `code_letter` for the codes it covers; 0 for anything it does not.
fn letter_code(letter: u8) -> u32 {
    match letter {
        b'b' => 1,
        b'r' => 2,
        b'0'..=b'8' => 16 + (letter - b'0') as u32,
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
        code_letter, letter_code, previous_segment_range, segment_count, segment_record_bytes,
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
        for code in [1u32, 2, 16, 17, 21, 24] {
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
