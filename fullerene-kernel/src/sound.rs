//! Sound / Audio subsystem for Fullerene OS.
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use nitrogen::pci::PciDevice;
use spin::Mutex;

pub fn pc_speaker_on(frequency_hz: u32) {
    if frequency_hz == 0 { pc_speaker_off(); return; }
    let divisor = (1_193_182u32 / frequency_hz).min(65535) as u16;
    unsafe {
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x43).write(0xB6);
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x42).write(divisor as u8);
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x42).write((divisor>>8) as u8);
        let t = x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read();
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(t|0x03);
    }
}
pub fn pc_speaker_off() {
    unsafe {
        let t = x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read();
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(t&!0x03);
    }
}

const GCAP: usize=0x0000; const GCTL: usize=0x0008; const STATESTS: usize=0x000E; const INTCTL: usize=0x0020;
const CORBLBASE: usize=0x0040; const CORBUBASE: usize=0x0044; const CORBWP: usize=0x0048; const CORBRP: usize=0x004A;
const CORBCTL: usize=0x004C; const RIRBLBASE: usize=0x0050; const RIRBUBASE: usize=0x0054; const RIRBWP: usize=0x0058;
const RIRBCTL: usize=0x005C; const SD_BASE: usize=0x0080; const SD_SIZE: usize=0x0020; const SD_CTL: usize=0x00;
const SD_STS: usize=0x03; const SD_LPIB: usize=0x04; const SD_CBL: usize=0x08; const SD_LVI: usize=0x0C;
const SD_FMT: usize=0x12; const SD_BDPL: usize=0x18; const SD_BDPU: usize=0x1C;
const VERB_GET_PARAM: u32=0xF00; const VERB_SET_FMT: u32=0x002; const VERB_SET_AMP_GAIN_MUTE: u32=0x003;
const VERB_SET_PIN_CTL: u32=0x707; const VERB_SET_STREAM: u32=0x706; const VERB_SET_EAPD: u32=0x70C;
const PARAM_SUBORDINATE_COUNT: u8=0x04; const PARAM_AUDIO_WIDGET_CAP: u8=0x09; const PARAM_OUTPUT_AMP_CAP: u8=0x12;
const PARAM_PIN_CAP: u8=0x0C; const WTYPE_AUDIO_OUTPUT: u32=0x0; const WTYPE_PIN_COMPLEX: u32=0x4;
const WTYPE_AFG: u32=0x1; const CORB_ENTRIES: usize=256; const RIRB_ENTRIES: usize=256;

#[repr(C)] struct BdlEntry { addr_lo: u32, addr_hi: u32, length: u32, flags: u32 }
const DMA_BUF_SIZE: u32=32768; const BDL_ENTRIES: u32=2;

static HDA_PHYS: Mutex<u64>=Mutex::new(0); static HDA_READY: AtomicBool=AtomicBool::new(false);
static HDA_VIRT: Mutex<usize>=Mutex::new(0); static HDA_DMA: Mutex<usize>=Mutex::new(0);
static HDA_AUDIO_OFF: Mutex<u32>=Mutex::new(0); static HDA_AUDIO_SZ: Mutex<u32>=Mutex::new(0);
static HDA_HALF: Mutex<u32>=Mutex::new(0); static HDA_SD: Mutex<usize>=Mutex::new(0);
static HDA_LAST_LPIB: AtomicU64=AtomicU64::new(u64::MAX); static HDA_CORB_V: Mutex<usize>=Mutex::new(0);
static HDA_RIRB_V: Mutex<usize>=Mutex::new(0); static HDA_INIT_DONE: AtomicBool=AtomicBool::new(false);
/// Actual CORB entry count (derived from GCAP CORBSZCAP; 2, 16, or 256).
/// Used by `corb_send_verb` for circular‑buffer wrap.
static HDA_CORB_ENTRIES: Mutex<usize> = Mutex::new(256);

