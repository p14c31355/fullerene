//! EHCI Async Schedule — Queue Head (qH) and Queue Element Descriptor (qTD) pool.
//!
//! Manages the async schedule list: qH insertion/removal, qTD allocation/freeing,
//! and completion polling.
//!
//! # Data structures (EHCI spec §3.3–§3.6)
//!
//! | Structure  | Size     | Purpose                           |
//! |------------|----------|-----------------------------------|
//! | QueueHead  | 48 bytes | Endpoint state, links to qTDs     |
//! | Qtd        | 32 bytes | Transfer descriptor (data buffer) |

use crate::DriverContext;
use crate::usb::UsbSpeed;
use alloc::vec::Vec;
use core::ptr;

// ============================================================================
//  QueueHead (qH) — 48 bytes, 32-byte aligned
// ============================================================================

#[repr(C, align(32))]
pub struct QueueHead {
    pub horz_link: u32,
    pub ep_chars: u32,
    pub ep_caps: u32,
    pub current_qtd: u32,
    // Overlay area (9 dwords = 36 bytes)
    pub next_qtd: u32,
    pub alt_next_qtd: u32,
    pub token: u32,
    pub buf0: u32,
    pub buf1: u32,
    pub buf2: u32,
    pub buf3: u32,
    pub buf4: u32,
}

// ============================================================================
//  Queue Element Descriptor (qTD) — 32 bytes, 32-byte aligned
// ============================================================================

#[repr(C, align(32))]
pub struct Qtd {
    pub next_qtd: u32,
    pub alt_next_qtd: u32,
    pub token: u32,
    pub buf0: u32,
    pub buf1: u32,
    pub buf2: u32,
    pub buf3: u32,
    pub buf4: u32,
}

// ============================================================================
//  qH / qTD constant helpers
// ============================================================================

pub const QH_HORZ_TYPE_QH: u32 = 0x02; // bit 1 = 1 → qH

/// Build qH endpoint characteristics.
pub const fn qh_ep_chars(addr: u8, endpoint: u8, speed: UsbSpeed, mps: u16) -> u32 {
    let speed_bits = match speed {
        UsbSpeed::Full => 0u32,
        UsbSpeed::Low => 1u32,
        UsbSpeed::High => 2u32,
    };
    (addr as u32)
        | ((endpoint as u32) << 8)
        | (speed_bits << 12)
        | (1 << 14)  // DTC (Data Toggle Control)
        | ((mps as u32) << 16)  // MaxPacketLength (bits 16-26)
        | (8 << 28) // RL (NAK reload count, bits 28-31)
}

// ── qTD token fields ─────────────────────────────────────────
pub const QTD_ACTIVE: u32 = 1 << 7;
pub const QTD_HALTED: u32 = 1 << 6;
pub const QTD_PID_OUT: u32 = 0 << 8;
pub const QTD_PID_IN: u32 = 1 << 8;
pub const QTD_PID_SETUP: u32 = 2 << 8;
pub const QTD_CERR: u32 = 3 << 10; // 3 error counts
pub const QTD_TERMINATE: u32 = 0x01;

/// Build qTD total-bytes field.
pub const fn qtd_total_bytes(n: u32) -> u32 {
    if n == 0 {
        0x8000
    } else {
        (n << 16) & 0x7FFF_0000
    }
}

// ============================================================================
//  QueueHeadPool — manages a page of QueueHeads
// ============================================================================

/// Pre-allocated pool of QueueHeads (one 4KB page = 64 entries).
pub struct QueueHeadPool {
    entries: &'static mut [QueueHead],
    phys: u64,
    free: [usize; 64],
    free_len: usize,
}

