//! HDA codec diagnostic inventory dump.
//!
//! This module provides `dump_codec_inventory()`, which performs a
//! comprehensive enumeration of all widgets and their capabilities,
//! producing detailed log output suitable for debugging silent‑output
//! issues on real hardware.

use crate::hda::corb::CorbEngine;
use crate::hda::corb::verbs;
use crate::hda::corb::params;
use crate::hda::codec::{CodecGraph, WidgetInfo, widget_type_name};
use crate::hda::widget_type;

/// Dump a comprehensive inventory of every widget node reachable from
/// the AFG.  This includes:
///
/// - Codec vendor / device / revision IDs
/// - Per‑widget: node id, wcaps, type, connection list, pin caps & default,
///   amp caps (input + output), current amp state, current power state,
///   current pin control, EAPD state.
///
/// The output is intentionally verbose so that real‑hardware silent‑output
/// bugs (wrong DAC→Pin routing, muted amp, powered‑down EAPD, etc.) can be
/// diagnosed from the serial log alone.
///
/// # Safety
///
/// `mmio` must be a valid HDA MMIO base.  `corb` must be initialised.
/// `graph` should be the result of `CodecGraph::enumerate()`.
pub unsafe fn dump_codec_inventory(
    mmio: *mut u8,
    corb: &CorbEngine,
    graph: &CodecGraph,
) {
    let codec: u8 = 0;

    log::info!("HDA: === CODEC INVENTORY (codec={}) ===", codec);
    log::info!(
        "HDA:  Vendor=0x{:08x} Rev=0x{:08x}",
        graph.vendor_id, graph.revision_id
    );
    log::info!("HDA:  AFG node=0x{:02x}", graph.afg_node);

    if graph.subsystem_id != 0xFFFF_FFFF {
        log::info!(
            "HDA:  SubsystemID: {:04x}:{:04x}",
            (graph.subsystem_id >> 16) & 0xFFFF,
            graph.subsystem_id & 0xFFFF
        );
    }

    log::info!("HDA:  Widget nodes {} entries:", graph.widgets.len());

    for w in &graph.widgets {
        let n = w.node_id;
        let t = w.widget_type;

        log::info!(
            "HDA:  ┌─ node=0x{:02x} wcaps=0x{:08x} type={}({})",
            n, w.wcaps, widget_type_name(t), t
        );

        // Connection list
        if w.connection_count > 0 {
            log::info!("HDA:  │ connections ({}):", w.connection_count);
            for (ci, &src) in w.connections.iter().enumerate() {
                log::info!("HDA:  │   [{}] → 0x{:02x}", ci, src);
            }
        }

        // Current power state
        let ps = corb.send_verb(mmio, codec, n, verbs::GET_POWER_STATE, 0);
        if ps != 0xFFFF_FFFF {
            log::info!("HDA:  │ PowerState=0x{:08x}", ps);
        }

        match t {
            widget_type::AUDIO_OUTPUT
            | widget_type::AUDIO_INPUT
            | widget_type::AUDIO_MIXER
            | widget_type::AUDIO_SELECTOR => {
                // Output amp cap
                if w.out_amp_cap != 0xFFFF_FFFF && w.out_amp_cap != 0 {
                    let mute_capable = (w.out_amp_cap >> 31) & 1;
                    let step_size = (w.out_amp_cap >> 16) & 0x7F;
                    let num_steps = (w.out_amp_cap >> 8) & 0x7F;
                    let offset = w.out_amp_cap & 0x7F;
                    log::info!(
                        "HDA:  │ OutAmpCap=0x{:08x} mute={} stepSize={} nSteps={} offset={}",
                        w.out_amp_cap, mute_capable, step_size, num_steps, offset
                    );
                }

                // Input amp cap
                if (t == widget_type::AUDIO_MIXER
                    || t == widget_type::AUDIO_SELECTOR
                    || t == widget_type::AUDIO_INPUT)
                    && w.in_amp_cap != 0xFFFF_FFFF
                    && w.in_amp_cap != 0
                {
                    log::info!(
                        "HDA:  │ InAmpCap=0x{:08x} mute={} stepSize={} nSteps={} offset={}",
                        w.in_amp_cap,
                        (w.in_amp_cap >> 31) & 1,
                        (w.in_amp_cap >> 16) & 0x7F,
                        (w.in_amp_cap >> 8) & 0x7F,
                        w.in_amp_cap & 0x7F
                    );
                }

                // Current output amp state
                let amp_out =
                    corb.send_verb(mmio, codec, n, verbs::GET_AMP_GAIN_MUTE, 0x8000);
                if amp_out != 0xFFFF_FFFF {
                    let muted = (amp_out >> 7) & 1;
                    let gain = amp_out & 0x7F;
                    log::info!(
                        "HDA:  │ CurOutAmp=0x{:04x} mute={} gain={}",
                        amp_out, muted, gain
                    );
                }

                // Current input amp for mixers
                if t == widget_type::AUDIO_MIXER || t == widget_type::AUDIO_SELECTOR {
                    for inp_idx in 0..w.connection_count.min(4) {
                        let amp_in = corb.send_verb(
                            mmio, codec, n,
                            verbs::GET_AMP_GAIN_MUTE,
                            (inp_idx as u16) << 8,
                        );
                        if amp_in != 0xFFFF_FFFF {
                            let muted = (amp_in >> 7) & 1;
                            let gain = amp_in & 0x7F;
                            log::info!(
                                "HDA:  │ CurInAmp[{}]=0x{:04x} mute={} gain={}",
                                inp_idx, amp_in, muted, gain
                            );
                        }
                    }
                }

                // PCM / stream
                if w.pcm != 0xFFFF_FFFF && w.pcm != 0 {
                    log::info!("HDA:  │ PCM=0x{:08x}", w.pcm);
                }
                if w.stream != 0xFFFF_FFFF && w.stream != 0 {
                    log::info!("HDA:  │ Stream=0x{:08x}", w.stream);
                }
            }
            widget_type::PIN_COMPLEX => {
                // Pin capabilities
                if w.pin_cap != 0xFFFF_FFFF {
                    log::info!(
                        "HDA:  │ PinCap=0x{:08x} [{}]",
                        w.pin_cap, pin_cap_str(w.pin_cap)
                    );
                }

                // Pin default configuration
                if w.pin_default != 0xFFFF_FFFF {
                    log::info!(
                        "HDA:  │ PinDefault=0x{:08x} → {}",
                        w.pin_default, pin_default_str(w.pin_default)
                    );
                }

                // Current pin control
                let pin_ctl = corb.send_verb(mmio, codec, n, verbs::GET_PIN_CTL, 0);
                if pin_ctl != 0xFFFF_FFFF {
                    let out = pin_ctl & (1 << 6) != 0;
                    let hp = pin_ctl & (1 << 7) != 0;
                    let in_en = pin_ctl & (1 << 5) != 0;
                    let vref = (pin_ctl >> 8) & 0xFF;
                    let eapd_raw = (pin_ctl >> 16) & 0xFF;
                    log::info!(
                        "HDA:  │ CurPinCtl=0x{:02x} OUT={} HP={} IN={} VRef=0x{:02x} EAPD=0x{:02x}",
                        pin_ctl, out, hp, in_en, vref, eapd_raw
                    );
                }

                // EAPD current state
                if w.pin_cap != 0xFFFF_FFFF && (w.pin_cap >> 16) & 1 != 0 {
                    let eapd_state = corb.send_verb(mmio, codec, n, verbs::GET_EAPD, 0);
                    if eapd_state != 0xFFFF_FFFF {
                        log::info!("HDA:  │ CurEAPD=0x{:02x}", eapd_state & 0xFF);
                    }
                }

                // Pin sense
                let sense = corb.send_verb(mmio, codec, n, verbs::GET_PIN_SENSE, 0);
                if sense != 0xFFFF_FFFF {
                    let present = (sense >> 31) & 1;
                    log::info!("HDA:  │ PinSense=0x{:08x} present={}", sense, present);
                }
            }
            _ => {}
        }
        log::info!("HDA:  └─ end node=0x{:02x}", n);
    }

    log::info!("HDA: === END CODEC INVENTORY ===");
}