unsafe fn r32(m:*mut u8, o:usize)->u32{core::ptr::read_volatile(m.add(o) as *const u32)}
unsafe fn w32(m:*mut u8, o:usize, v:u32){core::ptr::write_volatile(m.add(o) as *mut u32, v)}
unsafe fn r16(m:*mut u8, o:usize)->u16{core::ptr::read_volatile(m.add(o) as *const u16)}
unsafe fn w16(m:*mut u8, o:usize, v:u16){core::ptr::write_volatile(m.add(o) as *mut u16, v)}
unsafe fn r8(m:*mut u8, o:usize)->u8{core::ptr::read_volatile(m.add(o))}
unsafe fn w8(m:*mut u8, o:usize, v:u8){core::ptr::write_volatile(m.add(o), v)}

fn alloc_dma_pages(pages:usize)->Option<(u64,*mut u8)>{
    let off=petroleum::common::memory::get_physical_memory_offset() as u64;
    let phys=match petroleum::page_table::constants::get_frame_allocator_mut().allocate_contiguous_frames(pages)
    {Ok(a)=>a,Err(_)=>{log::error!("Sound: DMA alloc fail"); return None}};
    let virt=(phys+off) as *mut u8; unsafe{core::ptr::write_bytes(virt,0,pages*4096);} Some((phys,virt))
}

/// Probe for HDA controller across all PCI buses.
///
/// On real hardware (InsydeH2O) the HDA controller may reside on a bus other
/// than 0, so we iterate bus 0..=255, skipping buses that don't exist.
fn probe_hda()->Option<(u8,u8,u8,u64)>{
    use nitrogen::pci::PciConfigSpace;
    /// Check whether a PCI bus exists by probing device 0 function 0.
    fn bus_exists(bus:u8)->bool{
        PciConfigSpace::read_config_word(bus,0,0,0)!=0xFFFF
    }
    for bus in 0..=255u8{
        if bus>0&&!bus_exists(bus){continue}
        for d in 0..=31u8{
            let Some(dev)=PciDevice::new(bus,d,0) else{continue};
            if dev.class_code!=0x04||dev.subclass!=0x03{continue}
            let bar0=dev.read_bar(0)?; dev.enable_memory_access();
            let Some(mut cfg)=PciConfigSpace::read_from_device(bus,d,0) else{continue};
            cfg.command|=0x0004; let v=(cfg.status as u32)<<16|(cfg.command as u32);
            PciConfigSpace::write_config_dword(&mut cfg,bus,d,0,0x04,v);
            log::info!("Sound: HDA found at {:04x}:{:02x}.{}, MMIO=0x{:x}",bus,d,0,bar0);
            return Some((bus,d,0,bar0));
        }
    }None
}

pub fn init(){match probe_hda(){
    Some((bus,dev,func,mmio))=>{log::info!("Sound: HDA at {:04x}:{:02x}.{}, MMIO=0x{:x}",bus,dev,func,mmio); *HDA_PHYS.lock()=mmio;}
    None=>log::info!("Sound: No HDA (PC speaker only)"),
}}

unsafe fn corb_send_verb(mmio:*mut u8,codec:u8,node:u8,verb:u32,payload:u8)->u32{
    let corb_v=*HDA_CORB_V.lock(); let rirb_v=*HDA_RIRB_V.lock();
    if corb_v==0||rirb_v==0{return 0xFFFF_FFFF;}
    let corb_n=*HDA_CORB_ENTRIES.lock();
    let corb=corb_v as *mut u32; let rirb=rirb_v as *mut u64;
    let cmd=((codec as u32)<<28)|((node as u32)<<20)|(verb<<8)|(payload as u32);
    for _ in 0..1000{let wp=r16(mmio,CORBWP) as usize; let rp=r16(mmio,CORBRP) as usize&0xFF;
        if (wp+1)%corb_n!=rp{break} core::hint::spin_loop();}
    let wp=r16(mmio,CORBWP) as usize; let next_wp=(wp+1)%corb_n;
    core::ptr::write_volatile(corb.add(next_wp),cmd); w16(mmio,CORBWP,next_wp as u16);
    let rirb_wp_before=r16(mmio,RIRBWP)&0xFF;
    for _ in 0..50_000{let rirb_wp=r16(mmio,RIRBWP)&0xFF; if rirb_wp!=rirb_wp_before{
        let resp=core::ptr::read_volatile(rirb.add(rirb_wp as usize));
        if (resp>>63)&1==0{return (resp>>32) as u32;}} core::hint::spin_loop();}
    log::warn!("Sound: verb timeout c={} n={:#x} v={:#03x}",codec,node,verb); 0xFFFF_FFFF
}

