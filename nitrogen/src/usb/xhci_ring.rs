//! xHCI Ring structures — TRB, Transfer Ring, Event Ring.
//!
//! All ring-related data structures and operations are confined here.
//! Rings are circular queues of Transfer Request Blocks (TRBs) used
//! for command submission, transfer scheduling, and event reporting.
//!
//! # Ring types
//!
//! | Ring         | Direction     | Purpose                              |
//! |--------------|---------------|--------------------------------------|
//! | CommandRing  | Driver → HC   | Enable Slot, Address Device, etc.    |
//! | TransferRing | Driver → HC   | Per-endpoint data transfers          |
//! | EventRing    | HC → Driver   | Command completion, transfer events  |

use crate::DriverContext;
use core::ptr;

// ============================================================================
//  TRB — Transfer Request Block (16 bytes)
// ============================================================================

pub const TRB_SIZE: usize = 16;

/// TRB type (bits 10..15 of flags field).
pub mod trb_type {
    pub const NORMAL: u8 = 1;
    pub const SETUP_STAGE: u8 = 2;
    pub const DATA_STAGE: u8 = 3;
    pub const STATUS_STAGE: u8 = 4;
    pub const LINK: u8 = 6;
    pub const ENABLE_SLOT: u8 = 9;
    pub const ADDRESS_DEVICE: u8 = 10;
    pub const CONFIGURE_ENDPOINT: u8 = 11;
    pub const EVALUATE_CONTEXT: u8 = 12;
    pub const RESET_ENDPOINT: u8 = 14;
    pub const STOP_ENDPOINT: u8 = 15;
    pub const SET_TR_DEQUEUE: u8 = 16;
    pub const RESET_DEVICE: u8 = 17;
    pub const NO_OP: u8 = 23;
}

/// TRB flag bits.
pub mod trb_flag {
    pub const CYCLE: u32 = 1 << 0; // Cycle bit
    pub const TC: u32 = 1 << 1; // Toggle Cycle (Link TRB)
    pub const CHAIN: u32 = 1 << 4; // Chain bit
    pub const IOC: u32 = 1 << 5; // Interrupt On Completion
    pub const IDT: u32 = 1 << 6; // Immediate Data
    pub const ENT: u32 = 1 << 11; // Evaluate Next TRB
    pub const DIR_IN: u32 = 1 << 16; // Direction = IN (Data Stage)
    pub const TRB_TYPE_SHIFT: u32 = 10;
    pub const TRB_TYPE_MASK: u32 = 0x3F << TRB_TYPE_SHIFT;
}

/// Raw TRB (16 bytes).
///
/// Layout per xHCI spec §6.4:
/// ```text
/// Offset 0-7:   Parameter (data buffer pointer, etc.)
/// Offset 8-11:  Status (transfer length, completion code)
/// Offset 12-15: Flags (cycle, type, IOC, chain, etc.)
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Trb {
    pub params: [u8; 8],
    pub status: u32,
    pub flags: u32,
}

impl Trb {
    /// Create a new TRB with the given type and initial cycle bit.
    pub fn new(trb_type: u8, cycle: u32) -> Self {
        Self {
            params: [0; 8],
            status: 0,
            flags: cycle | ((trb_type as u32) << trb_flag::TRB_TYPE_SHIFT),
        }
    }

    /// Set the data buffer physical address.
    pub fn set_data_ptr(&mut self, phys: u64) {
        self.params[..8].copy_from_slice(&phys.to_le_bytes());
    }

    /// Set the transfer length in the status field (bits 0-16).
    pub fn set_transfer_length(&mut self, len: u32) {
        self.status = (self.status & !0x1FFFF) | (len & 0x1FFFF);
    }

    /// Get the TRB type from the flags field.
    pub fn trb_type(&self) -> u8 {
        ((self.flags & trb_flag::TRB_TYPE_MASK) >> trb_flag::TRB_TYPE_SHIFT) as u8
    }

    /// Get the completion code from the status field (bits 24-31).
    pub fn completion_code(&self) -> u8 {
        ((self.status >> 24) & 0xFF) as u8
    }

    /// Remaining transfer length (bits 0-23 of status).
    pub fn remaining(&self) -> u32 {
        self.status & 0xFFFFFF
    }
}

// ============================================================================
//  Ring — driver-to-controller transfer ring
// ============================================================================

/// A circular transfer ring (Command Ring or per-Endpoint Transfer Ring).
///
/// The driver enqueues TRBs; the controller dequeues and executes them.
/// A Link TRB at `entries[len - 1]` points back to `entries[0]` for
/// circular behaviour.
pub struct Ring {
    entries: &'static mut [Trb],
    /// Physical address of the ring buffer.
    pub phys: u64,
    /// Next enqueue index.
    enq: usize,
    /// Current producer cycle state.
    pub cycle: u32,
    /// Number of TRB slots (including the Link TRB at the end).
    len: usize,
}

