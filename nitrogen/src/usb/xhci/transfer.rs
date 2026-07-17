//! xHCI transfer operations — control and bulk transfers.
//!
//! Implements control transfer (setup/data/status stages on EP0) and
//! bulk transfer (IN/OUT on bulk endpoints).  All methods are on
//! [`XhciContext`] and are split into this module for organisational
//! clarity.

use core::ptr;

use super::context::XhciContext;
use super::ring::{COMP_SHORT_PACKET, COMP_SUCCESS, Trb, trb_flag, trb_type};
use crate::usb::{UsbDirection, UsbSetupPacket};

impl XhciContext {
    /// Perform a control transfer on EP0.
    pub fn control_transfer(
        &mut self,
        slot_id: u32,
        setup: &UsbSetupPacket,
        buf: &mut [u8],
    ) -> Result<usize, crate::DriverError> {
        let is_in = (setup.bm_request_type & 0x80) != 0;
        let data_len = setup.w_length as usize;
        if data_len > buf.len() {
            return Err(crate::DriverError::InvalidArgument);
        }

        let _ep0_cycle = {
            let slot = self
                .device
                .slots
                .get(slot_id)
                .ok_or(crate::DriverError::InvalidArgument)?;
            slot.ep0_ring.cycle
        };

        let staging_phys = if data_len > 0 {
            self.driver_ctx
                .allocate_contiguous_frames((data_len + 4095) / 4096)
                .map_err(|_| crate::DriverError::OutOfMemory)?
        } else {
            0
        };
        let staging_virt = if staging_phys != 0 {
            self.driver_ctx.phys_to_virt(staging_phys) as *mut u8
        } else {
            core::ptr::null_mut()
        };

        if data_len > 0 && !is_in {
            unsafe {
                ptr::copy_nonoverlapping(buf.as_ptr(), staging_virt, data_len);
            }
        }

        if let Some(slot) = self.device.slots.get_mut(slot_id) {
            let trt = if data_len == 0 {
                0
            } else if is_in {
                2 << 16
            } else {
                3 << 16
            };
            let mut s_trb = Trb::new(trb_type::SETUP_STAGE, slot.ep0_ring.cycle)
                .with_length(8)
                .with_flags(trb_flag::IDT);
            unsafe {
                core::ptr::copy_nonoverlapping(
                    setup as *const UsbSetupPacket as *const u8,
                    s_trb.params.as_mut_ptr(),
                    8,
                );
            }
            s_trb.flags |= trb_flag::CHAIN | trt;
            slot.ep0_ring.enqueue(s_trb);

            if data_len > 0 {
                let dir = if is_in { trb_flag::DIR_IN } else { 0 };
                slot.ep0_ring.enqueue(
                    Trb::new(trb_type::DATA_STAGE, slot.ep0_ring.cycle)
                        .with_data_ptr(staging_phys)
                        .with_length(data_len as u32)
                        .with_flags(dir | trb_flag::CHAIN),
                );
            }

            let st_dir = if !is_in || data_len == 0 {
                trb_flag::DIR_IN
            } else {
                0
            };
            slot.ep0_ring.enqueue(
                Trb::new(trb_type::STATUS_STAGE, slot.ep0_ring.cycle)
                    .with_flags(st_dir | trb_flag::IOC),
            );
        }

        crate::mmio::write_barrier();
        self.registers.doorbell.ring(slot_id, 1);
        let res = self.wait_event(5_000_000);

        let actual = res
            .as_ref()
            .ok()
            .filter(|ev| matches!(ev.completion_code(), COMP_SUCCESS | COMP_SHORT_PACKET))
            .map(|ev| data_len.saturating_sub((ev.remaining() as usize).min(data_len)));

        if let Some(actual) = actual.filter(|_| is_in && data_len > 0) {
            unsafe {
                ptr::copy_nonoverlapping(staging_virt, buf.as_mut_ptr(), actual);
            }
        }

        match (res, actual) {
            (Ok(_), Some(actual)) => {
                if staging_phys != 0 {
                    self.driver_ctx
                        .free_contiguous_frames(staging_phys, (data_len + 4095) / 4096);
                }
                Ok(actual)
            }
            _ => {
                if staging_phys != 0 {
                    self.deferred_free_list
                        .push((staging_phys, (data_len + 4095) / 4096));
                }
                Err(crate::DriverError::Protocol)
            }
        }
    }

