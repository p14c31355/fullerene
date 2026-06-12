//! Codec widget graph enumeration.
//!
//! `CodecGraph` discovers all widgets (nodes) reachable from the AFG
//! (Audio Function Group) and builds a lightweight in‑memory graph:
//!
//! ```text
//! CodecGraph
//!  ├── widgets: Vec<WidgetInfo>
//!  │    ├── node_id, wcaps, widget_type
//!  │    ├── connection_list (Vec<u8>)
//!  │    ├── pin_cap, pin_default, eapd
//!  │    └── amp_cap (out + in)
//!  └── afg_node: u8
//! ```
//!
//! This is intentionally separate from the route‑finding logic
//! (`route.rs`), so diagnostics and routing can both consume the
//! graph without duplicating verb queries.

use crate::hda::corb::CorbEngine;
use crate::hda::corb::params;
use crate::hda::corb::verbs;
use crate::hda::widget_type;

/// Information about a single widget node in the codec.
#[derive(Clone, Debug)]
pub struct WidgetInfo {
    /// Widget node ID (4‑bit or 7‑bit address).
    pub node_id: u8,
    /// Audio Widget Capabilities raw value.
    pub wcaps: u32,
    /// Widget type (see `widget_type` module).
    pub widget_type: u32,
    /// Connection list (source node IDs), empty if no connections.
    pub connections: alloc::vec::Vec<u8>,
    /// Connection list length (from parameter 0x0E).
    pub connection_count: u8,
    /// Pin Capabilities (only valid for PIN_COMPLEX).
    pub pin_cap: u32,
    /// Pin Default Configuration (only valid for PIN_COMPLEX).
    pub pin_default: u32,
    /// Output Amp Capabilities.
    pub out_amp_cap: u32,
    /// Input Amp Capabilities.
    pub in_amp_cap: u32,
    /// PCM format support.
    pub pcm: u32,
    /// Stream format support.
    pub stream: u32,
}

/// A snapshot of the codec's widget graph.
pub struct CodecGraph {
    /// All widgets discovered under the AFG.
    pub widgets: alloc::vec::Vec<WidgetInfo>,
    /// The AFG node ID.
    pub afg_node: u8,
    /// Vendor ID (from root node).
    pub vendor_id: u32,
    /// Revision ID.
    pub revision_id: u32,
    /// Subsystem ID (32‑bit: [31:16] = SSID, [15:0] = SVID).
    pub subsystem_id: u32,
}

