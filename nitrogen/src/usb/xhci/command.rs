//! xHCI command-ring submission and device-slot configuration.

use super::context::XhciContext;
use super::interrupt::wait_event_type;
use super::ring::{COMP_SUCCESS, Ring, Trb, trb_type};

impl XhciContext {
    /// Allocate a device slot.
    pub fn enable_slot(&mut self) -> Result<u32, crate::DriverError> {
        let trb = Trb::new(trb_type::ENABLE_SLOT, self.rings.command.cycle);
        let flags = self.send_cmd(trb)?;
        let slot_id = (flags >> 24) & 0xFF;
        let (slot_id, slot) = self.device.slots.alloc_slot(self.driver_ctx, slot_id)?;
        self.device.dcbaa.set_slot(slot_id, slot.dev_ctx_phys);
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
        }

        self.send_cmd(
            Trb::new(trb_type::ADDRESS_DEVICE, self.rings.command.cycle)
                .with_data_ptr(in_ctx_phys)
                .with_flags(slot_id << 24),
        )?;

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
        self.rings.command.enqueue(trb);
        crate::mmio::write_barrier();
        self.registers.doorbell.ring(0, 0);
        let event = wait_event_type(
            &mut self.rings.event,
            &self.registers.runtime,
            5_000_000,
            trb_type::COMMAND_COMPLETION_EVENT,
        )?;
        if event.completion_code() != COMP_SUCCESS {
            return Err(crate::DriverError::Protocol);
        }
        Ok(event.flags)
    }
}
