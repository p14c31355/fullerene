//! Live digit-word readouts for the `usb2-live-*` instruments.
//!
//! Each word publishes numeric fields as N short Run/Stop blips
//! (`link_recovery::runstop_blips_fast`: 100 ms stopped + 70 ms running per pair),
//! one field per scheduled slot. The drive loop owns the clock and calls `publish()`;
//! the digits live here so that function reads as code rather than as eleven
//! instrument descriptions in a row.
//!
//! The bodies were moved verbatim out of `poll.rs` on 2026-09-30, together with the
//! locals each one computed first. The per-word narrative still sits beside each
//! `let <word> = matches!(..)` in that file and the two are meant to be read together.
//! Decode a capture with `python3 tools/ccs_fields.py <rundir>`.

use super::*;

/// Publish the next field of `name`, if this pass is the one that is due.
///
/// `counter` advances once per published field, so the caller only has to ask on
/// every pass. `first_ms` and `step_ms` are the word's schedule; `entry` and
/// `frequency` come from the drive loop's clock.
pub(super) fn publish(
    name: &str,
    counter: &mut u32,
    first_ms: u64,
    step_ms: u64,
    entry: u64,
    frequency: u64,
) {
    let count = field_count(name);
    if count == 0 || *counter >= count {
        return;
    }
    let at = entry.saturating_add(
        frequency
            .saturating_mul(first_ms + (*counter as u64) * step_ms)
            .saturating_div(1_000),
    );
    if unsafe { arch_counter() } < at {
        return;
    }
    let digit = digit_of(name, *counter);
    super::link_recovery::runstop_blips_fast(digit.min(6));
    *counter += 1;
}

/// How many fields a word publishes.
fn field_count(name: &str) -> u32 {
    match name {
        "trb_after" => 5,
        "depcmd_state" => 5,
        "global_block" => 5,
        "fail_state" => 5,
        "data_cmd" => 5,
        "depcmd" => 4,
        "data_report" => 6,
        // `event_count` asks which event-count register answers at all;
        // `blackbox` reports the reserved-RAM region the boot claimed.
        "event_count" => 3,
        "blackbox" => 4,
        "blackbox_read" => 3,
        "blackbox_stage" => 4,
        "blackbox_recent" => 4,
        "blackbox_state" => 4,
        "blackbox_count" => 4,
        "blackbox_probe" => 4,
        "blackbox_code0" => 1,
        "blackbox_code1" => 1,
        "blackbox_code2" => 1,
        "blackbox_boots" => 4,
        "blackbox_bootsn" => 1,
        "blackbox_why" => 1,
        "status_try" => 2,
        "data_zero" => 2,
        "data_alt" => 2,
        "data_late" => 2,
        "data_persist" => 3,
        "ioava_check" => 4,
        "data_status" => 3,
        "pstore_ready" => 2,
        "pmsg_read" => 4,
        "pmsg_age" => 1,
        "text" => 4,
        "prev_record" => 1,
        "prev_boot" => 1,
        "prev_region" => 1,
        "prev_stage" => 1,
        "fresh_gates" => 7,
        "setup_buffer_word" => 5,
        "response_bytes_word" => 6,
        "observe" => 4,
        _ => 0,
    }
}