impl Ring {
    /// Free the ring's physical memory.
    pub fn free(&self, ctx: &dyn DriverContext) {
        let size = self.len * TRB_SIZE;
        let pages = (size + 4095) / 4096;
        let _ = ctx.free_contiguous_frames(self.phys, pages);
    }

    /// Allocate a ring with `n` TRB slots.
    ///
    /// The ring is backed by contiguous physical memory and includes
    /// a Link TRB at `entries[n - 1]` that wraps back to the start.
    pub fn alloc(ctx: &dyn DriverContext, n: usize) -> Option<Self> {
        let size = n * TRB_SIZE;
        let pages = (size + 4095) / 4096;
        let p = ctx.allocate_contiguous_frames(pages).ok()?;
        let v = ctx.phys_to_virt(p) as *mut Trb;
        let entries = unsafe { core::slice::from_raw_parts_mut(v, n) };

        // Initialise all TRBs to zero
        for e in entries.iter_mut() {
            e.params = [0; 8];
            e.status = 0;
            e.flags = 0;
        }

        // Set up the Link TRB at the last slot
        if n > 1 {
            let last = &mut entries[n - 1];
            last.flags = ((trb_type::LINK as u32) << trb_flag::TRB_TYPE_SHIFT)
                | trb_flag::TC   // toggle cycle on wrap
                | trb_flag::CYCLE; // initially valid
            last.params[..8].copy_from_slice(&p.to_le_bytes());
        }

        Some(Self {
            entries,
            phys: p,
            enq: 0,
            cycle: 1,
            len: n,
        })
    }

    /// Enqueue a TRB.
    ///
    /// The TRB's cycle bit is overwritten with the current producer cycle.
    /// When the enqueue index reaches `len - 1` (the Link TRB), the index
    /// wraps to 0 and the cycle bit toggles.
    pub fn enqueue(&mut self, mut trb: Trb) {
        trb.flags = (trb.flags & !trb_flag::CYCLE) | self.cycle;
        unsafe {
            ptr::write_volatile(&mut self.entries[self.enq], trb);
        }
        self.enq += 1;

        if self.enq >= self.len - 1 {
            // Wrap: update Link TRB's cycle, then loop back
            let link_idx = self.len - 1;
            unsafe {
                ptr::write_volatile(
                    &mut self.entries[link_idx].flags,
                    (self.entries[link_idx].flags & !trb_flag::CYCLE) | self.cycle,
                );
            }
            self.enq = 0;
            self.cycle ^= 1;
        }
    }

    /// Get the physical address of the next enqueue slot.
    pub fn enqueue_phys(&self) -> u64 {
        self.phys + (self.enq as u64 * TRB_SIZE as u64)
    }

    /// Number of usable slots (excluding the Link TRB).
    pub fn capacity(&self) -> usize {
        self.len.saturating_sub(1)
    }

    /// Current enqueue index.
    pub fn enq_index(&self) -> usize {
        self.enq
    }
}

// ============================================================================
//  CommandRing — specialised Ring for commands
// ============================================================================

/// The Command Ring is a [`Ring`] used to send commands to the controller
/// (Enable Slot, Address Device, Configure Endpoint, etc.).
pub struct CommandRing {
    pub ring: Ring,
}

impl CommandRing {
    pub fn alloc(ctx: &dyn DriverContext, n: usize) -> Option<Self> {
        Ring::alloc(ctx, n).map(|ring| Self { ring })
    }

    pub fn enqueue(&mut self, trb: Trb) {
        self.ring.enqueue(trb);
    }

    pub fn phys(&self) -> u64 {
        self.ring.phys
    }

    pub fn cycle(&self) -> u32 {
        self.ring.cycle
    }
}

// ============================================================================
//  EventRing — controller-to-driver event ring
// ============================================================================

/// The Event Ring receives completion events from the controller.
///
/// The driver dequeues events by reading TRBs that have been written
/// by the hardware. The cycle bit in each TRB is toggled by hardware
/// to indicate completion.
pub struct EventRing {
    entries: &'static mut [Trb],
    /// Physical address of the event ring.
    pub phys: u64,
    /// Next dequeue index.
    deq: usize,
    /// Consumer cycle state.
    cycle: u32,
    /// Number of TRB slots.
    len: usize,
}

