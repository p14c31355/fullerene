//! Host-command and transmit-ring handling for [`IwlWifiDevice`].

use crate::mmio;

use super::device::IwlWifiDevice;
use super::registers::*;
use super::types::*;

impl IwlWifiDevice {
    pub(super) fn send_hcmd(
        &mut self,
        opcode: u8,
        group: u8,
        data: &[u8],
    ) -> Result<(), crate::DriverError> {
        let total_len = core::mem::size_of::<HcmdHeader>() + data.len();
        if total_len > MAX_FRAME_SIZE {
            return Err(crate::DriverError::InvalidArgument);
        }

        self.health
            .pre_mmio_access()
            .map_err(|_| crate::DriverError::DeviceNotFound)?;
        let hcmd_header = HcmdHeader {
            opcode,
            group_id: group,
            length: data.len() as u16,
            flags: 0,
            reserved: 0,
        };

        let used = self.tx_head.wrapping_sub(self.tx_tail);
        if used >= TX_QUEUE_SIZE {
            return Err(crate::DriverError::Busy);
        }
        let desc_idx = self.tx_head % TX_QUEUE_SIZE;
        let desc_ptr = self.tx_dma_ring.virt() as *mut TxDmaDesc;
        let cmd_buf = &mut self.tx_bufs[desc_idx];
        let header_bytes = unsafe {
            core::slice::from_raw_parts(
                &hcmd_header as *const HcmdHeader as *const u8,
                core::mem::size_of::<HcmdHeader>(),
            )
        };
        let mut full_data = alloc::vec::Vec::with_capacity(total_len);
        full_data.extend_from_slice(header_bytes);
        full_data.extend_from_slice(data);
        cmd_buf.write_from(&full_data);

        let dma_addr = cmd_buf.dma_iova();
        let desc = unsafe { &mut *desc_ptr.add(desc_idx) };
        desc.addr_lo = dma_addr as u32;
        desc.addr_hi = (dma_addr >> 32) as u32;
        desc.len = total_len as u16;
        desc.flags = 0;
        mmio::cache_flush(desc as *const TxDmaDesc as *const u8);

        self.tx_head = self.tx_head.wrapping_add(1);
        mmio::write_barrier();
        unsafe {
            core::ptr::write_volatile(self.mmio.add(0x0BC / 4), self.tx_head as u32);
        }
        mmio::write_barrier();
        Ok(())
    }

    pub fn send_init_commands(&mut self) -> Result<(), crate::DriverError> {
        let ant_cfg: [u8; 8] = [0x03, 0x03, 0, 0, 0, 0, 0, 0];
        self.send_hcmd(
            LegacyCmd::TxAntConfig as u8,
            GroupId::Legacy as u8,
            &ant_cfg,
        )?;
        log::info!("iwlwifi: TX antenna config sent");

        let mut rxon = [0u8; 36];
        rxon[0] = 0x42;
        rxon[1] = 0x00;
        rxon[12..18].copy_from_slice(&self.mac);
        rxon[22] = 100;
        self.send_hcmd(LegacyCmd::Rxon as u8, GroupId::Legacy as u8, &rxon)?;
        log::info!("iwlwifi: RXON config sent");

        self.fw_state = FwState::Ready;
        log::info!("iwlwifi: init commands complete, device operational");
        Ok(())
    }

    pub fn send_raw_80211_frame(&mut self, frame: &[u8]) -> Result<(), crate::DriverError> {
        self.tx_queue.push_back(frame.to_vec());
        self.process_tx_queue();
        Ok(())
    }

    pub(super) fn process_tx_queue(&mut self) {
        if self.health.pre_mmio_access().is_err() {
            return;
        }

        while let Some(tx_frame) = self.tx_queue.front() {
            if tx_frame.len() > MAX_FRAME_SIZE {
                self.tx_queue.pop_front();
                continue;
            }
            if self.tx_head.wrapping_sub(self.tx_tail) >= TX_QUEUE_SIZE {
                break;
            }

            let tx_frame = self.tx_queue.pop_front().unwrap();
            let desc_idx = self.tx_head % TX_QUEUE_SIZE;
            let desc_ptr = self.tx_dma_ring.virt() as *mut TxDmaDesc;
            let buf = &mut self.tx_bufs[desc_idx];
            buf.write_from(&tx_frame);

            let dma_addr = buf.dma_iova();
            let desc = unsafe { &mut *desc_ptr.add(desc_idx) };
            desc.addr_lo = dma_addr as u32;
            desc.addr_hi = (dma_addr >> 32) as u32;
            desc.len = tx_frame.len() as u16;
            desc.flags = 0;
            mmio::cache_flush(desc as *const TxDmaDesc as *const u8);

            self.tx_head = self.tx_head.wrapping_add(1);
            mmio::write_barrier();
            unsafe {
                core::ptr::write_volatile(self.mmio.add(0x0BC / 4), self.tx_head as u32);
            }
            mmio::write_barrier();
        }
    }
}