/// The digit for one field of a word.
fn digit_of(name: &str, field: u32) -> u32 {
    match name {
        "trb_after" => trb_after_digit(field),
        "depcmd_state" => depcmd_state_digit(field),
        "global_block" => global_block_digit(field),
        "fail_state" => fail_state_digit(field),
        "data_cmd" => data_cmd_digit(field),
        "depcmd" => depcmd_digit(field),
        "data_report" => data_report_digit(field),
        "event_count" => event_count_digit(field),
        "blackbox" => blackbox_digit(field),
        "blackbox_read" => blackbox_read_digit(field),
        "blackbox_stage" => blackbox_stage_digit(field),
        "blackbox_recent" => blackbox_recent_digit(field),
        "blackbox_state" => crate::blackbox::state_digits()[field.min(3) as usize],
        "blackbox_count" => crate::blackbox::count_thresholds()[field.min(3) as usize],
        "blackbox_probe" => crate::blackbox::probe_digits()[field.min(3) as usize],
        "blackbox_code0" => crate::blackbox::previous_boot_code(0),
        "blackbox_code1" => crate::blackbox::previous_boot_code(1),
        "blackbox_code2" => crate::blackbox::previous_boot_code(2),
        "blackbox_boots" => crate::blackbox::boots_thresholds()[field.min(3) as usize],
        "blackbox_bootsn" => crate::blackbox::boots_digit(),
        "blackbox_why" => crate::blackbox::previous_boot_status(),
        // 1 = the status-only command retired, 2 = it did not, and the second field is
        // the same question asked of the TRB the controller was handed.
        // Does the DMA address the data stage is handed actually point at the descriptor?
        //
        // The late re-issue retires *and* the controller clears the TRB's HWO, so it did
        // fetch something from the address in `bpl/bph`. The host still answers with
        // protocol error -71 rather than a timeout, which means it received something
        // malformed rather than nothing. If the address means a different physical page
        // than the one the descriptor sits in - the obvious hazard when the Fastboot DMA
        // arena has been adopted and the IOVA is not the CPU address - the controller
        // would fetch plausible-looking garbage.
        //
        //   1  DMA adoption is active (the IOVA may differ from the CPU address)
        //   2  the IOVA's page offset matches the response pointer's
        //   3  the byte at the IOVA is 0x12, the descriptor's bLength - the decisive one
        //   4  the byte at the IOVA equals the byte at the response pointer
        // Whether each Android-readable mirror target was armed from the device tree.
        //
        // Two fields because "the mirror was never written" and "it was written and did not
        // survive the reboot" look identical from Android's side: pstore shows nothing in
        // both cases. This says which one is true, and it is the first thing to read
        // whenever the pstore route looks closed.
        // The persistence probe. `pmsg_write` puts a record naming this boot into the
        // ramoops pmsg zones, which nothing else writes during an ordinary boot;
        // `pmsg_read` reports what a later boot finds there. Field 1 of the read is the
        // whole exercise: 2 means an earlier boot's text is still in that memory.
        "pmsg_read" => crate::blackbox::pmsg_probe_digits()[field.min(3) as usize],
        // How many boots have written a segment to the durable log, capped at 6; 2 when the log
        // is empty. This word used to publish the *ring's* boot counter while documenting an
        // answer about the durable text, which is a different question with a different answer
        // (the ring counter never climbs; the log's segment count always does).
        "pmsg_age" => match crate::blackbox::text_log_digits()[0] {
            2 => 2,
            other => other,
        },

        // The durable log in the ramoops pmsg zone. `prev_*` are single-field yes/no words on
        // purpose: a word that packs several codes into fields is unreadable when a mark is
        // absorbed, and that has cost real runs today.
        "text" => crate::blackbox::text_log_digits()[field.min(3) as usize],
        "prev_record" => {
            if crate::blackbox::previous_segment_len() > 0 {
                1
            } else {
                2
            }
        }
        "prev_boot" => {
            if crate::blackbox::previous_segment_byte(0) == 1 {
                1
            } else {
                2
            }
        }
        "prev_region" => {
            if crate::blackbox::previous_segment_byte(1) == 2 {
                1
            } else {
                2
            }
        }
        "prev_stage" => {
            let third = crate::blackbox::previous_segment_byte(2);
            if (16..=24).contains(&third) { 1 } else { 2 }
        }
        "pstore_ready" => {
            let (primary, alt) = crate::blackbox::pstore_ready();
            match field {
                0 => {
                    if primary {
                        1
                    } else {
                        2
                    }
                }
                _ => {
                    if alt {
                        1
                    } else {
                        2
                    }
                }
            }
        }
        "data_status" => match field {
            // The two results, then whether the TRB was consumed. 3 in either of the first two
            // means the sequence had not run yet when that field was sampled, which is a state
            // this word used to report as "retired".
            0 => match super::ep0::data_then_status_result() {
                0 => 3,
                r => {
                    if r & 1 != 0 {
                        1
                    } else {
                        2
                    }
                }
            },
            1 => match super::ep0::data_then_status_result() {
                0 => 3,
                r => {
                    if r & 2 != 0 {
                        1
                    } else {
                        2
                    }
                }
            },
            _ => {
                let after = unsafe { super::DATA_PHASE_TRB_AFTER };
                if after[0] & TRB_HWO != 0 { 2 } else { 1 }
            }
        },
        "ioava_check" => {
            let response = unsafe { super::ep0_response_ptr() };
            let iova = unsafe { super::dma_iova_for(response as usize) };
            let at_iova = unsafe { core::ptr::read_volatile(iova as *const u8) };
            let at_cpu = unsafe { core::ptr::read_volatile(response) };
            match field {
                0 => {
                    if unsafe { super::DMA_ADOPTED } {
                        1
                    } else {
                        2
                    }
                }
                1 => {
                    if (iova & 0xfff) == (response as u64 & 0xfff) {
                        1
                    } else {
                        2
                    }
                }
                2 => {
                    if at_iova == 0x12 {
                        1
                    } else {
                        2
                    }
                }
                _ => {
                    if at_iova == at_cpu {
                        1
                    } else {
                        2
                    }
                }
            }
        }
        "data_persist" => match field {
            // 1 = nothing retired, 2 = one attempt retired, 3 = more than one, 6 = the probe had
            // not run when this field was sampled
            0 => match super::ep0::data_persist_result() {
                0 => 6,
                other => other.min(5),
            },
            1 => {
                let after = unsafe { super::DATA_PHASE_TRB_AFTER };
                if after[0] & TRB_HWO != 0 { 2 } else { 1 }
            }
            _ => {
                // TRBSTS of the data TRB: 0 means the controller never wrote it, which is
                // 0 when the transfer did not happen and non-zero when it did.
                let after = unsafe { super::DATA_PHASE_TRB_AFTER };
                let trbsts = (after[1] >> 28) & 0xf;
                if trbsts == 0 { 2 } else { 1 }
            }
        },
        "data_late" => match field {
            0 => match super::ep0::data_late_result() {
                0 => 3,
                other => other,
            },
            _ => {
                let after = unsafe { super::DATA_PHASE_TRB_AFTER };
                if after[0] & TRB_HWO != 0 { 2 } else { 1 }
            }
        },
        "data_alt" => match field {
            0 => match super::ep0::data_alt_result() {
                0 => 3,
                other => other,
            },
            _ => {
                let after = unsafe { super::DATA_PHASE_TRB_AFTER };
                if after[0] & TRB_HWO != 0 { 2 } else { 1 }
            }
        },
        "data_zero" => match field {
            0 => match super::ep0::data_zero_result() {
                0 => 3,
                other => other,
            },
            _ => {
                let after = unsafe { super::DATA_PHASE_TRB_AFTER };
                if after[0] & TRB_HWO != 0 { 2 } else { 1 }
            }
        },
        // NOTE, and this is the review's most important finding about these words: their fields
        // used to be published on a schedule starting at 1.0 s while their actions run at 1.5-2.5
        // s, and the latches read 0 ("not attempted") until then - which `clamp(1, 2)` mapped
        // onto digit 1, the digit every one of them documents as success. Every reading taken
        // from those fields before this change is suspect. 0 now has its own digit (3 here, 6
        // for `data_persist`, whose low values are already taken by the attempt count) and the
        // fields of these words are published after their actions.
        // READ THIS BEFORE TRUSTING THESE WORDS. Their result fields cannot be read inside the
        // boot that runs them: the probe acts at 2.5 s, and a field published before that samples
        // the "not attempted" zero while one published after it (2.7 s) is outside the run window
        // whenever the host gives up early - measured, twice, as a run that ends with no fields at
        // all. Both failure modes have already produced a false reading here, the first while the
        // zero was mapped onto the success digit.
        //
        // The fix is to carry the result across a boot in the durable log (the probe's return
        // value as a code class), which is what the black box exists for. Until that is done,
        // treat a digit from these fields as evidence about *when* the run ended, not about the
        // probe's outcome.
        "status_try" => match field {
            0 => match super::ep0::status_only_result() {
                0 => 3,
                other => other,
            },
            _ => {
                let after = unsafe { super::DATA_PHASE_TRB_AFTER };
                if after[0] & TRB_HWO != 0 { 2 } else { 1 }
            }
        },
        "fresh_gates" => fresh_gates_digit(field),
        "setup_buffer_word" => setup_buffer_word_digit(field),
        "response_bytes_word" => response_bytes_word_digit(field),
        "observe" => observe_digit(field),
        _ => 2,
    }
}