// ── Helper: pin capability string ─────────────────────────────────

fn pin_cap_str(pincap: u32) -> alloc::string::String {
    let mut s = alloc::string::String::new();
    if pincap & (1 << 0) != 0 { s.push_str("ImpSense "); }
    if pincap & (1 << 1) != 0 { s.push_str("TrigReq "); }
    if pincap & (1 << 2) != 0 { s.push_str("PresDet "); }
    if pincap & (1 << 4) != 0 { s.push_str("OUT "); }
    if pincap & (1 << 5) != 0 { s.push_str("IN "); }
    if pincap & (1 << 6) != 0 { s.push_str("Balanced "); }
    if pincap & (1 << 7) != 0 { s.push_str("HP-Drv "); }
    if pincap & (1 << 8) != 0 { s.push_str("Vref "); }
    if pincap & (1 << 16) != 0 { s.push_str("EAPD "); }
    if pincap & (1 << 24) != 0 { s.push_str("DP "); }
    if pincap & (1 << 25) != 0 { s.push_str("HDMI "); }
    if s.is_empty() { s.push_str("(none)"); }
    s
}

/// Decode Pin Default Configuration word (verb F1C response).
fn pin_default_str(cfg: u32) -> alloc::string::String {
    let location = (cfg >> 24) & 0x3F;
    let device = (cfg >> 20) & 0xF;
    let conn_type = (cfg >> 16) & 0xF;
    let color = (cfg >> 12) & 0xF;
    let misc = (cfg >> 8) & 0xF;
    let def_assoc = (cfg >> 4) & 0xF;
    let sequence = cfg & 0xF;

    let device_name = match device {
        0x0 => "LineOut",
        0x1 => "Speaker",
        0x2 => "HPOut",
        0x3 => "CD",
        0x4 => "SPDIFOut",
        0x5 => "DigitalOther",
        0x6 => "ModemLine",
        0x7 => "ModemHandset",
        0x8 => "LineIn",
        0x9 => "AUX",
        0xA => "MicIn",
        0xB => "Telephony",
        0xC => "SPDIFIn",
        0xD => "DigitalOtherIn",
        0xF => "Other",
        _ => "?",
    };

    let conn_name = match conn_type {
        0x0 => "Unknown",
        0x1 => "1/8\"",
        0x2 => "1/4\"",
        0x3 => "ATAPI",
        0x4 => "RCA",
        0x5 => "Optical",
        0x6 => "OtherDigital",
        0x7 => "OtherAnalog",
        0x8 => "DIN",
        0x9 => "XLR",
        0xF => "Other",
        _ => "?",
    };

    let is_connected = def_assoc != 0xF;
    alloc::format!(
        "{}(dev={}, color={:#x}, conn={}, loc={:#x}, misc={:#x}, seq={})",
        if is_connected { "" } else { "UNCONNECTED " },
        device_name, color, conn_name, location, misc, sequence,
    )
}