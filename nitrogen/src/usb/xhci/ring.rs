//! xHCI Ring structures — TRB, Transfer Ring, Event Ring.
//!
//! All ring-related data structures and operations are confined here.
//! Rings are circular queues of Transfer Request Blocks (TRBs) used
//! for command submission, transfer scheduling, and event reporting.

use crate::DriverContext;
use crate::usb::dma;
use core::ptr;

pub const TRB_SIZE: usize = 16;

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
    pub const DISABLE_SLOT: u8 = 13;
    pub const RESET_ENDPOINT: u8 = 14;
    pub const STOP_ENDPOINT: u8 = 15;
    pub const SET_TR_DEQUEUE: u8 = 16;
    pub const RESET_DEVICE: u8 = 17;
    pub const NO_OP: u8 = 23;
}

/// Command completion code for Success (xHCI spec §6.4.2.1, Table 6-93).
pub const COMP_SUCCESS: u8 = 1;

pub mod trb_flag {
    pub const CYCLE: u32 = 1 << 0;
    pub const TC: u32 = 1 << 1;
    pub const CHAIN: u32 = 1 << 4;
    pub const IOC: u32 = 1 << 5;
    pub const IDT: u32 = 1 << 6;
    pub const ENT: u32 = 1 << 11;
    pub const DIR_IN: u32 = 1 << 16;
    pub const TRB_TYPE_SHIFT: u32 = 10;
    pub const TRB_TYPE_MASK: u32 = 0x3F << TRB_TYPE_SHIFT;
}

// ══════════════════════════════════════════════════════════════
//  TRB
// ══════════════════════════════════════════════════════════════

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Trb {
    pub params: [u8; 8],
    pub status: u32,
    pub flags: u32,
}

impl Trb {
    pub fn new(trb_type: u8, cycle: u32) -> Self {
        Self { params: [0; 8], status: 0, flags: cycle | ((trb_type as u32) << trb_flag::TRB_TYPE_SHIFT) }
    }

    pub fn with_data_ptr(mut self, phys: u64) -> Self {
        self.params[..8].copy_from_slice(&phys.to_le_bytes());
        self
    }

    pub fn with_length(mut self, len: u32) -> Self {
        self.status = (self.status & !0x1FFFF) | (len & 0x1FFFF);
        self
    }

    pub fn with_flags(mut self, flags: u32) -> Self {
        self.flags |= flags;
        self
    }

    pub fn set_data_ptr(&mut self, phys: u64) {
        self.params[..8].copy_from_slice(&phys.to_le_bytes());
    }

    pub fn set_transfer_length(&mut self, len: u32) {
        self.status = (self.status & !0x1FFFF) | (len & 0x1FFFF);
    }

    pub fn trb_type(&self) -> u8 {
        ((self.flags & trb_flag::TRB_TYPE_MASK) >> trb_flag::TRB_TYPE_SHIFT) as u8
    }

    pub fn completion_code(&self) -> u8 {
        ((self.status >> 24) & 0xFF) as u8
    }

    pub fn remaining(&self) -> u32 {
        self.status & 0xFFFFFF
    }
}

// ══════════════════════════════════════════════════════════════
//  Common ring buffer allocation helper
// ══════════════════════════════════════════════════════════════

fn alloc_ring_slice(ctx: &dyn DriverContext, n: usize) -> Option<(&'static mut [Trb], u64)> {
    dma::alloc_dma::<Trb>(ctx, n)
}

// ══════════════════════════════════════════════════════════════
//  Ring  (transfer / command ring)
// ══════════════════════════════════════════════════════════════

pub struct Ring {
    entries: &'static mut [Trb],
    pub phys: u64,
    enq: usize,
    pub cycle: u32,
    pub len: usize,
}

impl Ring {
    pub fn alloc(ctx: &dyn DriverContext, n: usize) -> Option<Self> {
        let (entries, phys) = alloc_ring_slice(ctx, n)?;
        if n > 1 {
            let last = &mut entries[n - 1];
            last.flags = ((trb_type::LINK as u32) << trb_flag::TRB_TYPE_SHIFT) | trb_flag::TC | trb_flag::CYCLE;
            last.params[..8].copy_from_slice(&phys.to_le_bytes());
        }
        Some(Self { entries, phys, enq: 0, cycle: 1, len: n })
    }

    pub fn free(&self, ctx: &dyn DriverContext) {
        dma::free_dma(ctx, self.phys, (self.len * TRB_SIZE + 4095) / 4096);
    }

