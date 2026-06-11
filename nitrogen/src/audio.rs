//! # Nitrogen Audio API
//!
//! Hardware‑agnostic audio abstractions for the Nitrogen hardware‑mechanism
//! layer.  This module defines the traits and data structures that audio
//! backends (HDA, VirtIO‑sound, AC'97, etc.) implement, plus a PCM mixer
//! and buffer queue that live above the backend.
//!
//! ## Layer model
//!
//! ```text
//! Application (Bad Apple, …)
//!  ↓
//! Nitrogen Audio API   ← this module
//!  ├ AudioDevice trait
//!  ├ PcmMixer
//!  └ BufferQueue
//!  ↓
//! Audio Backend        ← implemented in the kernel (e.g. sound.rs)
//!  ↓
//! Hardware (HDA, …)
//! ```
//!
//! ## Design decisions
//!
//! - `AudioDevice` is a trait with no heap allocation requirements.
//! - `PcmMixer` handles format conversion (sample rate, channels) in
//!   software, so backends only need to deal with their native format.
//! - `BufferQueue` is a lock‑free (SPSC) ring buffer for passing audio
//!   data from the application to the backend DMA loop.

use core::sync::atomic::{AtomicUsize, Ordering};

/// An abstract audio output device (HDA codec, VirtIO‑sound, etc.).
///
/// Implementors must provide the hardware‑specific DMA / codec
/// programming; everything above this trait works only in terms of
/// `AudioDevice`.
pub trait AudioDevice {
    /// Native sample rate of the hardware stream, in Hz (e.g. 48000).
    fn sample_rate(&self) -> u32;

    /// Number of channels the hardware stream expects (1 = mono, 2 = stereo).
    fn channels(&self) -> u8;

    /// Write interleaved PCM samples directly to the device.
    ///
    /// `samples` is a slice of `i16` samples.  The caller must ensure the
    /// number of samples matches `channels` × frame_count.
    ///
    /// Returns the number of **frames** successfully written (one frame =
    /// `channels` samples).  Implementations must be non‑blocking: if the
    /// hardware FIFO / DMA buffer is full, return 0 immediately.
    fn write_samples(&mut self, samples: &[i16]) -> usize;

    /// Number of PCM bytes the hardware has consumed since the stream
    /// started.  Used for A/V sync.
    ///
    /// Returns `None` if the device is not ready or the counter is
    /// unavailable.
    fn playback_progress_bytes(&self) -> Option<u64>;

    /// Push a slice of raw byte samples into the hardware half‑buffer.
    ///
    /// The default implementation converts `&[u8]` into `&[i16]` and
    /// delegates to `write_samples`.  Backends may override this if they
    /// have a more efficient direct byte path (e.g. DMA into a ring
    /// buffer that uses `&[u8]` natively).
    fn feed_bytes(&mut self, bytes: &[u8]) -> usize {
        let i16_size = core::mem::size_of::<i16>();
        let len = bytes.len() / i16_size;
        if len == 0 {
            return 0;
        }
        // SAFETY: `from_raw_parts` requires correct alignment for i16.
        // Reject unaligned input — callers must provide 2‑byte‑aligned
        // PCM data (e.g. static arrays or allocated buffers).
        let align = core::mem::align_of::<i16>();
        if bytes.as_ptr() as usize % align != 0 {
            return 0;
        }
        let samples =
            unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const i16, len) };
        let frames = self.write_samples(samples);
        frames * self.channels() as usize * i16_size
    }
}

/// A software PCM mixer that converts between arbitrary sample rates and
/// channel counts.
///
/// The mixer is stateless; each call to `mix` takes an input buffer, applies
/// nearest‑neighbour resampling (for simplicity; linear interpolation can be
/// added later), and writes the result into an output buffer.
pub struct PcmMixer {
    /// Output sample rate (Hz).
    out_rate: u32,
    /// Output channel count.
    out_channels: u8,
}

impl PcmMixer {
    /// Create a new mixer targeting the given output format.
    pub const fn new(out_rate: u32, out_channels: u8) -> Self {
        Self {
            out_rate,
            out_channels,
        }
    }