unsafe fn discover_codec(mmio:*mut u8,codec:u8)->Option<(u8,u8)>{
    let sub=corb_send_verb(mmio,codec,0,VERB_GET_PARAM,PARAM_SUBORDINATE_COUNT);
    if sub==0xFFFF_FFFF{return None;} let start=((sub>>16)&0xFF) as u8; let count=(sub&0xFF) as u8;
    if count==0{return None;} let end=start+count-1; log::info!("Sound: root children {}-{}",start,end);
    let mut afg:Option<u8>=None;
    for n in start..=end{let cap=corb_send_verb(mmio,codec,n,VERB_GET_PARAM,PARAM_AUDIO_WIDGET_CAP);
        if cap==0xFFFF_FFFF{continue} if (cap>>20)&0xF==WTYPE_AFG{afg=Some(n); log::info!("Sound: AFG node {}",n); break;}}
    let afg=afg?; let sub=corb_send_verb(mmio,codec,afg,VERB_GET_PARAM,PARAM_SUBORDINATE_COUNT);
    if sub==0xFFFF_FFFF{return None;} let start=((sub>>16)&0xFF) as u8; let count=(sub&0xFF) as u8;
    if count==0{return None;} let end=start+count-1; log::info!("Sound: AFG children {}-{}",start,end);
    let mut dac:Option<u8>=None; let mut pin:Option<u8>=None;
    for n in start..=end{let cap=corb_send_verb(mmio,codec,n,VERB_GET_PARAM,PARAM_AUDIO_WIDGET_CAP);
        if cap==0xFFFF_FFFF{continue} let t=(cap>>20)&0xF;
        if t==WTYPE_AUDIO_OUTPUT&&dac.is_none(){dac=Some(n);}
        if t==WTYPE_PIN_COMPLEX&&pin.is_none(){pin=Some(n);}}
    match(dac,pin){(Some(d),Some(p))=>Some((d,p)),_=>None}
}

unsafe fn configure_codec(mmio:*mut u8,codec:u8,dac:u8,pin:u8,stream:u8){
    let ac=corb_send_verb(mmio,codec,dac,VERB_GET_PARAM,PARAM_OUTPUT_AMP_CAP);
    let steps=ac as u8&0x7F; let gain=if steps>0{steps/2}else{0};
    corb_send_verb(mmio,codec,dac,VERB_SET_AMP_GAIN_MUTE,0x70|gain);
    // 8-bit mono: bits 7:4 = 0x0 (8-bit), bits 3:0 = 0x0 (1ch mono)
    // The PCM data is raw 8-bit signed mono at 44100 Hz.
    corb_send_verb(mmio,codec,dac,VERB_SET_FMT,0x00);
    corb_send_verb(mmio,codec,dac,VERB_SET_STREAM,stream);
    let pa=corb_send_verb(mmio,codec,pin,VERB_GET_PARAM,PARAM_OUTPUT_AMP_CAP);
    let psteps=pa as u8&0x7F; let pgain=if psteps>0{psteps/2}else{0};
    corb_send_verb(mmio,codec,pin,VERB_SET_AMP_GAIN_MUTE,0x70|pgain);
    corb_send_verb(mmio,codec,pin,VERB_SET_PIN_CTL,0xC0);
    let cap=corb_send_verb(mmio,codec,pin,VERB_GET_PARAM,PARAM_PIN_CAP);
    if cap!=0xFFFF_FFFF&&(cap>>16)&1!=0{corb_send_verb(mmio,codec,pin,VERB_SET_EAPD,0x02);}
    log::info!("Sound: codec done DAC=0x{:x} Pin=0x{:x}",dac,pin);
}

