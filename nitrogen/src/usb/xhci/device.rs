//! xHCI Device Context structures — DCBAA, Slot Manager, Input Context.
//!
//! Manages device slot lifecycle: slot allocation, device context pages,
//! input context setup for Address Device / Configure Endpoint commands.
//!
//! # Key structures
//!
//! | Structure        | Description                                         |
//! |------------------|-----------------------------------------------------|
//! | DCBAA            | Device Context Base Address Array (256 entries)     |
//! | Slot             | Per-slot state (transfer rings, context pointers)   |
//! | InputContext     | Input context page used for Address/Configure        |
//! | DeviceContext    | Output device context page (written by controller)   |
//! | Scratchpad       | Scratchpad buffer array for controller use           |
//! | SlotManager      | Manages all 1..MaxSlots-1 slots                     |

use super::ring::Ring;
use crate::DriverContext;
use crate::usb::dma;

use alloc::vec::Vec;
use core::ptr;

// ============================================================================
//  DCBAA — Device Context Base Address Array
// ============================================================================

/// DCBAA holds 64-bit physical addresses for up to 256 device contexts.
///
/// Index 0 is reserved (scratchpad array pointer or 0).
/// Indices 1..MaxSlots-1 hold pointers to per-slot Device Context pages.
pub struct Dcbaa {
    entries: &'static mut [u64; 256],
    /// Physical address of the DCBAA page.
    pub phys: u64,
}

impl Dcbaa {
    pub fn alloc(ctx: &dyn DriverContext) -> Option<Self> {
        let (virt, phys) = dma::alloc_dma_page(ctx)?;
        Some(Self {
            entries: unsafe { &mut *virt.cast::<[u64; 256]>() },
            phys,
        })
    }

    /// Write the device context pointer for a given slot.
    pub fn set_slot(&mut self, slot_id: u32, phys: u64) {
        if slot_id < 256 {
            self.entries[slot_id as usize] = phys;
        }
    }

    /// Clear the device context pointer for a given slot.
    pub fn clear_slot(&mut self, slot_id: u32) {
        if slot_id < 256 {
            self.entries[slot_id as usize] = 0;
        }
    }
}

// ============================================================================
//  Device Context (per-slot, written by controller)
// ============================================================================

/// Device context page for one slot (32 bytes per context, xHCI spec §6.2.3).
///
/// Each slot context + endpoint contexts are held in a single 4KB page.
/// We represent the slot context explicitly; endpoint contexts are accessed
/// via raw offset arithmetic.
#[repr(C, align(64))]
pub struct DeviceContext {
    /// Slot context — 8 dwords (32 bytes).
    pub slot: [u32; 8],
}

impl DeviceContext {
    pub fn alloc(ctx: &dyn DriverContext) -> Option<(*mut Self, u64)> {
        let (virt, phys) = dma::alloc_dma_page(ctx)?;
        Some((virt as *mut Self, phys))
    }
}

// ============================================================================
//  Input Context — used for Address Device / Configure Endpoint
// ============================================================================

/// Input Context page (xHCI spec §6.2.5.1).
///
/// Layout:
/// - Dword 0:     Drop Context Flags
/// - Dword 1:     Add Context Flags
/// - Dwords 2–7:  Reserved
/// - Dwords 8–15: Slot Context (32 bytes)
/// - Dwords 16+:  Endpoint Contexts (EP0 at dword 16, EP1 Out at 24, EP1 In at 32, ...)
///
/// Each endpoint context is 8 dwords (32 bytes).  Context indices follow
/// §6.2.3: EP<N> Out = 2*N, EP<N> In = 2*N + 1.
/// Input Context structure (1 page, 64-byte aligned).
///
/// Layout (xHCI §6.2.3):
/// - Dwords 0-1:   Drop/Add flags
/// - Dwords 2-7:   Reserved
/// - Dwords 8-15:  Slot Context (8 dwords)
/// - Dwords 16+:   Endpoint Contexts (EP1 Out at index 0, EP1 In at index 1, ...)
///
/// Each endpoint context is 8 dwords (32 bytes).  Context indices follow
/// §6.2.3: EP<N> Out = 2*N, EP<N> In = 2*N + 1.
/// We store 31 endpoint contexts (EP1..EP31) plus EP0 is at array index 0.
#[repr(C, align(64))]
pub struct InputContext {
    pub drop_flags: u32,
    pub add_flags: u32,
    _rsvd: [u32; 6],            // 6 dwords reserved = 24 bytes
    pub slot_ctx: [u32; 8],     // Slot context (8 dwords = 32 bytes)
    pub ep_ctx: [[u32; 8]; 31], // EP1 Out (=ctx_idx 2 → index 0) through EP31 In (=ctx_idx 63 → index 30)
}

impl InputContext {
    pub fn alloc(ctx: &dyn DriverContext) -> Option<(*mut Self, u64)> {
        let (virt, phys) = dma::alloc_dma_page(ctx)?;
        Some((virt as *mut Self, phys))
    }