    /// Mix (resample + channel‑map) interleaved i16 PCM from an arbitrary
    /// input format into the mixer's output format.
    ///
    /// # Parameters
    ///
    /// - `input`: interleaved i16 samples.
    /// - `in_rate`: sample rate of `input` (Hz).
    /// - `in_channels`: channel count of `input`.
    /// - `output`: destination buffer.  Must have enough capacity; the
    ///   required size can be computed via `output_frames()`.
    ///
    /// # Returns
    ///
    /// Number of **output frames** actually written.
    pub fn mix(
        &self,
        input: &[i16],
        in_rate: u32,
        in_channels: u8,
        output: &mut [i16],
    ) -> usize {
        if in_rate == 0 || in_channels == 0 || self.out_channels == 0 || self.out_rate == 0 {
            return 0;
        }
        let in_frames = input.len() / in_channels as usize;
        if in_frames == 0 {
            return 0;
        }

        // Nearest‑neighbour resampling ratio.
        // For each output frame index j, input frame index =
        //   floor(j * in_rate / out_rate)
        let ratio_num = in_rate as u128;
        let ratio_den = self.out_rate as u128;

        let max_out_frames = output.len() / self.out_channels as usize;
        let mut out_frame = 0usize;

        while out_frame < max_out_frames {
            let in_frame =
                ((out_frame as u128 * ratio_num) / ratio_den) as usize;
            if in_frame >= in_frames {
                break;
            }

            let in_base = in_frame * in_channels as usize;
            let out_base = out_frame * self.out_channels as usize;

            // Channel mapping: if input has fewer channels, duplicate
            // the last one; if input has more, drop the extras.
            for ch in 0..self.out_channels {
                let src_ch = if (ch as u8) < in_channels {
                    ch as usize
                } else {
                    (in_channels - 1) as usize
                };
                output[out_base + ch as usize] = input[in_base + src_ch];
            }

            out_frame += 1;
        }

        out_frame
    }

    /// Compute the number of output frames that `input_frames` at
    /// the given input rate will produce.
    pub fn output_frames(&self, input_frames: usize, in_rate: u32) -> usize {
        if in_rate == 0 {
            return 0;
        }
        // ceil(input_frames * out_rate / in_rate)
        let num = input_frames as u128 * self.out_rate as u128;
        let den = in_rate as u128;
        ((num + den - 1) / den) as usize
    }

    /// Output sample rate.
    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    /// Output channel count.
    pub fn out_channels(&self) -> u8 {
        self.out_channels
    }
}