    /// Perform a bulk transfer.
    pub fn bulk_transfer(
        &mut self,
        slot_id: u32,
        endpoint: u8,
        buf: &mut [u8],
        dir: UsbDirection,
        _mps: u16,
    ) -> Result<usize, crate::DriverError> {
        if buf.len() > 65536 {
            return Err(crate::DriverError::InvalidArgument);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let len = buf.len();

        {
            let slot = self
                .device
                .slots
                .get(slot_id)
                .ok_or(crate::DriverError::InvalidArgument)?;
            match dir {
                UsbDirection::In => {
                    let _ = slot
                        .bulk_in_ring
                        .as_ref()
                        .ok_or(crate::DriverError::NotReady)?;
                }
                UsbDirection::Out => {
                    let _ = slot
                        .bulk_out_ring
                        .as_ref()
                        .ok_or(crate::DriverError::NotReady)?;
                }
            }
        }

        let staging_pages = (len + 4095) / 4096;
        let staging_phys = self
            .driver_ctx
            .allocate_contiguous_frames(staging_pages)
            .map_err(|_| crate::DriverError::OutOfMemory)?;
        let staging_virt = self.driver_ctx.phys_to_virt(staging_phys) as *mut u8;

        if dir == UsbDirection::Out {
            unsafe {
                ptr::copy_nonoverlapping(buf.as_ptr(), staging_virt, len);
            }
        }

        let db_stream = {
            let slot = self.device.slots.get_mut(slot_id).unwrap();

            // Verify endpoint direction matches the requested direction
            let ep_is_in = (endpoint & 0x80) != 0;
            let dir_is_in = dir == UsbDirection::In;
            if ep_is_in != dir_is_in {
                self.driver_ctx
                    .free_contiguous_frames(staging_phys, staging_pages);
                return Err(crate::DriverError::InvalidArgument);
            }

            let ring = match dir {
                UsbDirection::In => slot.bulk_in_ring.as_mut().unwrap(),
                UsbDirection::Out => slot.bulk_out_ring.as_mut().unwrap(),
            };

            ring.enqueue(
                Trb::new(trb_type::NORMAL, ring.cycle)
                    .with_data_ptr(staging_phys)
                    .with_length(len as u32)
                    .with_flags(trb_flag::IOC),
            );

            let ep_num = (endpoint & 0x0F) as u32;
            ep_num * 2 + u32::from(ep_is_in)
        };

        crate::mmio::write_barrier();
        self.registers.doorbell.ring(slot_id, db_stream);
        let res = self.wait_event(5_000_000);

        match res {
            Ok(ev) => {
                if !matches!(ev.completion_code(), COMP_SUCCESS | COMP_SHORT_PACKET) {
                    self.deferred_free_list.push((staging_phys, staging_pages));
                    return Err(crate::DriverError::Protocol);
                }
                let remainder = ev.remaining() as usize;
                let xfer_len = len.saturating_sub(remainder.min(len));
                if dir == UsbDirection::In && xfer_len > 0 {
                    unsafe {
                        ptr::copy_nonoverlapping(staging_virt, buf.as_mut_ptr(), xfer_len);
                    }
                }
                self.driver_ctx
                    .free_contiguous_frames(staging_phys, staging_pages);
                Ok(xfer_len)
            }
            Err(_) => {
                self.deferred_free_list.push((staging_phys, staging_pages));
                Err(crate::DriverError::Protocol)
            }
        }
    }

    /// Get device descriptor (18 bytes).
    pub fn get_device_descriptor(
        &mut self,
        slot_id: u32,
    ) -> Result<crate::usb::UsbDeviceDescriptor, crate::DriverError> {
        let mut buf = [0u8; 18];
        let setup = UsbSetupPacket {
            bm_request_type: 0x80,
            b_request: crate::usb::REQ_GET_DESCRIPTOR,
            w_value: (crate::usb::DESC_DEVICE as u16) << 8,
            w_index: 0,
            w_length: 18,
        };
        self.control_transfer(slot_id, &setup, &mut buf)?;
        let desc =
            unsafe { ptr::read_unaligned(buf.as_ptr() as *const crate::usb::UsbDeviceDescriptor) };
        Ok(desc)
    }

    /// Set device address.
    pub fn set_address(&mut self, slot_id: u32, addr: u8) -> Result<(), crate::DriverError> {
        let setup = UsbSetupPacket {
            bm_request_type: 0x00,
            b_request: crate::usb::REQ_SET_ADDRESS,
            w_value: addr as u16,
            w_index: 0,
            w_length: 0,
        };
        self.control_transfer(slot_id, &setup, &mut [])?;
        Ok(())
    }

    /// Set device configuration.
    pub fn set_configuration(
        &mut self,
        slot_id: u32,
        config_value: u8,
    ) -> Result<(), crate::DriverError> {
        let setup = UsbSetupPacket {
            bm_request_type: 0x00,
            b_request: crate::usb::REQ_SET_CONFIGURATION,
            w_value: config_value as u16,
            w_index: 0,
            w_length: 0,
        };
        self.control_transfer(slot_id, &setup, &mut [])?;
        Ok(())
    }
}