    /// Get a mutable reference to an endpoint context by its context index.
    ///
    /// Context indices (per xHCI §6.2.3):
    /// - Index 0: Slot Context
    /// - Index 1: EP0 Context
    /// - Index 2*N: EP<N> Out
    /// - Index 2*N+1: EP<N> In
    pub fn ep_ctx_mut(&mut self, ctx_idx: u32) -> Option<&mut [u32; 8]> {
        if ctx_idx == 0 {
            return None; // Slot context, not an endpoint
        }
        // ctx_idx 1 (EP0) → ep_ctx[0], ctx_idx 2 → ep_ctx[1], etc.
        let ep_idx = (ctx_idx - 1) as usize;
        self.ep_ctx.get_mut(ep_idx)
    }

    /// Get a reference to the EP0 context (ctx_idx=1).
    pub fn ep0_ctx(&self) -> &[u32; 8] {
        &self.ep_ctx[0]
    }

    /// Get a mutable reference to the EP0 context (ctx_idx=1).
    pub fn ep0_ctx_mut(&mut self) -> &mut [u32; 8] {
        &mut self.ep_ctx[0]
    }

    /// Set up minimal Input Context for Address Device:
    /// - Add slot context (bit 0) + EP0 context (bit 1)
    /// - Set device address in slot context
    /// - Set EP0 context: MPS=64, type=Control (4), ring pointer
    pub fn setup_address_device(&mut self, dev_addr: u8, ep0_ring_phys: u64) {
        self.add_flags = 3; // add slot + EP0
        self.drop_flags = 0;
        self.slot_ctx[0] = 0; // route string = 0 (root port)
        self.slot_ctx[1] = (dev_addr as u32) << 24; // slot state: addressed
        // EP0 context: dword 1 contains MPS and EP Type
        self.ep_ctx[0][1] = (64 << 16) | (4 << 3); // MPS=64, type=Control(4)
        // TR Dequeue Pointer: dwords 2-3, with DCS bit in low bit of dword 2
        self.ep_ctx[0][2] = (ep0_ring_phys as u32) | 1; // TR Dequeue Pointer Low + DCS
        self.ep_ctx[0][3] = (ep0_ring_phys >> 32) as u32; // TR Dequeue Pointer High
    }
}

// ============================================================================
//  Slot — per-device-slot state
// ============================================================================

/// State for one device slot.
pub struct Slot {
    pub slot_id: u32,
    pub dev_addr: u8,
    /// EP0 transfer ring.
    pub ep0_ring: Ring,
    /// Bulk OUT transfer ring (optional, configured per endpoint).
    pub bulk_out_ring: Option<Ring>,
    /// Bulk IN transfer ring (optional).
    pub bulk_in_ring: Option<Ring>,
    /// Physical address of the output Device Context.
    pub dev_ctx_phys: u64,
    /// Physical address of the Input Context.
    pub in_ctx_phys: u64,
}

impl Slot {
    /// Create a new slot with allocated rings and context pages.
    pub fn new(
        ctx: &dyn DriverContext,
        slot_id: u32,
        dev_ctx_phys: u64,
        in_ctx_phys: u64,
    ) -> Option<Self> {
        let ep0_ring = Ring::alloc(ctx, 64)?;
        Some(Self {
            slot_id,
            dev_addr: 0,
            ep0_ring,
            bulk_out_ring: None,
            bulk_in_ring: None,
            dev_ctx_phys,
            in_ctx_phys,
        })
    }
}

// ============================================================================
//  Scratchpad — scratchpad buffer array
// ============================================================================

/// Scratchpad buffer array for controller-internal use (xHCI spec §4.20).
///
/// If HCSPARAMS2 reports max_scratchpad_bufs > 0, a scratchpad buffer
/// array must be allocated and its pointer stored in DCBAA[0].
pub struct Scratchpad {
    pub phys: u64, // physical address of the scratchpad array
    pub count: u32,
}

impl Scratchpad {
    pub fn alloc(ctx: &dyn DriverContext, count: u32) -> Option<Self> {
        if count == 0 {
            return None;
        }
        let mut dma = dma::alloc_dma::<u64>(ctx, count as usize)?;
        let phys = dma.phys;
        let pages = dma.pages;
        {
            let array = dma.as_mut();
            for i in 0..count as usize {
                let (_, buf_phys) = match dma::alloc_dma_page(ctx) {
                    Some(v) => v,
                    None => {
                        for j in 0..i {
                            let prev = unsafe { ptr::read_volatile(&array[j]) };
                            dma::free_dma_page(ctx, prev);
                        }
                        dma::free_dma(ctx, phys, pages);
                        return None;
                    }
                };
                unsafe {
                    ptr::write_volatile(&mut array[i], buf_phys);
                }
            }
        }
        Some(Self { phys, count })
    }
}

// ============================================================================
//  SlotManager — manages all device slots
// ============================================================================

/// Manages all device slots (slot IDs 1..MaxSlots-1).
pub struct SlotManager {
    /// All allocated slots.
    pub slots: Vec<Slot>,
    /// Maximum number of device slots supported.
    pub max_slots: u32,
    /// Number of slots currently in use.
    n_used: u32,
}

