//! xHCI slot release, deferred DMA cleanup, and controller teardown.

use alloc::vec::Vec;
use core::ptr;

use super::context::XhciContext;
use super::ring::{Trb, trb_type};

impl XhciContext {
    /// Release a single device slot and all resources owned by it.
    pub fn disable_slot(&mut self, slot_id: u32) {
        let _ = self.send_cmd(
            Trb::new(trb_type::DISABLE_SLOT, self.rings.command.cycle).with_flags(slot_id << 24),
        );
        self.device.dcbaa.clear_slot(slot_id);
        self.device.slots.release_slot(slot_id, self.driver_ctx);
        self.drain_deferred_free_list();
    }

    /// Release every active device slot.
    pub fn disable_all_slots(&mut self) {
        let slot_ids: Vec<u32> = self
            .device
            .slots
            .slots
            .iter()
            .map(|slot| slot.slot_id)
            .collect();
        for slot_id in slot_ids {
            let _ = self.send_cmd(
                Trb::new(trb_type::DISABLE_SLOT, self.rings.command.cycle)
                    .with_flags(slot_id << 24),
            );
            self.device.dcbaa.clear_slot(slot_id);
        }
        self.device.slots.release_all(self.driver_ctx);
        self.drain_deferred_free_list();
    }

    /// Release staging buffers after the controller no longer owns them.
    fn drain_deferred_free_list(&mut self) {
        for (phys, pages) in self.deferred_free_list.drain(..) {
            self.driver_ctx.free_contiguous_frames(phys, pages);
        }
    }
}

impl Drop for XhciContext {
    fn drop(&mut self) {
        self.disable_all_slots();
        self.rings.command.free(self.driver_ctx);
        self.rings.event.free(self.driver_ctx);
        self.driver_ctx
            .free_contiguous_frames(self.device.dcbaa.phys, 1);

        if let Some(erst_phys) = self.erst_phys {
            self.driver_ctx.free_contiguous_frames(erst_phys, 1);
        }

        if let Some(ref scratchpad) = self.device.scratchpad {
            let array_virt = self.driver_ctx.phys_to_virt(scratchpad.phys) as *const u64;
            for index in 0..scratchpad.count as usize {
                let buffer_phys = unsafe { ptr::read_volatile(array_virt.add(index)) };
                self.driver_ctx.free_contiguous_frames(buffer_phys, 1);
            }
            let array_pages = (scratchpad.count as usize * 8).div_ceil(4096);
            self.driver_ctx
                .free_contiguous_frames(scratchpad.phys, array_pages);
        }

        self.drain_deferred_free_list();
    }
}