fn hda_init(){
    if HDA_INIT_DONE.load(Ordering::Acquire){return}
    let phys=*HDA_PHYS.lock(); if phys==0{return}
    let off=petroleum::common::memory::get_physical_memory_offset() as u64;
    let virt=(phys+off) as usize; *HDA_VIRT.lock()=virt;
    let gctest=unsafe{r32(virt as *mut u8,GCAP)};
    if gctest==0||gctest==0xFFFF_FFFF{log::warn!("Sound: GCAP invalid"); *HDA_PHYS.lock()=0; return;}
    log::info!("Sound: GCAP=0x{:x}",gctest);
    let (iss,oss)=unsafe{let m=virt as *mut u8; w32(m,GCTL,0); for _ in 0..2000{core::hint::spin_loop();}
        w32(m,GCTL,1); for _ in 0..20000{if r32(m,GCTL)&1!=0{break}}
        if r32(m,GCTL)&1==0{log::warn!("Sound: controller reset timeout"); return;}
        w16(m,STATESTS,0x000F); w32(m,INTCTL,0); let gcap=r32(m,GCAP); ((gcap>>8)&0xF,(gcap>>12)&0xF)};
    log::info!("Sound: ISS={} OSS={}",iss,oss);
    if oss==0{log::warn!("Sound: no output streams"); return;}
    *HDA_SD.lock()=SD_BASE+(iss as usize)*SD_SIZE;
    let Some((corb_phys,corb_virt))=alloc_dma_pages(1) else{return}; *HDA_CORB_V.lock()=corb_virt as usize;
    let Some((rirb_phys,rirb_virt))=alloc_dma_pages(1) else{return}; *HDA_RIRB_V.lock()=rirb_virt as usize;
    // ── CORB size encoding ────────────────────────────────────
    // GCAP bit 0 → 64-bit address support; bits 7:4 → CORBSZCAP
    // We request 256 entries → CORBSIZE = 10b (bits 9:8 of CORBCTL).
    // But first we must ensure the controller supports it; if not,
    // fall back to 2 entries (00b) or 16 entries (01b).
    let gcap=unsafe{r16(virt as *mut u8,GCAP+2) as u32 | ((r16(virt as *mut u8,GCAP) as u32)<<16)};
    // Full 32-bit GCAP.  CORB size capability in bits 7:4.
    let corb_szcap=(gcap>>4)&0xF; // 0=2, 1=16, 2=256 entries
    // By default assume 256 entries.  If the controller does not
    // support that, fall back to 16 entries.
    let corb_sz:u32=if corb_szcap>=2{2}else if corb_szcap>=1{1}else{0};
    let corb_sz_bits=corb_sz<<8; // CORBSIZE in bits 9:8
    // CORB entries count derived from size code
    let corb_n:usize=match corb_sz{0=>2,1=>16,_=>256};
    // RIRB uses the same size field (bits 9:8 of RIRBCTL);
    // the controller only supports a single size for both.
    let rirb_sz_bits=corb_sz_bits;
    // Store for corb_send_verb
    *HDA_CORB_ENTRIES.lock()=corb_n;

    unsafe{let m=virt as *mut u8;
        // Stop CORB/RIRB DMA engines before programming
        w32(m,CORBCTL,0); w32(m,RIRBCTL,0);
        w32(m,CORBLBASE,corb_phys as u32); w32(m,CORBUBASE,(corb_phys>>32) as u32);
        // CORB Read Pointer Reset via bit 15, then clear RP/WP
        w16(m,CORBRP,0x8000); for _ in 0..200{core::hint::spin_loop();} w16(m,CORBRP,0); w16(m,CORBWP,0);
        // Enable CORB DMA with the correct size
        w32(m,CORBCTL,0x02|corb_sz_bits);
        w32(m,RIRBLBASE,rirb_phys as u32); w32(m,RIRBUBASE,(rirb_phys>>32) as u32);
        // RIRBWP reset: set bit 15 (RIRBRST) then clear
        w16(m,RIRBWP,0x8000); for _ in 0..200{core::hint::spin_loop();}
        // Read back to confirm reset is released, then zero WP
        if r16(m,RIRBWP)&0x8000!=0{w16(m,RIRBWP,0);}
        // Enable RIRB DMA with the correct size
        w32(m,RIRBCTL,0x02|rirb_sz_bits);
        log::info!("Sound: CORB/RIRB enabled (size={} entries)",corb_n);}
    let codec_addr:u8=0;
    unsafe{if let Some((dac,pin))=discover_codec(virt as *mut u8,codec_addr){
        configure_codec(virt as *mut u8,codec_addr,dac,pin,0);}else{log::warn!("Sound: no codec widgets");}}
    let dma_pages=(DMA_BUF_SIZE as usize+4095)/4096;
    let Some((dma_phys,dma_virt))=alloc_dma_pages(dma_pages) else{return}; *HDA_DMA.lock()=dma_virt as usize;
    let bdl_sz=core::mem::size_of::<BdlEntry>() as u64*BDL_ENTRIES as u64;
    let audio_phys=dma_phys+bdl_sz; let audio_off=bdl_sz as u32; let audio_sz=DMA_BUF_SIZE-audio_off;
    let half=audio_sz/2; *HDA_AUDIO_OFF.lock()=audio_off; *HDA_AUDIO_SZ.lock()=audio_sz; *HDA_HALF.lock()=half;
    unsafe{let bdl=dma_virt as *mut BdlEntry;
        *bdl.add(0)=BdlEntry{addr_lo:audio_phys as u32,addr_hi:(audio_phys>>32) as u32,length:half,flags:0};
        *bdl.add(1)=BdlEntry{addr_lo:(audio_phys+half as u64) as u32,addr_hi:((audio_phys+half as u64)>>32) as u32,length:half,flags:0};}
    unsafe{let m=virt as *mut u8; let sd=*HDA_SD.lock();
        // Stop any previous stream, then clear status
        w32(m,sd+SD_CTL,0); for _ in 0..2000{core::hint::spin_loop();}
        w8(m,sd+SD_STS,0xFF); // clear all status bits (WC)
        // Reset stream
        w32(m,sd+SD_CTL,0x01); for _ in 0..2000{core::hint::spin_loop();}
        // Wait for reset to complete
        for _ in 0..50000{if r32(m,sd+SD_CTL)&0x01==0{break}core::hint::spin_loop();}
        // Program format, BDL and stream settings
        w8(m,sd+SD_STS,0xFF);
        w16(m,sd+SD_FMT,0x4000); // 44.1 kHz 8-bit mono (bit14=BASE44, bits7:4=0=8-bit, bits3:0=0=1ch)
        w32(m,sd+SD_CBL,audio_sz);
        w16(m,sd+SD_LVI,BDL_ENTRIES as u16-1);
        w32(m,sd+SD_BDPL,dma_phys as u32); w32(m,sd+SD_BDPU,(dma_phys>>32) as u32);
        // Store fence: ensure BDL / DMA buffer writes are visible
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // Start stream: RUN (bit 1) + IOCE (bit 2) + STRIPE1 (bits 18:16)
        w32(m,sd+SD_CTL,(1u32<<16)|0x02);
        log::info!("Sound: stream started ({} B, fmt=0x4000)",audio_sz);}
    HDA_READY.store(true,Ordering::Release); HDA_INIT_DONE.store(true,Ordering::Release);
}