/// Digits for `trb_after`.
fn trb_after_digit(field: u32) -> u32 {
    let after = unsafe { DATA_PHASE_TRB_AFTER };
    let ctrl = after[0];
    let size = after[1];
    let trbsts = (size >> 28) & 0xf;
    match field {
        0 => trbsts.min(5) + 1,
        1 => {
            if ctrl & TRB_HWO != 0 {
                1
            } else {
                2
            }
        }
        2 => {
            if (ctrl >> 4) & 0x3f == 5 {
                1
            } else {
                2
            }
        }
        3 => {
            if size & 0x00ff_ffff == 0 {
                1
            } else {
                2
            }
        }
        _ => {
            if ctrl & TRB_IOC != 0 {
                1
            } else {
                2
            }
        }
    }
}

/// Digits for `depcmd_state`.
fn depcmd_state_digit(field: u32) -> u32 {
    let snap = unsafe { DATA_PHASE_SNAPSHOT };
    let ep0_cmd = snap[5];
    let ep1_cmd = snap[6];
    match field {
        0 => {
            if ep0_cmd & DEPCMD_CMDACT == 0 {
                1
            } else {
                2
            }
        }
        1 => {
            if (ep0_cmd >> 12) & 0xf == 0 {
                1
            } else {
                2
            }
        }
        2 => {
            if ep1_cmd & DEPCMD_CMDACT == 0 {
                1
            } else {
                2
            }
        }
        3 => {
            if ep0_cmd != 0xffff_ffff {
                1
            } else {
                2
            }
        }
        // "completed cleanly" needs both: the command is a STARTTRANSFER *and* bit 31 is clear.
        // A timed-out EP0 command is latched as 0x8000_0000, whose status nibble is 0, so the
        // nibble alone reports a timeout as a clean completion.
        _ => {
            if ep0_cmd & 0x0f == DEPCMD_STARTTRANSFER && ep0_cmd & 0x8000_0000 == 0 {
                1
            } else {
                2
            }
        }
    }
}

