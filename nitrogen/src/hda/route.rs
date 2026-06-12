//! DAC→Pin route finding and codec configuration.
//!
//! `RouteFinder` takes a `CodecGraph` and finds a path from an
//! analog DAC to a speaker/headphone pin complex, dealing with
//! intermediate mixers and EAPD-capable pins.

use crate::hda::corb::CorbEngine;
use crate::hda::corb::verbs;
use crate::hda::corb::params;
use crate::hda::codec::{CodecGraph, WidgetInfo};
use crate::hda::widget_type;

pub struct RouteFinder;

impl RouteFinder {
    /// Find the best (DAC, Pin) pair for speaker output.
    ///
    /// Strategy:
    /// - Collect all analog DACs (AUDIO_OUTPUT with no digital bit).
    /// - Collect all output‑capable pin complexes sorted by preference:
    ///   EAPD‑connected > non‑EAPD > fallback.
    /// - Return the last DAC (higher node = internal speaker on many codecs)
    ///   and the best pin.
    ///
    /// # Safety
    ///
    /// `mmio` must be a valid HDA MMIO base.  `corb` must be initialised.
    pub unsafe fn find_speaker_route(
        mmio: *mut u8,
        corb: &CorbEngine,
        graph: &CodecGraph,
    ) -> Option<(u8, u8)> {
        let mut dacs: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
        let mut best_pin: Option<u8> = None;
        let mut best_score: u8 = 0;

        for w in &graph.widgets {
            if w.widget_type == widget_type::AUDIO_OUTPUT {
                // Skip digital converters — bit 9 of wcaps.
                if (w.wcaps >> 9) & 1 != 0 {
                    log::info!("HDA: skipping digital DAC 0x{:x}", w.node_id);
                    continue;
                }
                dacs.push(w.node_id);
            }
            if w.widget_type == widget_type::PIN_COMPLEX {
                // Must support output (bit 4 of pin_cap)
                if w.pin_cap == 0xFFFF_FFFF || (w.pin_cap & (1 << 4)) == 0 {
                    log::info!(
                        "HDA: pin 0x{:x} cap=0x{:08x} — skipping (no OUT)",
                        w.node_id, w.pin_cap
                    );
                    continue;
                }

                let has_eapd = (w.pin_cap >> 16) & 1 != 0;
                let is_connected =
                    w.pin_default != 0xFFFF_FFFF && ((w.pin_default >> 8) & 0xF) != 0xF;

                // Scoring: connected EAPD (4) > connected non-EAPD (3) >
                // unconnected EAPD (2) > unconnected non-EAPD (1)
                let score: u8 = match (has_eapd, is_connected) {
                    (true, true) => 4,
                    (false, true) => 3,
                    (true, false) => 2,
                    (false, false) => 1,
                };

                if score > best_score {
                    best_pin = Some(w.node_id);
                    best_score = score;
                }
            }
        }

        // Prefer the last DAC (higher node IDs = internal speaker on ALC286 etc.)
        let dac = dacs.last().copied()?;
        let pin = best_pin?;

        log::info!("HDA: selected DAC=0x{:x} Pin=0x{:x}", dac, pin);
        Some((dac, pin))
    }

