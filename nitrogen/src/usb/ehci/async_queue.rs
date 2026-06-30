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
use crate::usb::dma;
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
        UsbSpeed::SuperSpeed => 2u32, // EHCI doesn't support SS; treat as High
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
macro_rules! dma_pool {
    ($name:ident, $ty:ty, $count:expr, $reserved:expr, $init:expr) => {
        pub struct $name {
            dma: dma::DmaSlice<$ty>,
            free: [usize; $count],
            free_len: usize,
        }

        impl $name {
            pub fn alloc(ctx: &dyn DriverContext) -> Option<Self> {
                let dma = dma::alloc_dma::<$ty>(ctx, $count)?;
                let init_fn = $init;
                for q in dma.as_mut().iter_mut() { init_fn(q); }
                let usable = $count - $reserved;
                let mut free = [0usize; $count];
                for i in 0..usable { free[i] = usable - 1 - i; }
                Some(Self { dma, free, free_len: usable })
            }

            pub fn allocate(&mut self) -> Option<(&'static mut $ty, u64)> {
                if self.free_len == 0 { return None; }
                self.free_len -= 1;
                let idx = self.free[self.free_len];
                let ptr = &mut self.dma.as_mut()[idx] as *mut $ty;
                let phys = self.dma.phys + (idx as u64) * core::mem::size_of::<$ty>() as u64;
                let init_fn = $init;
                unsafe { init_fn(&mut *ptr); }
                unsafe { Some((&mut *ptr, phys)) }
            }

            pub fn free(&mut self, item: &mut $ty) {
                let base = self.dma.as_mut().as_ptr() as usize;
                let idx = ((item as *mut $ty as usize) - base) / core::mem::size_of::<$ty>();
                if idx < $count && self.free_len < $count {
                    self.free[self.free_len] = idx;
                    self.free_len += 1;
                }
            }

            pub fn reset(&mut self) {
                self.free_len = $count - $reserved;
                for i in 0..self.free_len { self.free[i] = self.free_len - 1 - i; }
                let init_fn = $init;
                for q in self.dma.as_mut().iter_mut() { init_fn(q); }
            }
        }
    };
}

const fn qh_init(q: &mut QueueHead) {
    q.horz_link = QTD_TERMINATE;
    q.current_qtd = QTD_TERMINATE;
    q.next_qtd = QTD_TERMINATE;
    q.alt_next_qtd = QTD_TERMINATE;
}

const fn qtd_init(q: &mut Qtd) {
    q.next_qtd = QTD_TERMINATE;
    q.alt_next_qtd = QTD_TERMINATE;
    q.buf0 = QTD_TERMINATE;
    q.buf1 = QTD_TERMINATE;
    q.buf2 = QTD_TERMINATE;
    q.buf3 = QTD_TERMINATE;
    q.buf4 = QTD_TERMINATE;
}

dma_pool!(QueueHeadPool, QueueHead, 64, 0, qh_init);
dma_pool!(QtdPool, Qtd, 128, 8, qtd_init);

impl QtdPool {
    pub fn staging_phys(&self) -> u64 { self.dma.phys + 120 * core::mem::size_of::<Qtd>() as u64 }
}

// ============================================================================
//  AsyncSchedule — async list head + qH insert/remove
// ============================================================================

/// Manages the async schedule list (the circular qH list).
pub struct AsyncSchedule {
    /// Keeps the DMA allocation alive for the lifetime of the schedule.
    _dma: dma::DmaSlice<QueueHead>,
    /// Async list head qH (self-loop when idle).
    pub head: &'static mut QueueHead,
    /// Physical address of the async head.
    pub head_phys: u64,
}

impl AsyncSchedule {
    pub fn alloc(ctx: &dyn DriverContext) -> Option<Self> {
        let dma = dma::alloc_dma::<QueueHead>(ctx, 1)?;
        let phys = dma.phys;
        let head = unsafe { &mut *dma.as_mut_ptr() };
        qh_init(head);
        unsafe {
            ptr::write_volatile(&mut head.horz_link, (phys as u32) | QH_HORZ_TYPE_QH);
            ptr::write_volatile(&mut head.ep_chars, 1 << 15);
        }
        Some(Self { _dma: dma, head, head_phys: phys })
    }

    /// Insert a qH into the async list (after the head).
    ///
    /// The list is circular: head → ... → qHx → ... → head.
    /// This inserts the new qH immediately after the head.
    /// Per EHCI spec §4.8.2: the new qH's horz_link must be set BEFORE
    /// modifying the predecessor's link, to prevent the HC from following
    /// a broken link during async schedule traversal.
    pub fn insert(&mut self, qh_phys: u64, ctx: &dyn DriverContext) {
        let head_next = unsafe { ptr::read_volatile(&self.head.horz_link) };
        let qh_virt = ctx.phys_to_virt(qh_phys) as *mut QueueHead;
        unsafe {
            ptr::write_volatile(&mut (*qh_virt).horz_link, head_next);
        }
        unsafe {
            ptr::write_volatile(&mut self.head.horz_link, (qh_phys as u32) | QH_HORZ_TYPE_QH);
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
            core::hint::spin_loop();
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