/// A lock‑free single‑producer single‑consumer ring buffer for PCM byte
/// data.
///
/// The producer (application / decoder) writes PCM bytes; the consumer
/// (DMA interrupt handler or polling loop) reads them and feeds the
/// hardware.
///
/// # Memory ordering
///
    /// - `write_head` / `read_tail` use `Relaxed` for the fast path plus a
    ///   single `Release`/`Acquire` fence on each side.
    pub struct BufferQueue<const N: usize> {
        // SAFETY: backed by UnsafeCell because the buffer is mutated
        // through shared references (&self) by both producer and consumer
        // via raw pointers (SPSC contract).  Sync is implemented manually
        // because the AtomicUsize heads provide sufficient synchronisation.
        buf: core::cell::UnsafeCell<[u8; N]>,
        /// Next write position (only written by producer).
        write_head: AtomicUsize,
        /// Next read position (only written by consumer).
        read_tail: AtomicUsize,
    }

    // SAFETY: BufferQueue is Sync because the SPSC contract ensures
    // mutually exclusive access to each logical region: the producer
    // only writes between write_head and read_tail (wrapping), and the
    // consumer only reads between read_tail and write_head.  The atomic
    // heads provide happens-before synchronisation.
    unsafe impl<const N: usize> Sync for BufferQueue<N> {}

    impl<const N: usize> BufferQueue<N> {
        /// Create an empty buffer queue.
        pub const fn new() -> Self {
            Self {
                buf: core::cell::UnsafeCell::new([0u8; N]),
            write_head: AtomicUsize::new(0),
            read_tail: AtomicUsize::new(0),
        }
    }

    /// Number of bytes currently available for reading.
    pub fn available(&self) -> usize {
        let w = self.write_head.load(Ordering::Acquire);
        let r = self.read_tail.load(Ordering::Relaxed);
        w.wrapping_sub(r)
    }

    /// Number of bytes of free space for writing.
    pub fn free_space(&self) -> usize {
        N - self.available()
    }

    /// Try to write `data` into the queue.
    ///
    /// Returns the number of bytes actually written (may be less than
    /// `data.len()` if the queue is nearly full).
    pub fn write(&self, data: &[u8]) -> usize {
        let w = self.write_head.load(Ordering::Relaxed);
        let r = self.read_tail.load(Ordering::Acquire);
        let used = w.wrapping_sub(r);
        let free = N - used;
        if free == 0 {
            return 0;
        }
        let n = data.len().min(free);
        let w_idx = w % N;
        let first_chunk = (N - w_idx).min(n);
        // SAFETY: we hold unique write access up to w+n.
        unsafe {
            let ptr = self.buf.get() as *mut u8;
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                ptr.add(w_idx),
                first_chunk,
            );
            if first_chunk < n {
                core::ptr::copy_nonoverlapping(
                    data.as_ptr().add(first_chunk),
                    ptr,
                    n - first_chunk,
                );
            }
        }
        self.write_head.store(w.wrapping_add(n), Ordering::Release);
        n
    }

    /// Try to read up to `dst.len()` bytes from the queue.
    ///
    /// Returns the number of bytes actually read.
    pub fn read(&self, dst: &mut [u8]) -> usize {
        let r = self.read_tail.load(Ordering::Relaxed);
        let w = self.write_head.load(Ordering::Acquire);
        let avail = w.wrapping_sub(r);
        if avail == 0 {
            return 0;
        }
        let n = dst.len().min(avail);
        let r_idx = r % N;
        let first_chunk = (N - r_idx).min(n);
        // SAFETY: we hold unique read access up to r+n.
        unsafe {
            let ptr = self.buf.get() as *mut u8;
            core::ptr::copy_nonoverlapping(
                ptr.add(r_idx),
                dst.as_mut_ptr(),
                first_chunk,
            );
            if first_chunk < n {
                core::ptr::copy_nonoverlapping(
                    ptr,
                    dst.as_mut_ptr().add(first_chunk),
                    n - first_chunk,
                );
            }
        }
        self.read_tail.store(r.wrapping_add(n), Ordering::Release);
        n
    }

    /// Reset the queue to empty (discards all data).
    ///
    /// Only safe to call when both producer and consumer are quiesced.
    pub fn reset(&self) {
        let w = self.write_head.load(Ordering::Relaxed);
        self.read_tail.store(w, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_queue_basic() {
        let q: BufferQueue<64> = BufferQueue::new();
        assert_eq!(q.available(), 0);
        assert_eq!(q.free_space(), 64);

        let n = q.write(b"hello");
        assert_eq!(n, 5);
        assert_eq!(q.available(), 5);

        let mut dst = [0u8; 10];
        let r = q.read(&mut dst);
        assert_eq!(r, 5);
        assert_eq!(&dst[..5], b"hello");
        assert_eq!(q.available(), 0);
    }

    #[test]
    fn buffer_queue_wrap() {
        let q: BufferQueue<8> = BufferQueue::new();
        q.write(b"abcd"); // w=4, r=0
        let mut tmp = [0u8; 4];
        q.read(&mut tmp); // w=4, r=4
        q.write(b"efghij"); // should write 4+2=6, wrap: "efgh"+"ij"→6 bytes
        let avail = q.available();
        assert!(
            avail == 6,
            "expected 6 available, got {}",
            avail
        );
        let mut dst = [0u8; 8];
        let r = q.read(&mut dst);
        assert_eq!(r, 6);
        assert_eq!(&dst[..6], b"efghij");
    }

    #[test]
    fn buffer_queue_full() {
        let q: BufferQueue<4> = BufferQueue::new();
        assert_eq!(q.write(b"abcd"), 4);
        assert_eq!(q.free_space(), 0);
        assert_eq!(q.write(b"x"), 0);
    }

    #[test]
    fn pcm_mixer_passthrough() {
        // 48000 Hz mono → 48000 Hz mono: identity
        let mixer = PcmMixer::new(48000, 1);
        let input: [i16; 4] = [100, 200, 300, 400];
        let mut output = [0i16; 8];
        let frames = mixer.mix(&input, 48000, 1, &mut output);
        assert_eq!(frames, 4);
        assert_eq!(&output[..4], &input[..]);
    }

    #[test]
    fn pcm_mixer_downsample() {
        // 96000 Hz mono → 48000 Hz mono: every other sample
        let mixer = PcmMixer::new(48000, 1);
        let input: [i16; 8] = [10, 20, 30, 40, 50, 60, 70, 80];
        let mut output = [0i16; 8];
        let frames = mixer.mix(&input, 96000, 1, &mut output);
        // rate / input rate = 48000/96000 = 0.5
        // out_frame=0 → in_frame=0 (10)
        // out_frame=1 → in_frame=1 (20) — no, nearest: 1*96000/48000=2 → 30
        // Actually: in_frame = floor(out_frame * 96000 / 48000)
        // out_frame 0 → 0
        // out_frame 1 → floor(1 * 96000/48000)=floor(2)=2 → 30
        // out_frame 2 → floor(2 * 96000/48000)=floor(4)=4 → 50
        // out_frame 3 → floor(3 * 96000/48000)=floor(6)=6 → 70
        assert_eq!(frames, 4);
        assert_eq!(output[0], 10); // frame 0
        assert_eq!(output[1], 30); // frame 2 → actually 30
        assert_eq!(output[2], 50);
        assert_eq!(output[3], 70);
    }

    #[test]
    fn pcm_mixer_channel_expand() {
        // 48000 Hz mono → 48000 Hz stereo: duplicate mono sample
        let mixer = PcmMixer::new(48000, 2);
        let input: [i16; 3] = [100, 200, 300];
        let mut output = [0i16; 8];
        let frames = mixer.mix(&input, 48000, 1, &mut output);
        assert_eq!(frames, 3);
        assert_eq!(output[0], 100);
        assert_eq!(output[1], 100);
        assert_eq!(output[2], 200);
        assert_eq!(output[3], 200);
        assert_eq!(output[4], 300);
        assert_eq!(output[5], 300);
    }
}