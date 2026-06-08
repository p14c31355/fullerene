//! Bad Apple!! — shadow-art video + HDA PCM audio playback
use alloc::vec::Vec;
use spin::Mutex;

static BADAPPLE_RLE: &[u8] = include_bytes!("badapple.rle");
static BADAPPLE_PCM: &[u8] = include_bytes!("badapple.pcm");
const PCM_BYTES_PER_SEC: u32 = 44100;
const RLE_MAGIC: &[u8; 4] = b"BARL";
const RLE_HDR_SIZE: usize = 16;

fn calibrate_spins_per_ms() -> u64 {
    const FALLBACK: u64 = 400_000;
    unsafe { core::arch::x86_64::_mm_lfence(); }
    let tsc_start = unsafe { core::arch::x86_64::_rdtsc() };
    let mut spins = 0u64;
    loop {
        spins += 1; core::hint::spin_loop();
        if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(tsc_start) >= 250_000_000 { break; }
    }
    let per_ms = spins / 100;
    if per_ms == 0 { FALLBACK } else { per_ms }
}
fn delay_ms(s: u64, m: u64) { for _ in 0..m.saturating_mul(s) { core::hint::spin_loop(); } }
unsafe fn fb_write_u32(d: *mut u32, v: u32) { core::ptr::write_volatile(d, v); }

static RLE_BUF: Mutex<Option<Vec<u8>>> = Mutex::new(None);

fn draw_rle_frame(fb: *mut u32, fb_stride: usize, fb_h: usize, fw: u16, fh: u16, data: &[u8]) {
    let fw = fw as usize; let fh = fh as usize;
    if fw == 0 || fh == 0 || fb_stride == 0 || fb_h == 0 { return; }
    let total = fw * fh;
    let buf_ptr: *const u8 = {
        let mut g = RLE_BUF.lock();
        let buf = g.get_or_insert_with(|| alloc::vec![0u8; 19200]);
        buf.resize(total.max(19200), 0);
        let db = &mut buf[..total];
        let mut p = 0usize; let mut c = 0usize;
        while c + 3 <= data.len() && p < total {
            let fill = data[c];
            let rl = u16::from_le_bytes([data[c+1], data[c+2]]) as usize; c += 3;
            let end = (p + rl).min(total);
            db[p..end].fill(fill); p = end;
        }
        db.as_ptr()
    };
    let fw_u = fw as u32; let fh_u = fh as u32;
    let fs_u = fb_stride as u32; let fh_u32 = fb_h as u32;
    let decode = unsafe { core::slice::from_raw_parts(buf_ptr, total) };
    for fy in 0..fh_u32 {
        let ry = (fy * fh_u / fh_u32) as usize;
        let src = &decode[ry * fw..];
        let rb = (fy as usize) * fb_stride;
        for fx in 0..fs_u {
            let rx = (fx * fw_u / fs_u) as usize;
            let g = src[rx] as u32;
            unsafe { fb_write_u32(fb.add(rb + fx as usize), 0xFF000000 | (g<<16) | (g<<8) | g); }
        }
    }
}

