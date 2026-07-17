//! Fixed-capacity sector cache for filesystem block devices.

use alloc::vec;
use alloc::vec::Vec;

use super::{BlockDevice, BlockError};

pub struct BlockCache<D: BlockDevice> {
    inner: D,
    bytes_per_sector: usize,
    entries: Vec<(Option<u64>, Vec<u8>)>,
    next_victim: usize,
}

impl<D: BlockDevice> BlockCache<D> {
    pub fn new(inner: D, capacity: usize) -> Self {
        assert!(capacity > 0, "block cache capacity must be non-zero");
        let bytes_per_sector = inner.sector_size() as usize;
        let mut entries = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            entries.push((None, vec![0u8; bytes_per_sector]));
        }
        Self {
            inner,
            bytes_per_sector,
            entries,
            next_victim: 0,
        }
    }

    fn lookup(&self, lba: u64) -> Option<usize> {
        self.entries
            .iter()
            .position(|(entry, _)| *entry == Some(lba))
    }

    fn evict_slot(&mut self) -> usize {
        if let Some(index) = self.entries.iter().position(|(entry, _)| entry.is_none()) {
            return index;
        }
        let index = self.next_victim;
        self.next_victim = (self.next_victim + 1) % self.entries.len();
        index
    }

    pub fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        if buf.len() < self.bytes_per_sector {
            return Err(BlockError::BufferTooSmall {
                required: self.bytes_per_sector,
                provided: buf.len(),
            });
        }
        if lba >= self.inner.total_sectors() {
            return Err(BlockError::LbaOverflow);
        }
        if let Some(index) = self.lookup(lba) {
            buf[..self.bytes_per_sector].copy_from_slice(&self.entries[index].1);
            return Ok(());
        }

        let index = self.evict_slot();
        let entry = &mut self.entries[index];
        self.inner.read_sectors(lba, 1, &mut entry.1)?;
        entry.0 = Some(lba);
        buf[..self.bytes_per_sector].copy_from_slice(&entry.1);
        Ok(())
    }

    pub fn get_sector(&mut self, lba: u64) -> Result<&[u8], BlockError> {
        if lba >= self.inner.total_sectors() {
            return Err(BlockError::LbaOverflow);
        }
        if let Some(index) = self.lookup(lba) {
            return Ok(&self.entries[index].1);
        }

        let index = self.evict_slot();
        let entry = &mut self.entries[index];
        self.inner.read_sectors(lba, 1, &mut entry.1)?;
        entry.0 = Some(lba);
        Ok(&self.entries[index].1)
    }

    pub fn write_sector(&mut self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        if lba >= self.inner.total_sectors() {
            return Err(BlockError::LbaOverflow);
        }
        if buf.len() < self.bytes_per_sector {
            return Err(BlockError::BufferTooSmall {
                required: self.bytes_per_sector,
                provided: buf.len(),
            });
        }
        if let Some(index) = self.lookup(lba) {
            self.entries[index].0 = None;
        }
        self.inner.write_sectors(lba, 1, buf)
    }

    pub fn sector_size(&self) -> u32 {
        self.bytes_per_sector as u32
    }

    pub fn total_sectors(&self) -> u64 {
        self.inner.total_sectors()
    }
}

impl<D: BlockDevice> BlockDevice for BlockCache<D> {
    fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        let count = count as usize;
        let needed = count
            .checked_mul(self.bytes_per_sector)
            .ok_or(BlockError::LbaOverflow)?;
        if buf.len() < needed {
            return Err(BlockError::BufferTooSmall {
                required: needed,
                provided: buf.len(),
            });
        }
        let end_lba = lba
            .checked_add(count as u64)
            .ok_or(BlockError::LbaOverflow)?;
        if end_lba > self.inner.total_sectors() {
            return Err(BlockError::LbaOverflow);
        }