pub fn hda_available()->bool{*HDA_PHYS.lock()!=0}

pub fn hda_write_direct(offset:u32,samples:&[u8])->usize{
    hda_init();
    if !HDA_READY.load(Ordering::Acquire){return 0}
    let dma=*HDA_DMA.lock() as *mut u8; let off=*HDA_AUDIO_OFF.lock();
    let total=*HDA_AUDIO_SZ.lock() as usize; let max_len=total.saturating_sub(offset as usize);
    let n=samples.len().min(max_len); if n==0{return 0}
    unsafe{let dst=dma.add((off+offset) as usize); core::ptr::copy_nonoverlapping(samples.as_ptr(),dst,n);}
    n
}

pub fn hda_feed_samples(samples:&[u8])->usize{
    hda_init();
    if !HDA_READY.load(Ordering::Acquire){return 0}
    let virt=*HDA_VIRT.lock(); if virt==0{return 0}
    let mmio=virt as *mut u8; let dma=*HDA_DMA.lock() as *mut u8;
    let off=*HDA_AUDIO_OFF.lock(); let half=*HDA_HALF.lock(); let sd=*HDA_SD.lock();

    let lpib=unsafe{r32(mmio,sd+SD_LPIB)};
    let last=HDA_LAST_LPIB.load(Ordering::Relaxed) as u32;

    let dma_in_first=lpib<half;
    let write_off=if dma_in_first{half}else{0};
    // Only write if DMA has crossed half boundary
    if last==write_off{return 0}
    HDA_LAST_LPIB.store(write_off as u64,Ordering::Relaxed);

    let write_max=half as usize;
    let n=samples.len().min(write_max); if n==0{return 0}
    unsafe{let dst=dma.add((off+write_off) as usize); core::ptr::copy_nonoverlapping(samples.as_ptr(),dst,n);
        if n<write_max{core::ptr::write_bytes(dst.add(n),0,write_max-n);}}
    n
}