pub fn play_badapple() {
    log::info!("Bad Apple playback started");
    let data = BADAPPLE_RLE;
    if data.len() < RLE_HDR_SIZE || &data[..4] != RLE_MAGIC { log::error!("Bad Apple: invalid RLE"); return; }
    let ver = u32::from_le_bytes([data[4],data[5],data[6],data[7]]);
    if ver != 1 { log::error!("Bad Apple: version {}", ver); return; }
    let fc = u32::from_le_bytes([data[8],data[9],data[10],data[11]]);
    let w = u16::from_le_bytes([data[12],data[13]]);
    let h = u16::from_le_bytes([data[14],data[15]]);
    if fc == 0 { log::error!("Bad Apple: zero frames"); return; }
    let n = fc as usize;
    let ds = RLE_HDR_SIZE + n * 2;
    if ds >= data.len() { log::error!("Bad Apple: RLE data exceeds file"); return; }
    let mut offs = Vec::with_capacity(n);
    let mut o: u64 = ds as u64;
    for i in 0..n {
        let cs = u16::from_le_bytes([data[RLE_HDR_SIZE+i*2], data[RLE_HDR_SIZE+i*2+1]]) as u64;
        offs.push(o); o = o.saturating_add(cs);
    }
    let (fb, fbs, fbh) = {
        let g = crate::graphics::PRIMARY_RENDERER.lock();
        let r = match g.as_ref() { Some(r) => r, None => { log::error!("Bad Apple: no renderer"); return; } };
        let i = r.get_info();
        (i.address as *mut u32, (i.stride as usize)/4, i.height as usize)
    };
    if fb.is_null() || fbs == 0 || fbh == 0 { log::error!("Bad Apple: invalid fb"); return; }
    let fb_len = fbs * fbh;
    for i in 0..fb_len { unsafe { fb_write_u32(fb.add(i), 0xFF000000); } }
    crate::graphics::flush_gpu();

    let spm = calibrate_spins_per_ms();
    let use_hda = crate::sound::hda_available();
    let pcm_total = BADAPPLE_PCM.len();
    let dur_ms = (pcm_total as u64 * 1000) / PCM_BYTES_PER_SEC as u64;
    let fims: u64 = dur_ms / (n as u64).max(1);
    const HALF: usize = 16368;
    log::info!("Bad Apple: {} frames, {:.1}s, {}ms/f, HDA={}", n, dur_ms as f64/1000.0, fims, use_hda);

    nitrogen::ps2::keyboard::flush_input();

    // Pre-fill both halves of the DMA ring buffer
    let mut pcm_off: usize = 0;
    if use_hda {
        let e0 = HALF.min(pcm_total);
        if e0 > 0 { crate::sound::hda_write_direct(0, &BADAPPLE_PCM[..e0]); pcm_off = e0; }
        let e1 = (pcm_off + HALF).min(pcm_total);
        if e1 > pcm_off { crate::sound::hda_write_direct(HALF as u32, &BADAPPLE_PCM[pcm_off..e1]); pcm_off = e1; }
    }

    let mut idx = 0usize;
    while idx < n && idx < offs.len() {
        if nitrogen::ps2::keyboard::input_available() { log::info!("Bad Apple aborted"); nitrogen::ps2::keyboard::flush_input(); break; }
        let fo = offs[idx] as usize;
        let no = if idx+1 < offs.len() { offs[idx+1] as usize } else { data.len() };
        if fo < data.len() && no <= data.len() {
            draw_rle_frame(fb, fbs, fbh, w, h, &data[fo..no]);
            crate::graphics::flush_gpu();
        }
        idx += 1;
        // PCM feed + frame delay: Spin + keep feeding as DMA advances past half-buffer boundaries.
        let deadline = fims.saturating_mul(spm);
        let mut waited = 0u64;
        while waited < deadline {
            if use_hda {
                let rem = pcm_total.saturating_sub(pcm_off);
                if rem > 0 {
                    let fe = (pcm_off + rem.min(HALF)).min(pcm_total);
                    let fed = crate::sound::hda_feed_samples(&BADAPPLE_PCM[pcm_off..fe]);
                    if fed > 0 { pcm_off += fed; }
                } else {
                    crate::sound::hda_feed_samples(&[0u8; HALF]);
                }
            }
            core::hint::spin_loop();
            waited += 1;
        }
    }

    // Drain remaining PCM
    if use_hda {
        while pcm_off < pcm_total {
            let rem = pcm_total - pcm_off;
            let fe = (pcm_off + rem.min(HALF)).min(pcm_total);
            loop {
                let fed = crate::sound::hda_feed_samples(&BADAPPLE_PCM[pcm_off..fe]);
                if fed > 0 { pcm_off += fed; break; }
                crate::sound::hda_poll();
            }
            delay_ms(spm, 50);
        }
        for _ in 0..10 {
            loop {
                if crate::sound::hda_feed_samples(&[0u8; HALF]) > 0 { break; }
                crate::sound::hda_poll();
            }
            delay_ms(spm, 100);
        }
    }

    solvent::force_desktop_redraw();
    log::info!("Bad Apple finished ({} frames)", idx);
}