/// Digits for `global_block`.
fn global_block_digit(field: u32) -> u32 {
    match field {
        0 => {
            if unsafe { read(GCTL) } & GCTL_DSBLCLKGTNG != 0 {
                1
            } else {
                2
            }
        }
        1 => {
            if unsafe { read(GCTL) } & GCTL_CORESOFTRESET == 0 {
                1
            } else {
                2
            }
        }
        2 => {
            if unsafe { read(GEVNTSIZ0) } != 0 {
                1
            } else {
                2
            }
        }
        3 => {
            if unsafe { read(GEVNTADRLO0) } != 0 {
                1
            } else {
                2
            }
        }
        _ => {
            if unsafe { read(GEVNTCOUNT0) } == 0 {
                1
            } else {
                2
            }
        }
    }
}

/// Digits for `fail_state`.
fn fail_state_digit(field: u32) -> u32 {
    let snap = unsafe { DATA_PHASE_SNAPSHOT };
    let dctl = snap[0];
    let dsts = snap[1];
    match field {
        0 => {
            if dctl & DCTL_RUN_STOP == 0 {
                1
            } else {
                2
            }
        }
        1 => {
            if (dsts >> 18) & 0xf == 0 {
                1
            } else {
                2
            }
        }
        2 => {
            if snap[2] != 0 {
                1
            } else {
                2
            }
        }
        3 => {
            if snap[3] != 0 || snap[4] != 0 {
                1
            } else {
                2
            }
        }
        _ => {
            if dsts & DSTS_DEVCTRLHLT == 0 {
                1
            } else {
                2
            }
        }
    }
}

