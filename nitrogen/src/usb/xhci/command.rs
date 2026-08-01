//! xHCI command-ring submission and device-slot configuration.

use super::context::XhciContext;
use super::interrupt::wait_command_completion;
use super::ring::{COMP_SUCCESS, Ring, Trb, trb_type};

impl XhciContext {
    /// Allocate a device slot.
    pub fn enable_slot(&mut self) -> Result<u32, crate::DriverError> {
        let trb = Trb::new(trb_type::ENABLE_SLOT, self.rings.command.cycle);
        let flags = self.send_cmd(trb)?;
        let slot_id = (flags >> 24) & 0xFF;
        let (slot_id, slot) = self.device.slots.alloc_slot(self.driver_ctx, slot_id)?;
        self.device.dcbaa.set_slot(slot_id, slot.dev_ctx_phys);
        log::info!(
            "xHCI: slot context published slot={} dcbaa={:#x} dcbaa_entry={:#x} output_ctx={:#x}",
            slot_id,
            self.device.dcbaa.phys,
            self.device.dcbaa.slot(slot_id),
            slot.dev_ctx_phys,
        );
        Ok(slot_id)
    }

    /// Address a device.
    pub fn address_device(
        &mut self,
        slot_id: u32,
        dev_idx: usize,
    ) -> Result<(), crate::DriverError> {
        let dev_addr = slot_id as u8;
        let port_index = self
            .devices
            .get(dev_idx)
            .ok_or(crate::DriverError::InvalidArgument)?
            .port_index;
        let root_port =
            u8::try_from(port_index + 1).map_err(|_| crate::DriverError::InvalidArgument)?;
        let speed_id = self.registers.op.portsc(port_index).speed() as u8;
        let (ep0_ring_phys, in_ctx_phys) = {
            let slot = self
                .device
                .slots
                .get(slot_id)
                .ok_or(crate::DriverError::InvalidArgument)?;
            (slot.ep0_ring.phys, slot.in_ctx_phys)
        };

        if let Some(in_ctx) = self.device.slots.input_ctx_mut(self.driver_ctx, slot_id) {
            in_ctx.setup_address_device(root_port, speed_id, ep0_ring_phys);
            log::info!(
                "xHCI: Address Device slot={} port={} speed={} in_ctx={:#x} add={:#x} slot0={:#010x} slot1={:#010x} ep0_1={:#010x} ep0_2={:#010x} ep0_3={:#010x} ep0_4={:#010x}",
                slot_id,
                root_port,
                speed_id,
                in_ctx_phys,
                in_ctx.add_flags,
                in_ctx.slot_ctx[0],
                in_ctx.slot_ctx[1],
                in_ctx.ep0_ctx()[1],
                in_ctx.ep0_ctx()[2],
                in_ctx.ep0_ctx()[3],
                in_ctx.ep0_ctx()[4],
            );
            crate::usb::dma::flush_range(
                in_ctx as *const _ as *const u8,
                core::mem::size_of::<super::device::InputContext>(),
            );
        }

        log::info!(
            "xHCI: Address Device submit slot={} dcbaap={:#x} dcbaa_entry={:#x} in_ctx={:#x} output_ctx={:#x}",
            slot_id,
            self.registers.op.dcbaap(),
            self.device.dcbaa.slot(slot_id),
            in_ctx_phys,
            self.device
                .slots
                .get(slot_id)
                .map(|slot| slot.dev_ctx_phys)
                .unwrap_or(0),
        );

        self.send_cmd(
            Trb::new(trb_type::ADDRESS_DEVICE, self.rings.command.cycle)
                .with_data_ptr(in_ctx_phys)
                .with_flags(slot_id << 24),
        )?;

        let dev_ctx_phys = self
            .device
            .slots
            .get(slot_id)
            .ok_or(crate::DriverError::InvalidArgument)?
            .dev_ctx_phys;
        let dev_ctx = self.driver_ctx.phys_to_virt(dev_ctx_phys) as *const u32;
        crate::mmio::cache_flush_range(dev_ctx as usize, 64);
        let out_slot0 = unsafe { core::ptr::read_volatile(dev_ctx) };
        let out_slot1 = unsafe { core::ptr::read_volatile(dev_ctx.add(1)) };
        let out_ep0_0 = unsafe { core::ptr::read_volatile(dev_ctx.add(8)) };
        let out_ep0_1 = unsafe { core::ptr::read_volatile(dev_ctx.add(9)) };
        let out_ep0_2 = unsafe { core::ptr::read_volatile(dev_ctx.add(10)) };
        log::info!(
            "xHCI: Address Device output slot={} ctx={:#x} slot0={:#010x} slot1={:#010x} ep0_0={:#010x} ep0_1={:#010x} ep0_2={:#010x}",
            slot_id,
            dev_ctx_phys,
            out_slot0,
            out_slot1,
            out_ep0_0,
            out_ep0_1,
            out_ep0_2,
        );

        if let Some(slot) = self.device.slots.get_mut(slot_id) {
            slot.dev_addr = dev_addr;
        }
        if let Some(device) = self.devices.get_mut(dev_idx) {
            device.address = dev_addr;
        }
        Ok(())
    }