impl QueueHeadPool {
    /// Allocate and zero a page for qH pool.
    pub fn alloc(ctx: &dyn DriverContext) -> Option<Self> {
        let phys = ctx.allocate_contiguous_frames(1).ok()?;
        let virt = ctx.phys_to_virt(phys) as *mut QueueHead;
        let entries = unsafe { core::slice::from_raw_parts_mut(virt, 64) };

        for q in entries.iter_mut() {
            unsafe {
                ptr::write_volatile(&mut q.horz_link, QTD_TERMINATE);
                ptr::write_volatile(&mut q.ep_chars, 0);
                ptr::write_volatile(&mut q.ep_caps, 0);
                ptr::write_volatile(&mut q.current_qtd, QTD_TERMINATE);
                ptr::write_volatile(&mut q.next_qtd, QTD_TERMINATE);
                ptr::write_volatile(&mut q.alt_next_qtd, QTD_TERMINATE);
                ptr::write_volatile(&mut q.token, 0);
            }
        }

        let mut free = [0usize; 64];
        for i in 0..64 {
            free[i] = 63 - i;
        }

        Some(Self {
            entries,
            phys,
            free,
            free_len: 64,
        })
    }

    /// Allocate a QueueHead. Returns (mutable ref, physical address).
    pub fn allocate(&mut self) -> Option<(&'static mut QueueHead, u64)> {
        if self.free_len == 0 {
            return None;
        }
        self.free_len -= 1;
        let idx = self.free[self.free_len];
        let ptr = &mut self.entries[idx] as *mut QueueHead;
        let phys = self.phys + (idx as u64) * core::mem::size_of::<QueueHead>() as u64;
        unsafe { Some((&mut *ptr, phys)) }
    }

    /// Free a QueueHead.
    pub fn free(&mut self, qh: &mut QueueHead) {
        let idx = ((qh as *mut QueueHead as usize) - (self.entries.as_ptr() as usize))
            / core::mem::size_of::<QueueHead>();
        if idx < 64 && self.free_len < 64 {
            self.free[self.free_len] = idx;
            self.free_len += 1;
        }
    }

    /// Reset the free list (clear all entries, zero pool).
    pub fn reset(&mut self) {
        self.free_len = 64;
        for i in 0..64 {
            self.free[i] = 63 - i;
        }
        for q in self.entries.iter_mut() {
            unsafe {
                ptr::write_volatile(&mut q.horz_link, QTD_TERMINATE);
                ptr::write_volatile(&mut q.ep_chars, 0);
                ptr::write_volatile(&mut q.ep_caps, 0);
                ptr::write_volatile(&mut q.current_qtd, QTD_TERMINATE);
                ptr::write_volatile(&mut q.next_qtd, QTD_TERMINATE);
                ptr::write_volatile(&mut q.alt_next_qtd, QTD_TERMINATE);
                ptr::write_volatile(&mut q.token, 0);
            }
        }
    }
}

// ============================================================================
//  QtdPool — manages a page of Qtds
// ============================================================================

/// Pre-allocated pool of QTDs (one 4KB page = 128 entries).
pub struct QtdPool {
    entries: &'static mut [Qtd],
    phys: u64,
    free: [usize; 128],
    free_len: usize,
}

impl QtdPool {
    /// Allocate and zero a page for qTD pool.
    /// Reserves slots 120-127 for control_transfer staging.
    pub fn alloc(ctx: &dyn DriverContext) -> Option<Self> {
        let phys = ctx.allocate_contiguous_frames(1).ok()?;
        let virt = ctx.phys_to_virt(phys) as *mut Qtd;
        let entries = unsafe { core::slice::from_raw_parts_mut(virt, 128) };

        for q in entries.iter_mut() {
            unsafe {
                ptr::write_volatile(&mut q.next_qtd, QTD_TERMINATE);
                ptr::write_volatile(&mut q.alt_next_qtd, QTD_TERMINATE);
                ptr::write_volatile(&mut q.token, 0);
                ptr::write_volatile(&mut q.buf0, QTD_TERMINATE);
                ptr::write_volatile(&mut q.buf1, QTD_TERMINATE);
                ptr::write_volatile(&mut q.buf2, QTD_TERMINATE);
                ptr::write_volatile(&mut q.buf3, QTD_TERMINATE);
                ptr::write_volatile(&mut q.buf4, QTD_TERMINATE);
            }
        }

        let mut free = [0usize; 128];
        let mut count = 0usize;
        for i in 0..120 {
            // Reserve slots 120-127 for control_transfer DMA buffer
            free[count] = 119 - i;
            count += 1;
        }
        Some(Self {
            entries,
            phys,
            free,
            free_len: count,
        })
    }