impl EventRing {
    /// Allocate an event ring with `n` TRB slots.
    pub fn alloc(ctx: &dyn DriverContext, n: usize) -> Option<Self> {
        let size = n * TRB_SIZE;
        let pages = (size + 4095) / 4096;
        let p = ctx.allocate_contiguous_frames(pages).ok()?;
        let v = ctx.phys_to_virt(p) as *mut Trb;
        let entries = unsafe { core::slice::from_raw_parts_mut(v, n) };

        // Zero all entries (HC will write with cycle=1 initially)
        for e in entries.iter_mut() {
            e.params = [0; 8];
            e.status = 0;
            e.flags = 0;
        }

        Some(Self {
            entries,
            phys: p,
            deq: 0,
            cycle: 1,
            len: n,
        })
    }

    /// Free the event ring's physical memory.
    pub fn free(&self, ctx: &dyn DriverContext) {
        let size = self.len * TRB_SIZE;
        let pages = (size + 4095) / 4096;
        let _ = ctx.free_contiguous_frames(self.phys, pages);
    }

    /// Check if the next TRB is pending (cycle bit matches our expected cycle).
    pub fn has_pending(&self) -> bool {
        let flags = unsafe { ptr::read_volatile(&self.entries[self.deq].flags) };
        (flags & trb_flag::CYCLE) == self.cycle
    }

    /// Dequeue the next completed TRB, if any.
    pub fn pop(&mut self) -> Option<Trb> {
        if !self.has_pending() {
            return None;
        }
        let trb = unsafe { ptr::read_volatile(&self.entries[self.deq]) };
        self.deq += 1;
        if self.deq >= self.len {
            self.deq = 0;
            self.cycle ^= 1;
        }
        Some(trb)
    }

    /// Get the dequeue pointer value to write to ERDP (Event Ring Dequeue Pointer).
    ///
    /// Format (xHCI spec §5.5.2.3.3):
    /// - Bits 63:4 → physical address of next dequeue slot
    /// - Bit 3     → Event Handler Busy (EHB); set to 1 to clear interrupt pending
    /// - Bits 2:0  → DESI (Dequeue ERST Segment Index), 0 for single-segment ring
    ///
    /// Note: The cycle state is maintained in software only and should NOT be
    /// written to the ERDP register.
    pub fn dequeue_ptr(&self) -> u64 {
        (self.phys + (self.deq as u64 * TRB_SIZE as u64)) | (1 << 3)
    }

    /// Current dequeue index.
    pub fn deq_index(&self) -> usize {
        self.deq
    }
}

// ============================================================================
//  ERST — Event Ring Segment Table
// ============================================================================

/// Event Ring Segment Table entry (16 bytes, xHCI spec §5.5.2.3.1).
#[repr(C)]
pub struct ErstEntry {
    pub base_lo: u32,
    pub base_hi: u32,
    pub size: u32,
    pub rsvd: u32,
}

impl ErstEntry {
    /// Create an ERST entry pointing to the event ring.
    pub fn new(ring_phys: u64, segment_size: u32) -> Self {
        Self {
            base_lo: ring_phys as u32,
            base_hi: (ring_phys >> 32) as u32,
            size: segment_size,
            rsvd: 0,
        }
    }
}

// ============================================================================
//  RingContext — top-level ring container
// ============================================================================

/// All ring structures owned by the xHCI controller.
pub struct RingContext {
    /// Command Ring.
    pub command: CommandRing,
    /// Event Ring.
    pub event: EventRing,
}

impl RingContext {
    /// Allocate command and event rings.
    pub fn alloc(ctx: &dyn DriverContext, cmd_size: usize, evt_size: usize) -> Option<Self> {
        let command = CommandRing::alloc(ctx, cmd_size)?;
        let event = EventRing::alloc(ctx, evt_size)?;
        Some(Self { command, event })
    }
}

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trb_new() {
        let trb = Trb::new(trb_type::NORMAL, 1);
        assert_eq!(trb.flags & trb_flag::CYCLE, 1);
        assert_eq!(trb.trb_type(), trb_type::NORMAL);
    }

    #[test]
    fn test_trb_set_data_ptr() {
        let mut trb = Trb::new(trb_type::NORMAL, 1);
        trb.set_data_ptr(0xDEAD_BEEF_CAFE_BABE);
        let phys = u64::from_le_bytes([
            trb.params[0],
            trb.params[1],
            trb.params[2],
            trb.params[3],
            trb.params[4],
            trb.params[5],
            trb.params[6],
            trb.params[7],
        ]);
        assert_eq!(phys, 0xDEAD_BEEF_CAFE_BABE);
    }

    #[test]
    fn test_trb_completion_code() {
        let mut trb = Trb::new(trb_type::NORMAL, 1);
        trb.status = 0x01000000;
        assert_eq!(trb.completion_code(), 1);
    }
}