impl SlotManager {
    pub fn new(max_slots: u32) -> Self {
        Self {
            slots: Vec::new(),
            max_slots,
            n_used: 0,
        }
    }

    /// Find a slot by its slot ID.
    pub fn get(&self, slot_id: u32) -> Option<&Slot> {
        self.slots.iter().find(|s| s.slot_id == slot_id)
    }

    /// Find a mut slot by its slot ID.
    pub fn get_mut(&mut self, slot_id: u32) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.slot_id == slot_id)
    }

    /// Allocate a new slot, using the controller-assigned slot ID.
    pub fn alloc_slot(
        &mut self,
        ctx: &dyn DriverContext,
        slot_id: u32,
    ) -> Result<(u32, &mut Slot), &'static str> {
        if slot_id == 0 || slot_id > self.max_slots {
            return Err("invalid slot ID");
        }
        if self.n_used >= self.max_slots {
            return Err("no free slots");
        }
        self.n_used += 1;

        // Allocate device and input context pages
        let (_dev_ctx_virt, dev_ctx_phys) =
            DeviceContext::alloc(ctx).ok_or("no device ctx page")?;
        let (_in_ctx_virt, in_ctx_phys) = InputContext::alloc(ctx).ok_or("no input ctx page")?;

        let slot = Slot::new(ctx, slot_id, dev_ctx_phys, in_ctx_phys).ok_or("no slot resources")?;
        self.slots.push(slot);

        Ok((slot_id, self.slots.last_mut().unwrap()))
    }

    /// Release a single slot and free its resources.
    pub fn release_slot(&mut self, slot_id: u32, ctx: &dyn DriverContext) {
        if let Some(pos) = self.slots.iter().position(|s| s.slot_id == slot_id) {
            let slot = self.slots.remove(pos);
            ctx.free_contiguous_frames(slot.dev_ctx_phys, 1);
            ctx.free_contiguous_frames(slot.in_ctx_phys, 1);
            slot.ep0_ring.free(ctx);
            if let Some(ref ring) = slot.bulk_out_ring {
                ring.free(ctx);
            }
            if let Some(ref ring) = slot.bulk_in_ring {
                ring.free(ctx);
            }
            self.n_used = self.n_used.saturating_sub(1);
        }
    }

    /// Release all slots and free their resources.
    pub fn release_all(&mut self, ctx: &dyn DriverContext) {
        for slot in self.slots.drain(..) {
            ctx.free_contiguous_frames(slot.dev_ctx_phys, 1);
            ctx.free_contiguous_frames(slot.in_ctx_phys, 1);
            slot.ep0_ring.free(ctx);
            if let Some(ref ring) = slot.bulk_out_ring {
                ring.free(ctx);
            }
            if let Some(ref ring) = slot.bulk_in_ring {
                ring.free(ctx);
            }
        }
        self.n_used = 0;
    }

    /// Get the input context for a slot (by converting phys→virt).
    pub fn input_ctx_mut(
        &mut self,
        ctx: &dyn DriverContext,
        slot_id: u32,
    ) -> Option<&mut InputContext> {
        let slot = self.get(slot_id)?;
        let virt = ctx.phys_to_virt(slot.in_ctx_phys) as *mut InputContext;
        unsafe { Some(&mut *virt) }
    }
}

// ============================================================================
//  DeviceContextSet — DCBAA + Scratchpad + SlotManager
// ============================================================================

/// Top-level container for all device-related state.
pub struct DeviceContextSet {
    /// Device Context Base Address Array.
    pub dcbaa: Dcbaa,
    /// Scratchpad buffers (optional, may be None if not needed).
    pub scratchpad: Option<Scratchpad>,
    /// Slot manager.
    pub slots: SlotManager,
}

impl DeviceContextSet {
    /// Create a new device context set with DCBAA, optional scratchpad, and slot manager.
    pub fn new(ctx: &dyn DriverContext, max_slots: u32, scratchpad_count: u32) -> Option<Self> {
        let dcbaa = Dcbaa::alloc(ctx)?;
        let scratchpad = if scratchpad_count == 0 {
            None
        } else {
            Some(Scratchpad::alloc(ctx, scratchpad_count)?)
        };

        // DCBAA[0] = scratchpad array pointer (or 0 if none)
        if let Some(ref sp) = scratchpad {
            dcbaa.entries[0] = sp.phys;
        }

        let slots = SlotManager::new(max_slots);

        Some(Self {
            dcbaa,
            scratchpad,
            slots,
        })
    }
}

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_context_layout() {
        let layout = InputContext {
            drop_flags: 0,
            add_flags: 0,
            _rsvd: [0; 6],
            slot_ctx: [0; 8],
            ep_ctx: [[0; 8]; 31],
        };
        // EP0 context (index 0) is 8 dwords = 32 bytes
        assert_eq!(core::mem::size_of_val(&layout.ep_ctx[0]), 32);
    }

    #[test]
    fn test_slot_manager_max() {
        let mgr = SlotManager::new(1);
        // Cannot allocate in tests without a real DriverContext,
        // but we can check the limit.
        assert_eq!(mgr.max_slots, 1);
    }
}