    /// Allocate a QTD. Returns (mutable ref, physical address).
    /// Clears all buffer pointer fields to prevent stale DMA.
    pub fn allocate(&mut self) -> Option<(&'static mut Qtd, u64)> {
        if self.free_len == 0 {
            return None;
        }
        self.free_len -= 1;
        let idx = self.free[self.free_len];
        let ptr = &mut self.entries[idx] as *mut Qtd;
        let phys = self.phys + (idx as u64) * 32;
        let q = unsafe { &mut *ptr };
        unsafe {
            ptr::write_volatile(&mut q.buf0, 0);
            ptr::write_volatile(&mut q.buf1, 0);
            ptr::write_volatile(&mut q.buf2, 0);
            ptr::write_volatile(&mut q.buf3, 0);
            ptr::write_volatile(&mut q.buf4, 0);
        }
        Some((q, phys))
    }

    /// Free a QTD.
    pub fn free(&mut self, qtd: &mut Qtd) {
        let idx = ((qtd as *mut Qtd as usize) - (self.entries.as_ptr() as usize))
            / core::mem::size_of::<Qtd>();
        if idx < 128 && self.free_len < 128 {
            self.free[self.free_len] = idx;
            self.free_len += 1;
        }
    }

    /// Get the physical address of reserved staging area (slots 120-127).
    pub fn staging_phys(&self) -> u64 {
        self.phys + 120 * 32
    }

    /// Reset the free list (restore all non-reserved slots).
    pub fn reset(&mut self) {
        self.free_len = 0;
        for i in 0..120 {
            self.free[self.free_len] = 119 - i;
            self.free_len += 1;
        }
        for q in self.entries.iter_mut() {
            unsafe {
                ptr::write_volatile(&mut q.next_qtd, QTD_TERMINATE);
                ptr::write_volatile(&mut q.alt_next_qtd, QTD_TERMINATE);
                ptr::write_volatile(&mut q.token, 0);
            }
        }
    }
}

// ============================================================================
//  AsyncSchedule — async list head + qH insert/remove
// ============================================================================

/// Manages the async schedule list (the circular qH list).
pub struct AsyncSchedule {
    /// Async list head qH (self-loop when idle).
    pub head: &'static mut QueueHead,
    /// Physical address of the async head.
    pub head_phys: u64,
}

impl AsyncSchedule {
    /// Allocate the async list head qH.
    pub fn alloc(ctx: &dyn DriverContext) -> Option<Self> {
        let phys = ctx.allocate_contiguous_frames(1).ok()?;
        let virt = ctx.phys_to_virt(phys) as *mut QueueHead;
        let head = unsafe { &mut *virt };

        // Self-loop: idle → head points to itself
        unsafe {
            ptr::write_volatile(&mut head.horz_link, (phys as u32) | QH_HORZ_TYPE_QH);
            ptr::write_volatile(&mut head.ep_chars, 0);
            ptr::write_volatile(&mut head.ep_caps, 0);
            ptr::write_volatile(&mut head.current_qtd, QTD_TERMINATE);
            ptr::write_volatile(&mut head.next_qtd, QTD_TERMINATE);
            ptr::write_volatile(&mut head.alt_next_qtd, QTD_TERMINATE);
            ptr::write_volatile(&mut head.token, 0);
        }

        Some(Self {
            head,
            head_phys: phys,
        })
    }