/// Digits for `data_cmd`.
fn data_cmd_digit(field: u32) -> u32 {
    let last = unsafe { DATA_PHASE_LAST_COMMAND };
    let status = (last >> 12) & 0xf;
    match field {
        0 => {
            if last & 0x8000_0000 != 0 {
                1
            } else {
                2
            }
        }
        1 => status.min(5) + 1,
        2 => {
            if last == 0xffff_ffff {
                1
            } else {
                2
            }
        }
        3 => {
            if unsafe { DATA_PHASE_PENDING_START } {
                1
            } else {
                2
            }
        }
        _ => {
            if unsafe { DATA_PHASE_PENDING_LEN } != 0 {
                1
            } else {
                2
            }
        }
    }
}

/// Digits for `depcmd`.
fn depcmd_digit(field: u32) -> u32 {
    let cmd1 = unsafe { read(dep_reg(1, 0x0c)) };
    let cmd0 = unsafe { read(dep_reg(0, 0x0c)) };
    match field {
        0 => {
            if cmd1 & DEPCMD_CMDACT != 0 {
                1
            } else {
                2
            }
        }
        1 => ((cmd1 >> 12) & 0xf).min(5) + 1,
        2 => {
            if (cmd1 >> DEPCMD_PARAM_SHIFT) & 0x7f != 0 {
                1
            } else {
                2
            }
        }
        _ => {
            if cmd0 & DEPCMD_CMDACT != 0 {
                1
            } else {
                2
            }
        }
    }
}

/// Digits for `data_report`.
fn data_report_digit(field: u32) -> u32 {
    let last = unsafe { SETUP_ARM_LAST_COMMAND };
    match field {
        0 => {
            if unsafe { DATA_PHASE_PENDING_LEN } != 0 {
                1
            } else {
                2
            }
        }
        1 => {
            if unsafe { DATA_PHASE_PENDING_START } {
                1
            } else {
                2
            }
        }
        2 => unsafe { super::ep0::ep0_state_digit() },
        3 => {
            let last = unsafe { SETUP_ARM_LAST_COMMAND };
            if last == 0xffff_ffff {
                2
            } else if (last >> 12) & 0xf == 0 {
                1
            } else {
                2
            }
        }
        4 => {
            if unsafe { SETUP_ARM_LAST_COMMAND } == 0xffff_ffff {
                1
            } else {
                2
            }
        }
        _ => (unsafe { super::ep0::control_data_trb_state() } as u32) + 1,
    }
}

/// Digits for `fresh_gates`.
fn fresh_gates_digit(field: u32) -> u32 {
    let (ctrl, size) = unsafe { super::ep0::setup_trb_words() };
    match field {
        0 => {
            if unsafe { super::ep0::setup_packet_pending() } {
                1
            } else {
                2
            }
        }
        1 => {
            if unsafe { EP0_STATE == Ep0State::Setup } {
                1
            } else {
                2
            }
        }
        2 => {
            if unsafe { EP0_SETUP_ARMED } {
                1
            } else {
                2
            }
        }
        3 => {
            if ctrl & 1 == 0 {
                1
            } else {
                2
            }
        }
        4 => {
            if (ctrl >> 4) & 0x3f == 2 {
                1
            } else {
                2
            }
        }
        5 => {
            if size & 0x00ff_ffff == 0 {
                1
            } else {
                2
            }
        }
        _ => {
            if size >> 28 == 0 {
                1
            } else {
                2
            }
        }
    }
}