    /// Configure the codec path from DAC to Pin.
    ///
    /// This sets:
    /// - DAC output amp (unmute, max gain)
    /// - DAC format (48 kHz, 16-bit, 1 channel)
    /// - DAC stream tag
    /// - Pin output amp
    /// - Route through any intermediate mixer
    /// - EAPD enable on the pin if supported
    /// - Pin output enable
    ///
    /// # Safety
    ///
    /// `mmio` must be valid.  `corb` must be initialised.
    pub unsafe fn configure_route(
        mmio: *mut u8,
        corb: &CorbEngine,
        graph: &CodecGraph,
        dac: u8,
        pin: u8,
        stream_tag: u8,
    ) {
        // ── Configure DAC ─────────────────────────────────────────
        let dac_widget = graph.get_widget(dac);
        let ac = dac_widget.map(|w| w.out_amp_cap).unwrap_or(0);
        let offset = (ac & 0x7F) as u8;
        let nsteps = ((ac >> 8) & 0x7F) as u8;
        let gain = if nsteps > 0 { offset } else { 0 };
        log::info!(
            "HDA: DAC 0x{:x} amp cap=0x{:08x} offset={} nsteps={} gain={}",
            dac, ac, offset, nsteps, gain
        );

        // Unmute DAC output amp: SetOut + SetLeft + SetRight + gain
        corb.send_verb(
            mmio, 0, dac,
            verbs::SET_AMP_GAIN_MUTE,
            0xB000u16 | gain as u16,
        );

        // 16-bit signed mono at 48 kHz:
        // bit[7]=0→48kHz, bits[6:4]=1→16-bit, bits[3:0]=0→1ch
        corb.send_verb(mmio, 0, dac, verbs::SET_FMT, 0x10u16);
        corb.send_verb(mmio, 0, dac, verbs::SET_STREAM, stream_tag as u16);

        // ── Configure Pin ─────────────────────────────────────────
        let pin_widget = graph.get_widget(pin);
        let pa = pin_widget.map(|w| w.out_amp_cap).unwrap_or(0);
        let p_offset = (pa & 0x7F) as u8;
        let p_nsteps = ((pa >> 8) & 0x7F) as u8;
        let pgain = if p_nsteps > 0 { p_offset } else { 0 };
        log::info!(
            "HDA: Pin 0x{:x} amp cap=0x{:08x} offset={} nsteps={} pgain={}",
            pin, pa, p_offset, p_nsteps, pgain
        );

        // Unmute pin output amp
        corb.send_verb(
            mmio, 0, pin,
            verbs::SET_AMP_GAIN_MUTE,
            0xB000u16 | pgain as u16,
        );

        // ── Route through mixer ───────────────────────────────────
        // Look at the pin's connection list; find a mixer that
        // includes our DAC as input.
        if let Some(pin_w) = graph.get_widget(pin) {
            'pin_con: for (con_idx, &con_node) in pin_w.connections.iter().enumerate() {
                // Direct connection to DAC — handle before widget lookup,
                // because the DAC widget IS in the graph and would never
                // reach the None branch below.
                if con_node == dac {
                    let r = corb.send_verb(
                        mmio, 0, pin,
                        verbs::SET_CONNECTION_SELECT, con_idx as u16,
                    );
                    log::info!(
                        "HDA: SET_CONN pin=0x{:x} → DAC 0x{:x} (direct) result=0x{:08x}",
                        pin, con_node, r
                    );
                    break 'pin_con;
                }

                let con_w = match graph.get_widget(con_node) {
                    Some(w) => w,
                    None => continue,
                };

                if con_w.widget_type != widget_type::AUDIO_MIXER {
                    continue;
                }

                // Check if this mixer includes our DAC
                for (mix_ci, &mix_src) in con_w.connections.iter().enumerate() {
                    if mix_src == dac {
                        // Select this mixer on the pin
                        let r = corb.send_verb(
                            mmio, 0, pin,
                            verbs::SET_CONNECTION_SELECT, con_idx as u16,
                        );
                        log::info!(
                            "HDA: SET_CONN pin=0x{:x} → mixer 0x{:x} (DAC 0x{:x}) result=0x{:08x}",
                            pin, con_node, dac, r
                        );

                        // Unmute mixer input for the DAC channel
                        let unmute_payload = 0x3000u16 | ((mix_ci as u16) << 8);
                        let r2 = corb.send_verb(
                            mmio, 0, con_node,
                            verbs::SET_AMP_GAIN_MUTE,
                            unmute_payload,
                        );
                        log::info!(
                            "HDA: UNMUTE mixer 0x{:x} input {} result=0x{:08x}",
                            con_node, mix_ci, r2
                        );
                        break 'pin_con;
                    }
                }
            }
        }

        // ── EAPD & Pin output ─────────────────────────────────────
        let pin_cap = pin_widget.map(|w| w.pin_cap).unwrap_or(0);
        let eapd_capable = pin_cap != 0xFFFF_FFFF && (pin_cap >> 16) & 1 != 0;
        log::info!(
            "HDA: pin 0x{:x} cap=0x{:08x} eapd_capable={}",
            pin, pin_cap, eapd_capable
        );

        if eapd_capable {
            let eapd_res = corb.send_verb(mmio, 0, pin, verbs::SET_EAPD, 0x02);
            log::info!("HDA: SET_EAPD pin=0x{:x} result=0x{:08x}", pin, eapd_res);
        }

        // Enable pin output (0x40 = Output Enable only)
        let pin_ctl_res = corb.send_verb(mmio, 0, pin, verbs::SET_PIN_CTL, 0x40u16);
        log::info!(
            "HDA: SET_PIN_CTL pin=0x{:x} val=0x40 result=0x{:08x}",
            pin, pin_ctl_res
        );

        log::info!("HDA: codec configured DAC=0x{:x} Pin=0x{:x}", dac, pin);
    }
}