    /// Insert a qH into the async list (after the head).
    ///
    /// The list is circular: head → ... → qHx → ... → head.
    /// This inserts the new qH immediately after the head.
    pub fn insert(&mut self, qh_phys: u64, ctx: &dyn DriverContext) {
        let head_next = unsafe { ptr::read_volatile(&self.head.horz_link) };
        unsafe {
            ptr::write_volatile(&mut self.head.horz_link, (qh_phys as u32) | QH_HORZ_TYPE_QH);
        }
        let qh_virt = ctx.phys_to_virt(qh_phys) as *mut QueueHead;
        unsafe {
            ptr::write_volatile(&mut (*qh_virt).horz_link, head_next);
        }
    }

    /// Remove a qH from the async list.
    ///
    /// Walks the list from head to find the predecessor of `qh_phys`.
    pub fn remove(&mut self, qh_phys: u64, ctx: &dyn DriverContext) {
        let mut prev_phys = self.head_phys;
        for _ in 0..1024 {
            let prev_virt = ctx.phys_to_virt(prev_phys) as *mut QueueHead;
            let prev_qh = unsafe { &*prev_virt };
            let next_link = unsafe { ptr::read_volatile(&prev_qh.horz_link) };
            let next_phys = (next_link & !0x1F) as u64;
            if next_phys == 0 {
                break;
            }

            if next_phys == qh_phys {
                // Found it. Point prev to qh's next.
                let qh_virt = ctx.phys_to_virt(qh_phys) as *const QueueHead;
                let qh = unsafe { &*qh_virt };
                let qh_next = unsafe { ptr::read_volatile(&qh.horz_link) };
                unsafe {
                    ptr::write_volatile(&mut (*prev_virt).horz_link, qh_next);
                }
                return;
            }

            if next_phys == self.head_phys {
                break; // back to head → not found
            }
            prev_phys = next_phys;
        }
    }
}

// ============================================================================
//  TransferContext — qH/qTD pools + async schedule
// ============================================================================

/// Bundle of async schedule, qH pool, qTD pool.
pub struct TransferContext {
    pub schedule: AsyncSchedule,
    pub qh_pool: QueueHeadPool,
    pub qtd_pool: QtdPool,
}

impl TransferContext {
    /// Allocate all transfer-related structures.
    pub fn alloc(ctx: &dyn DriverContext) -> Option<Self> {
        let schedule = AsyncSchedule::alloc(ctx)?;
        let qh_pool = QueueHeadPool::alloc(ctx)?;
        let qtd_pool = QtdPool::alloc(ctx)?;
        Some(Self {
            schedule,
            qh_pool,
            qtd_pool,
        })
    }

    /// Wait for a qTD to complete (active bit cleared).
    pub fn wait_qtd(&self, qtd: &Qtd, timeout_us: u32) -> Result<(), &'static str> {
        for _ in 0..timeout_us {
            let token = unsafe { ptr::read_volatile(&qtd.token) };
            if token & QTD_ACTIVE == 0 {
                if token & QTD_HALTED != 0 {
                    return Err("qTD halted");
                }
                return Ok(());
            }
            crate::port::PortWriter::new(0x80).write_safe(0u8);
        }
        Err("qTD timeout")
    }

    /// Reset all pools (reclaim all qH and qTD resources).
    pub fn reset_pools(&mut self) {
        self.qh_pool.reset();
        self.qtd_pool.reset();
    }
}

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qh_ep_chars_high_speed() {
        let v = qh_ep_chars(1, 0, UsbSpeed::High, 64);
        assert!(v & (1 << 14) != 0); // DTC
        assert_eq!((v >> 12) & 3, 2); // High speed
    }

    #[test]
    fn test_qtd_total_bytes() {
        assert_eq!(qtd_total_bytes(8), 8 << 16);
        assert_eq!(qtd_total_bytes(0), 0x8000);
    }
}