pub fn hda_poll(){
    if !HDA_READY.load(Ordering::Acquire){return}
    let virt=*HDA_VIRT.lock(); if virt==0{return}
    let mmio=virt as *mut u8; let half=*HDA_HALF.lock(); let sd=*HDA_SD.lock();
    let last=HDA_LAST_LPIB.load(Ordering::Relaxed) as u32;
    loop{let lpib=unsafe{r32(mmio,sd+SD_LPIB)}; let a=lpib<half; let b=last<half;
        if a!=b{break} core::hint::spin_loop();}
}

/// Poll with optional TSC‑based timeout.  Returns `true` when data
/// was fed, `false` on timeout / not ready.
pub fn hda_poll_block(timeout_tsc: Option<u64>) -> bool {
    if !HDA_READY.load(Ordering::Acquire) {
        return false;
    }
    let virt = *HDA_VIRT.lock();
    if virt == 0 {
        return false;
    }
    let mmio = virt as *mut u8;
    let half = *HDA_HALF.lock();
    let sd = *HDA_SD.lock();
    let last = HDA_LAST_LPIB.load(Ordering::Relaxed) as u32;
    let deadline = match timeout_tsc {
        Some(d) => unsafe { core::arch::x86_64::_rdtsc() }.wrapping_add(d),
        None => u64::MAX,
    };
    loop {
        let lpib = unsafe { r32(mmio, sd + SD_LPIB) };
        let a = lpib < half;
        let b = last < half;
        if a != b {
            return true;
        }
        if timeout_tsc.is_some() && unsafe { core::arch::x86_64::_rdtsc() } >= deadline {
            return false;
        }
        core::hint::spin_loop();
    }
}

/// TSC‑based delay after HDA poll (used for silence drain).
pub fn hda_poll_delay(tsc_per_ms: u64, ms: u64) {
    let deadline = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_add(tsc_per_ms.saturating_mul(ms));
    while unsafe { core::arch::x86_64::_rdtsc() } < deadline {
        hda_poll();
        core::hint::spin_loop();
    }
}

/// High‑level PCM feed: try to push `pcm[pcm_off..pcm_total]` into
/// the HDA half‑buffer.  Advances `*pcm_off`.  Returns immediately
/// if the destination half is not ready.
#[inline]
pub fn hda_feed_pcm(pcm: &[u8], pcm_off: &mut usize, pcm_total: usize, half: usize) -> usize {
    let off = *pcm_off;
    if off >= pcm_total {
        return 0;
    }
    let rem = pcm_total - off;
    let end = (off + rem.min(half)).min(pcm_total);
    let fed = hda_feed_samples(&pcm[off..end]);
    if fed > 0 {
        *pcm_off += fed;
    }
    fed
}

/// Feed silence into the HDA half‑buffer.
pub fn hda_feed_silence(half: usize) -> usize {
    hda_feed_samples(&[0u8; 16368][..half.min(16368)])
}