    /// Configure a bulk endpoint.
    pub fn configure_endpoint_bulk(
        &mut self,
        slot_id: u32,
        ep_addr: u8,
        mps: u16,
    ) -> Result<(), crate::DriverError> {
        let ep_num = (ep_addr & 0x0F) as usize;
        let is_in = ep_addr & 0x80 != 0;
        let bulk_ring = Ring::alloc(self.driver_ctx, 64).ok_or(crate::DriverError::OutOfMemory)?;
        let bulk_ring_phys = bulk_ring.phys;
        let context_index = 2 * ep_num + usize::from(is_in);

        if let Some(in_ctx) = self.device.slots.input_ctx_mut(self.driver_ctx, slot_id) {
            in_ctx.setup_bulk_endpoint(context_index as u32, mps, bulk_ring_phys);
            crate::usb::dma::flush_range(
                in_ctx as *const _ as *const u8,
                core::mem::size_of::<super::device::InputContext>(),
            );
        }

        let in_ctx_phys = self
            .device
            .slots
            .get(slot_id)
            .ok_or(crate::DriverError::InvalidArgument)?
            .in_ctx_phys;
        let command = self.send_cmd(
            Trb::new(trb_type::CONFIGURE_ENDPOINT, self.rings.command.cycle)
                .with_data_ptr(in_ctx_phys)
                .with_flags(slot_id << 24),
        );
        if command.is_err() {
            bulk_ring.free(self.driver_ctx);
            return command.map(|_| ());
        }

        if let Some(slot) = self.device.slots.get_mut(slot_id) {
            if is_in {
                slot.bulk_in_ring = Some(bulk_ring);
            } else {
                slot.bulk_out_ring = Some(bulk_ring);
            }
        }
        Ok(())
    }

    /// Enqueue a command TRB and wait for its completion event.
    pub(super) fn send_cmd(&mut self, trb: Trb) -> Result<u32, crate::DriverError> {
        let command_type = trb.trb_type();
        let command_index = self.rings.command.enq_index();
        let command_phys = self.rings.command.phys + (command_index * super::ring::TRB_SIZE) as u64;
        self.rings.command.enqueue(trb);
        crate::mmio::write_barrier();
        self.registers.doorbell.ring(0, 0);
        let event = match wait_command_completion(
            &mut self.rings.event,
            &self.registers.runtime,
            5_000_000,
            command_phys,
        ) {
            Ok(event) => event,
            Err(error) => {
                let pending = self.rings.event.peek();
                log::warn!(
                    "xHCI: command timeout type={} cmd_phys={:#x} cmd_next={:#x} CRCR={:#x} USBSTS={:#x} USBCMD={:#x} IMAN={:#x} ERSTSZ={:#x} ERSTBA={:#x} ERDP={:#x} ev_phys={:#x} ev_flags={:#x} ev_type={} ev_cc={}",
                    command_type,
                    command_phys,
                    self.rings.command.enqueue_phys(),
                    self.registers.op.crcr(),
                    self.registers.op.usbsts(),
                    self.registers.op.usbcmd(),
                    self.registers.runtime.iman(),
                    self.registers.runtime.erstsz(),
                    self.registers.runtime.erstba(),
                    self.registers.runtime.erdp(),
                    self.rings.event.phys,
                    pending.flags,
                    pending.trb_type(),
                    pending.completion_code(),
                );
                return Err(error);
            }
        };
        if event.completion_code() != COMP_SUCCESS {
            log::warn!(
                "xHCI: command failed type={} slot={} completion={} flags={:#010x}",
                command_type,
                (event.flags >> 24) & 0xFF,
                event.completion_code(),
                event.flags,
            );
            return Err(crate::DriverError::Protocol);
        }
        log::info!(
            "xHCI: command complete type={} slot={} completion={} cmd_phys={:#x} event_cmd={:#x} flags={:#010x}",
            command_type,
            (event.flags >> 24) & 0xFF,
            event.completion_code(),
            command_phys,
            u64::from_le_bytes(event.params) & !0xF,
            event.flags,
        );
        Ok(event.flags)
    }
}