    pub fn enqueue(&mut self, mut trb: Trb) {
        trb.flags = (trb.flags & !trb_flag::CYCLE) | self.cycle;
        unsafe { ptr::write_volatile(&mut self.entries[self.enq], trb); }
        self.enq += 1;
        if self.enq >= self.len - 1 {
            let link = self.len - 1;
            unsafe {
                ptr::write_volatile(&mut self.entries[link].flags,
                    (self.entries[link].flags & !trb_flag::CYCLE) | self.cycle);
            }
            self.enq = 0;
            self.cycle ^= 1;
        }
    }

    pub fn enqueue_phys(&self) -> u64 {
        self.phys + self.enq as u64 * TRB_SIZE as u64
    }

    pub fn capacity(&self) -> usize {
        self.len.saturating_sub(1)
    }

    pub fn enq_index(&self) -> usize { self.enq }
}

// ══════════════════════════════════════════════════════════════
//  EventRing  (controller → driver)
// ══════════════════════════════════════════════════════════════

pub struct EventRing {
    entries: &'static mut [Trb],
    pub phys: u64,
    deq: usize,
    cycle: u32,
    pub len: usize,
}

impl EventRing {
    pub fn alloc(ctx: &dyn DriverContext, n: usize) -> Option<Self> {
        let (entries, phys) = alloc_ring_slice(ctx, n)?;
        Some(Self { entries, phys, deq: 0, cycle: 1, len: n })
    }

    pub fn free(&self, ctx: &dyn DriverContext) {
        dma::free_dma(ctx, self.phys, (self.len * TRB_SIZE + 4095) / 4096);
    }

    pub fn has_pending(&self) -> bool {
        (unsafe { ptr::read_volatile(&self.entries[self.deq].flags) } & trb_flag::CYCLE) == self.cycle
    }

    pub fn pop(&mut self) -> Option<Trb> {
        if !self.has_pending() { return None; }
        let trb = unsafe { ptr::read_volatile(&self.entries[self.deq]) };
        self.deq += 1;
        if self.deq >= self.len { self.deq = 0; self.cycle ^= 1; }
        Some(trb)
    }

    pub fn dequeue_ptr(&self) -> u64 {
        (self.phys + self.deq as u64 * TRB_SIZE as u64) | (1 << 3)
    }

    pub fn deq_index(&self) -> usize { self.deq }
}

// ══════════════════════════════════════════════════════════════
//  ERST
// ══════════════════════════════════════════════════════════════

#[repr(C)]
pub struct ErstEntry {
    pub base_lo: u32,
    pub base_hi: u32,
    pub size: u32,
    pub rsvd: u32,
}

impl ErstEntry {
    pub fn new(ring_phys: u64, segment_size: u32) -> Self {
        Self {
            base_lo: ring_phys as u32,
            base_hi: (ring_phys >> 32) as u32,
            size: segment_size,
            rsvd: 0,
        }
    }
}

// ══════════════════════════════════════════════════════════════
//  RingContext
// ══════════════════════════════════════════════════════════════

pub struct RingContext {
    pub command: Ring,
    pub event: EventRing,
}

impl RingContext {
    pub fn alloc(ctx: &dyn DriverContext, cmd_size: usize, evt_size: usize) -> Option<Self> {
        Some(Self { command: Ring::alloc(ctx, cmd_size)?, event: EventRing::alloc(ctx, evt_size)? })
    }
}

// ══════════════════════════════════════════════════════════════
//  Tests
// ══════════════════════════════════════════════════════════════

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
        let trb = Trb::new(trb_type::NORMAL, 1).with_data_ptr(0xDEAD_BEEF_CAFE_BABE);
        let phys = u64::from_le_bytes(trb.params);
        assert_eq!(phys, 0xDEAD_BEEF_CAFE_BABE);
    }

    #[test]
    fn test_trb_completion_code() {
        let mut trb = Trb::new(trb_type::NORMAL, 1);
        trb.status = 0x01000000;
        assert_eq!(trb.completion_code(), 1);
    }

    #[test]
    fn test_trb_with_flags() {
        let trb = Trb::new(trb_type::NORMAL, 1).with_flags(trb_flag::IOC);
        assert!(trb.flags & trb_flag::IOC != 0);
    }

    #[test]
    fn test_trb_with_length() {
        let trb = Trb::new(trb_type::NORMAL, 1).with_length(1024);
        assert_eq!(trb.status & 0x1FFFF, 1024);
    }
}