        let mut index = 0;
        while index < count {
            let current_lba = lba + index as u64;
            if let Some(slot) = self.lookup(current_lba) {
                let offset = index * self.bytes_per_sector;
                buf[offset..offset + self.bytes_per_sector].copy_from_slice(&self.entries[slot].1);
                index += 1;
                continue;
            }

            let first = index;
            while index < count && self.lookup(lba + index as u64).is_none() {
                index += 1;
            }
            let start = first * self.bytes_per_sector;
            let end = index * self.bytes_per_sector;
            self.inner.read_sectors(
                lba + first as u64,
                (index - first) as u16,
                &mut buf[start..end],
            )?;
            for sector in first..index {
                let slot = self.evict_slot();
                let offset = sector * self.bytes_per_sector;
                self.entries[slot].0 = Some(lba + sector as u64);
                self.entries[slot]
                    .1
                    .copy_from_slice(&buf[offset..offset + self.bytes_per_sector]);
            }
        }
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
        let count = count as usize;
        let needed = count
            .checked_mul(self.bytes_per_sector)
            .ok_or(BlockError::LbaOverflow)?;
        if buf.len() < needed {
            return Err(BlockError::BufferTooSmall {
                required: needed,
                provided: buf.len(),
            });
        }
        let end_lba = lba
            .checked_add(count as u64)
            .ok_or(BlockError::LbaOverflow)?;
        if end_lba > self.inner.total_sectors() {
            return Err(BlockError::LbaOverflow);
        }

        for index in 0..count {
            if let Some(slot) = self.lookup(lba + index as u64) {
                self.entries[slot].0 = None;
            }
        }
        self.inner.write_sectors(lba, count as u16, &buf[..needed])
    }

    fn sector_size(&self) -> u32 {
        self.bytes_per_sector as u32
    }

    fn total_sectors(&self) -> u64 {
        self.inner.total_sectors()
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use spin::Mutex;

    use super::*;

    const SECTOR_SIZE: usize = 16;

    #[derive(Clone)]
    struct MemoryBlockDevice {
        data: Arc<Mutex<Vec<u8>>>,
        reads: Arc<AtomicUsize>,
    }

    impl MemoryBlockDevice {
        fn new(sectors: usize) -> Self {
            let mut data = vec![0; sectors * SECTOR_SIZE];
            for (index, byte) in data.iter_mut().enumerate() {
                *byte = (index / SECTOR_SIZE) as u8;
            }
            Self {
                data: Arc::new(Mutex::new(data)),
                reads: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl BlockDevice for MemoryBlockDevice {
        fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
            let len = count as usize * SECTOR_SIZE;
            let start = lba as usize * SECTOR_SIZE;
            let end = start.checked_add(len).ok_or(BlockError::LbaOverflow)?;
            let data = self.data.lock();
            if end > data.len() {
                return Err(BlockError::LbaOverflow);
            }
            buf[..len].copy_from_slice(&data[start..end]);
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
            let len = count as usize * SECTOR_SIZE;
            let start = lba as usize * SECTOR_SIZE;
            let end = start.checked_add(len).ok_or(BlockError::LbaOverflow)?;
            let mut data = self.data.lock();
            if end > data.len() {
                return Err(BlockError::LbaOverflow);
            }
            data[start..end].copy_from_slice(&buf[..len]);
            Ok(())
        }

        fn sector_size(&self) -> u32 {
            SECTOR_SIZE as u32
        }

        fn total_sectors(&self) -> u64 {
            (self.data.lock().len() / SECTOR_SIZE) as u64
        }
    }

    #[test]
    fn repeated_read_hits_cache() {
        let device = MemoryBlockDevice::new(4);
        let reads = Arc::clone(&device.reads);
        let mut cache = BlockCache::new(device, 2);
        let mut first = [0; SECTOR_SIZE];
        let mut second = [0; SECTOR_SIZE];

        cache.read_sector(1, &mut first).unwrap();
        cache.read_sector(1, &mut second).unwrap();

        assert_eq!(first, second);
        assert_eq!(reads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn full_cache_evicts_in_round_robin_order() {
        let device = MemoryBlockDevice::new(4);
        let reads = Arc::clone(&device.reads);
        let mut cache = BlockCache::new(device, 2);
        let mut buf = [0; SECTOR_SIZE];

        cache.read_sector(0, &mut buf).unwrap();
        cache.read_sector(1, &mut buf).unwrap();
        cache.read_sector(2, &mut buf).unwrap();
        cache.read_sector(1, &mut buf).unwrap();
        cache.read_sector(0, &mut buf).unwrap();

        assert_eq!(reads.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn write_invalidates_cached_sector() {
        let device = MemoryBlockDevice::new(2);
        let reads = Arc::clone(&device.reads);
        let mut cache = BlockCache::new(device, 1);
        let mut buf = [0; SECTOR_SIZE];

        cache.read_sector(0, &mut buf).unwrap();
        cache.write_sector(0, &[9; SECTOR_SIZE]).unwrap();
        cache.read_sector(0, &mut buf).unwrap();

        assert_eq!(buf, [9; SECTOR_SIZE]);
        assert_eq!(reads.load(Ordering::Relaxed), 2);
    }
}