/// Digits for `setup_buffer_word`.
fn setup_buffer_word_digit(field: u32) -> u32 {
    let b = unsafe { super::ep0::setup_bytes() };
    match field {
        0 => {
            if b[0] != 0 {
                1
            } else {
                2
            }
        }
        1 => {
            if b[0] == 0x80 {
                1
            } else {
                2
            }
        }
        2 => {
            if b[1] == 6 {
                1
            } else {
                2
            }
        }
        3 => {
            if b[3] == 1 {
                1
            } else {
                2
            }
        }
        _ => {
            if b.iter().all(|v| *v == 0) {
                1
            } else {
                2
            }
        }
    }
}

/// Digits for `response_bytes_word`.
fn response_bytes_word_digit(field: u32) -> u32 {
    let b = unsafe { response_bytes() };
    let any = b.iter().any(|v| *v != 0);
    match field {
        0 => {
            if b[0] == 18 {
                1
            } else {
                2
            }
        }
        1 => {
            if b[1] == 1 {
                1
            } else {
                2
            }
        }
        2 => {
            if b[8] == 0x34 && b[9] == 0x12 {
                1
            } else {
                2
            }
        }
        3 => {
            if b[10] == 0x01 && b[11] == 0x00 {
                1
            } else {
                2
            }
        }
        4 => {
            if unsafe { DATA_PHASE_PENDING_LEN } != 0 {
                1
            } else {
                2
            }
        }
        _ => {
            if any {
                1
            } else {
                2
            }
        }
    }
}

/// Digits for `observe`.
fn observe_digit(field: u32) -> u32 {
    match field {
        0 => {
            if unsafe { EP0_SETUP_ARMED } {
                1
            } else {
                2
            }
        }
        1 => (unsafe { control_data_trb_state() } as u32 + 1),
        2 => {
            if unsafe { data_phase_start_rejected() } {
                2
            } else {
                1
            }
        }
        _ => unsafe { ep0_state_digit() },
    }
}

/// `GEVNTCOUNT1`: `core.h` line 137 gives `DWC3_GEVNTCOUNT(n) = 0xc40c + n*0x10`,
/// so the second event buffer's count register is 0xc41c. Nothing else in this
/// project reads it; the word exists to tell "buffer 0 is stuck" apart from
/// "this register access never returns".
const GEVNTCOUNT1: usize = 0xc41c;

/// `GEVNTSIZ` bit 31 is the interrupt mask (`DWC3_GEVNTSIZ_INTMASK` in `core.h`),
/// kept as the third field because a masked ring and an unmasked one are
/// different states to be in while the count register refuses to answer.
const GEVNTSIZ_INTMASK: u32 = 1 << 31;

/// Digits for `event_count`: which of the two event buffers' count registers
/// answers at all.
///
/// This function carries a correction. Its declaration and its guards went into
/// `poll.rs` first while *this* readout did not, so the word published no fields
/// at all - and a run with no fields was read as "`GEVNTCOUNT1` wedges too",
/// which is not something an empty run can prove. A word that emits nothing is
/// an unverified instrument, not a negative result. The reading is retracted in
/// `boundary-state.md`. The wedging read stays last so the earlier fields would
/// survive it if it does wedge.
fn event_count_digit(field: u32) -> u32 {
    // Order matters and the doc says so: `GEVNTCOUNT0` is the read that never returns on this
    // handoff, so it is published *last*. It used to be field 1 of 3, which meant the third
    // field could never be observed in exactly the runs this word exists to diagnose.
    match field {
        0 => {
            if unsafe { read(GEVNTCOUNT1) } == 0 {
                1
            } else {
                2
            }
        }
        1 => {
            if unsafe { read(GEVNTSIZ0) } & GEVNTSIZ_INTMASK != 0 {
                1
            } else {
                2
            }
        }
        _ => {
            if unsafe { read(GEVNTCOUNT0) } == 0 {
                1
            } else {
                2
            }
        }
    }
}