impl CodecGraph {
    /// Enumerate all widgets reachable from the root node, then from
    /// the AFG node.
    ///
    /// # Safety
    ///
    /// `mmio` must be a valid HDA MMIO base.  `corb` must be initialised.
    pub unsafe fn enumerate(mmio: *mut u8, corb: &CorbEngine, codec: u8) -> Self {
        let vendor_id = corb.send_verb(mmio, codec, 0, verbs::GET_PARAM, params::VENDOR_ID);
        let revision_id = corb.send_verb(mmio, codec, 0, verbs::GET_PARAM, params::REVISION_ID);
        let sub = corb.send_verb(mmio, codec, 0, verbs::GET_PARAM, params::SUBORDINATE_COUNT);
        let ssid = corb.send_verb(mmio, codec, 0, verbs::GET_SUBSYSTEM_ID, 0);

        let start_root = ((sub >> 16) & 0xFF) as u8;
        let count_root = (sub & 0xFF) as u8;
        let mut afg: Option<u8> = None;

        // ── Find AFG among root nodes ────────────────────────────
        if sub != 0xFFFF_FFFF && count_root > 0 {
            let end_root = start_root + count_root - 1;
            for n in start_root..=end_root {
                let wc = corb.send_verb(mmio, codec, n, verbs::GET_PARAM, params::AUDIO_WIDGET_CAP);
                if wc == 0xFFFF_FFFF {
                    continue;
                }
                let t = (wc >> 20) & 0xF;
                log::info!(
                    "HDA: root node=0x{:02x} wcaps=0x{:08x} type={}({})",
                    n,
                    wc,
                    widget_type_name(t),
                    t
                );
                if t == widget_type::AFG {
                    afg = Some(n);
                }
            }
        }

        let afg_node = match afg {
            Some(a) => a,
            None => {
                log::warn!("HDA: no AFG found");
                return Self {
                    widgets: alloc::vec::Vec::new(),
                    afg_node: 0,
                    vendor_id,
                    revision_id,
                    subsystem_id: ssid,
                };
            }
        };

        // ── Enumerate AFG subordinates ───────────────────────────
        let sub2 = corb.send_verb(
            mmio,
            codec,
            afg_node,
            verbs::GET_PARAM,
            params::SUBORDINATE_COUNT,
        );
        let start_afg = ((sub2 >> 16) & 0xFF) as u8;
        let count_afg = (sub2 & 0xFF) as u8;

        let mut widgets = alloc::vec::Vec::new();

        if sub2 != 0xFFFF_FFFF && count_afg > 0 {
            let end_afg = start_afg + count_afg - 1;
            for n in start_afg..=end_afg {
                let wc = corb.send_verb(mmio, codec, n, verbs::GET_PARAM, params::AUDIO_WIDGET_CAP);
                if wc == 0xFFFF_FFFF {
                    log::info!("HDA: node=0x{:02x} *** NO RESPONSE ***", n);
                    continue;
                }
                let t = (wc >> 20) & 0xF;

                // Connection list
                let con_len = {
                    let r = corb.send_verb(
                        mmio,
                        codec,
                        n,
                        verbs::GET_PARAM,
                        params::CONNECTION_LIST_LEN,
                    );
                    if r == 0xFFFF_FFFF {
                        0
                    } else {
                        (r & 0x7F) as u8
                    }
                };
                let mut connections = alloc::vec::Vec::new();
                if con_len > 0 {
                    let count = con_len.min(16);
                    for ci in 0..count {
                        let chunk = (ci / 4) * 4;
                        let r = corb.send_verb(
                            mmio,
                            codec,
                            n,
                            verbs::GET_CONNECTION_LIST_ENTRY,
                            chunk as u16,
                        );
                        if r == 0xFFFF_FFFF {
                            continue;
                        }
                        let shift = (ci % 4) * 8;
                        let src = ((r >> shift) & 0x7F) as u8;
                        connections.push(src);
                    }
                }

                let mut pin_cap = 0;
                let mut pin_default = 0;
                let mut out_amp_cap = 0;
                let mut in_amp_cap = 0;
                let mut pcm = 0;
                let mut stream = 0;

                match t {
                    widget_type::AUDIO_OUTPUT
                    | widget_type::AUDIO_INPUT
                    | widget_type::AUDIO_MIXER
                    | widget_type::AUDIO_SELECTOR => {
                        out_amp_cap = corb.send_verb(
                            mmio,
                            codec,
                            n,
                            verbs::GET_PARAM,
                            params::OUTPUT_AMP_CAP,
                        );
                        if t == widget_type::AUDIO_MIXER
                            || t == widget_type::AUDIO_SELECTOR
                            || t == widget_type::AUDIO_INPUT
                        {
                            in_amp_cap = corb.send_verb(
                                mmio,
                                codec,
                                n,
                                verbs::GET_PARAM,
                                params::INPUT_AMP_CAP,
                            );
                        }
                        pcm = corb.send_verb(mmio, codec, n, verbs::GET_PARAM, params::PCM);
                        stream = corb.send_verb(mmio, codec, n, verbs::GET_PARAM, params::STREAM);
                    }
                    widget_type::PIN_COMPLEX => {
                        pin_cap = corb.send_verb(mmio, codec, n, verbs::GET_PARAM, params::PIN_CAP);
                        pin_default = corb.send_verb(mmio, codec, n, verbs::GET_CONFIG_DEFAULT, 0);
                        // Pins also have output amp
                        out_amp_cap = corb.send_verb(
                            mmio,
                            codec,
                            n,
                            verbs::GET_PARAM,
                            params::OUTPUT_AMP_CAP,
                        );
                    }
                    _ => {}
                }

                log::info!(
                    "HDA: node=0x{:02x} wcaps=0x{:08x} type={}({}) con_len={} pin_cap=0x{:08x}",
                    n,
                    wc,
                    widget_type_name(t),
                    t,
                    con_len,
                    pin_cap
                );

                widgets.push(WidgetInfo {
                    node_id: n,
                    wcaps: wc,
                    widget_type: t,
                    connections,
                    connection_count: con_len,
                    pin_cap,
                    pin_default,
                    out_amp_cap,
                    in_amp_cap,
                    pcm,
                    stream,
                });
            }
        }

        Self {
            widgets,
            afg_node,
            vendor_id,
            revision_id,
            subsystem_id: ssid,
        }
    }

    /// Look up a widget by node ID.
    pub fn get_widget(&self, node_id: u8) -> Option<&WidgetInfo> {
        self.widgets.iter().find(|w| w.node_id == node_id)
    }
}

/// Human‑readable widget type name.
pub fn widget_type_name(wtype: u32) -> &'static str {
    match wtype {
        widget_type::AUDIO_OUTPUT => "AudioOut",
        widget_type::AUDIO_INPUT => "AudioIn",
        widget_type::AUDIO_MIXER => "Mixer",
        widget_type::AUDIO_SELECTOR => "Selector",
        widget_type::PIN_COMPLEX => "Pin",
        widget_type::POWER_WIDGET => "Power",
        widget_type::VOLUME_KNOB => "VolumeKnob",
        widget_type::BEEP_GENERATOR => "BeepGen",
        widget_type::VENDOR_DEFINED => "VendorDef",
        _ => "Unknown",
    }
}