/// Digits for `blackbox`: did this boot claim the reserved-RAM region, and are a
/// previous boot's records still readably there?
///
/// Field 1 is the load-bearing one: `boots() >= 2` can only hold if the header a
/// *previous* boot wrote was still in RAM after a reset, and that is the entire
/// premise of the black box. If a reset clears the region, field 1 stays at its
/// negative digit and the approach is dead before a single USB stage is
/// instrumented.
fn blackbox_digit(field: u32) -> u32 {
    match field {
        0 => {
            if crate::blackbox::ready() {
                1
            } else {
                2
            }
        }
        1 => {
            if crate::blackbox::boots() >= 2 {
                1
            } else {
                2
            }
        }
        2 => {
            if crate::blackbox::count() > 2 {
                1
            } else {
                2
            }
        }
        _ => {
            if crate::blackbox::contains(crate::blackbox::CODE_BOOT) {
                1
            } else {
                2
            }
        }
    }
}

/// Digits for `blackbox_read`: is there a record in the ring that a *different*
/// boot wrote?
///
/// `boots()` alone cannot answer it - a run can load the image twice and advance
/// the counter without any persistence at all, which is exactly what the first
/// attempt measured. Field 1 compares the boot number carried by the oldest
/// surviving `CODE_BOOT` record against this boot's own number: every record this
/// boot writes carries the same value, so a different one can only be a
/// survivor.
fn blackbox_read_digit(field: u32) -> u32 {
    match field {
        0 => {
            if crate::blackbox::oldest_boot_record_number().is_some() {
                1
            } else {
                2
            }
        }
        1 => {
            let oldest = crate::blackbox::oldest_boot_record_number();
            if oldest.is_some() && oldest != Some(crate::blackbox::boots() as u64) {
                1
            } else {
                2
            }
        }
        _ => {
            if crate::blackbox::boots() >= 2 {
                1
            } else {
                2
            }
        }
    }
}

/// Digits for `blackbox_stage`: the last few stage codes, newest first.
///
/// This is what the black box is actually for. Reading the whole ring back over
/// the Run/Stop channel is impractical - the calibration showed a symbol still
/// costs about a second because the host merges closely spaced pulses - but the
/// question "which stage was the last one reached" needs only a handful of small
/// numbers. Field 0 says whether any record exists at all, then one field per
/// record, newest first, each carrying the record's code (6 means "six or more",
/// since a blip run tops out at six).
///
/// Codes are the `CODE_*` constants in `crate::blackbox`: 1 boot, 2 reserved
/// region, 16 stage, 17-18 PHY enter/leave, 19 DWC3 reset, 20 EP0 arm, 21 setup
/// done, 22 data command, 23 data timeout, 24 poll timeout.
fn blackbox_stage_digit(field: u32) -> u32 {
    if field == 0 {
        return if crate::blackbox::previous_boot_recent(0).is_some() {
            1
        } else {
            2
        };
    }
    // Records of a *previous* boot, not this one. The newest entries in the ring
    // are this boot's own boot/region records, and reading those would answer
    // "where did the boot I am currently running stop", which is not the
    // question - the question is where the last one stopped.
    //
    // The code is shifted into 2..=6 (code modulo five, plus two) so that digit 1
    // can mean "no record here" unambiguously. The first version mapped a missing
    // record to 1 as well, and since 1 is also CODE_BOOT the two were
    // indistinguishable - which is exactly how a run that had no previous records
    // at all could be read as "the previous boot ended on its boot record".
    match crate::blackbox::previous_boot_recent(field - 1) {
        Some(entry) => (entry.code as u32) % 5 + 2,
        None => 1,
    }
}

/// Digits for `blackbox_recent`: the newest records of *this* boot, newest first.
///
/// Split out from `blackbox_stage` because the two questions have to be separable
/// while the ring is being trusted: "what did the boot before me write" and "what
/// has this boot written so far". Field 0 is whether anything was written at all,
/// then one field per record with the same shifted code encoding as
/// `blackbox_stage_digit()`.
fn blackbox_recent_digit(field: u32) -> u32 {
    if field == 0 {
        return if crate::blackbox::recent(0).is_some() {
            1
        } else {
            2
        };
    }
    match crate::blackbox::recent(field - 1) {
        Some(entry) => (entry.code as u32) % 5 + 2,
        None => 1,
    }
}
