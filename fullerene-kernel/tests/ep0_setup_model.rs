//! Differential state model for the ANE DWC2 EP0 SETUP answering path.
//!
//! Reproduces the pop_rx/service_ep0 control flow of
//! `platform/ane/ane_usb_dev.rs` (8e9f11cf vs current) as a pure state
//! machine over two observable wire orderings the core can surface:
//!
//! A. RXFIFO SETUPRX arrives first; DOEPINT0.SETUP latches in a LATER pass.
//! B. DOEPINT0.SETUP latches first (bytes already staged); RX pop later.
//! C. both in the SAME pass (the coalesced case that the v204 comment
//!    describes as producing the double-answer).
//!
//! Invariant under test: exactly ONE `handle_setup` answer per distinct
//! SETUP packet, and the IN FIFO never receives a second copy of the same
//! payload. `SETUP_BYTES_READY` must not be observable after the RX-side
//! answer cleared it (the current-tree guard), and the belt must never
//! answer when `SETUP_HANDLED` is true.

/// Mirrors the statics used by the real driver (single-core, plain bools).
#[derive(Default)]
struct Ep0 {
    setup_armed: bool,
    setup_bytes_ready: bool,
    setup_handled: bool,
    transaction_busy: bool,
    /// Tracks the driver's `IN_WAS_DATA`: true while the armed IN is a data
    /// phase, false once the armed IN is the zero-length status. The driver
    /// flips it in ep0_send() and consumes it at IN completion.
    in_was_data: bool,
    doepint_setup_latched: bool,
    diepint_xfercompl_latched: bool,
    doepint_xfercompl_latched: bool,
    answers: Vec<u16>,
}

/// One wire-visible SETUP event (each packet gets a fresh id).
enum Ev {
    SetupRx(u16),
    DoepSetupLatch,
    InXferCompl,
    OutXferCompl,
}

impl Ep0 {
    /// Current-tree pop_rx SETUPRX branch: answer immediately on first RX.
    /// `clear_bytes` = the 07:0x current-tree guard (SETUPRX clears
    /// SETUP_BYTES_READY after answering); false = the 8e shape.
    fn rx(&mut self, id: u16, clear_bytes: bool) {
        self.setup_armed = false;
        self.setup_bytes_ready = true;
        if !self.setup_handled {
            self.setup_handled = true;
            // latch clear + CNAK ordering is irrelevant to the model (both
            // A/B variants failed on hardware in this same shape).
            self.doepint_setup_latched = false;
            self.answer(id);
            if clear_bytes {
                self.setup_bytes_ready = false;
            }
        }
    }

    /// service_ep0 pass over latched endpoint interrupts.
    fn service(&mut self, id: u16) {
        if self.diepint_xfercompl_latched {
            self.diepint_xfercompl_latched = false;
            // The real driver treats IN completion as: data phase done ->
            // arm status OUT; zero-length status done -> transfer complete.
            // Model both by the in_was_data flag as the driver does.
            if self.in_was_data {
                self.in_was_data = false;
                // status OUT armed; not a window reopen yet
            } else {
                self.transaction_busy = false;
                self.setup_handled = false; // the window reopens
            }
        }
        if self.doepint_xfercompl_latched {
            self.doepint_xfercompl_latched = false;
            self.transaction_busy = false;
            self.setup_handled = false;
        }
        if self.doepint_setup_latched {
            self.doepint_setup_latched = false;
            // belt: answer only if bytes present AND not already handled.
            if self.setup_bytes_ready && !self.setup_handled {
                self.setup_handled = true;
                self.answer(id);
            }
        }
    }

    fn answer(&mut self, id: u16) {
        self.transaction_busy = true;
        self.in_was_data = true;
        self.answers.push(id);
    }
}

fn run(ordering: &str, events: &[Ev]) -> Vec<u16> {
    let mut ep0 = Ep0::default();
    for ev in events {
        match *ev {
            Ev::SetupRx(id) => ep0.rx(id, true),
            Ev::DoepSetupLatch => ep0.doepint_setup_latched = true,
            Ev::InXferCompl => ep0.diepint_xfercompl_latched = true,
            Ev::OutXferCompl => ep0.doepint_xfercompl_latched = true,
        }
        ep0.service(1);
    }
    // One drain pass for anything latched during the last event.
    ep0.service(1);
    println!("{ordering}: answers = {:?}", ep0.answers);
    ep0.answers
}

#[track_caller]
fn expect_one(answers: &[u16], ordering: &str) {
    assert_eq!(
        answers.len(),
        1,
        "{ordering}: expected exactly one answer, got {answers:?}"
    );
    assert_eq!(answers[0], 1, "{ordering}: wrong setup answered");
}

#[test]
fn a_rx_then_latch_next_pass() {
    // SETUPRX answers now; DOEPINT0.SETUP latches afterwards (stale belt).
    let answers = run(
        "A rx->later-latch",
        &[
            Ev::SetupRx(1),
            Ev::InXferCompl, // status-stage done: window reopens
            Ev::DoepSetupLatch,
        ],
    );
    expect_one(&answers, "A");
}

#[test]
fn a_latch_same_pass_as_rx() {
    let answers = run(
        "A coalesced",
        &[Ev::SetupRx(1), Ev::DoepSetupLatch, Ev::InXferCompl],
    );
    expect_one(&answers, "A-coalesced");
}

#[test]
fn b_latch_before_rx() {
    // The DOEPINT0.SETUP interrupt latches first; bytes arrive next pass.
    let answers = run(
        "B latch->rx",
        &[
            Ev::DoepSetupLatch, // bytes not present yet: belt must not fire
            Ev::SetupRx(1),
            Ev::InXferCompl,
            Ev::DoepSetupLatch, // stale latch AFTER the answer (the v204 case)
        ],
    );
    expect_one(&answers, "B");
}

#[test]
fn b_latch_before_rx_without_current_guard() {
    // The 8e shape (no SETUP_BYTES_READY clear after the RX-side answer):
    // the double answer requires the belt to consume the stale SETUP latch
    // AFTER the window reopens. Two wire orderings can do that:
    //   1. IN data-phase completion, then IN status completion (window
    //      reopens), then the stale DOEPINT0.SETUP latch - each in its own
    //      service pass.
    //   2. The OUT (status-stage) completion reopening the window and the
    //      stale SETUP latch in the same pass.
    // Ordering 1 is the realistic GET_DESCRIPTOR flow: IN(18B) -> status
    // OUT -> DOEPINT0.XFERCOMPL both reopen the window. Model it exactly:
    let mut ep0 = Ep0::default();
    ep0.doepint_setup_latched = true;
    ep0.service(1); // belt cannot fire: no bytes
    // 8e shape: the RX-side answer does NOT clear SETUP_BYTES_READY; only
    // the next arm_setup_receive() (via service()) would, and that is
    // blocked while TRANSACTION_BUSY - so no re-arm runs between the
    // answer and the status completion.
    ep0.rx(1, false); // RX answers; in 8e setup_bytes_ready stays true
    assert!(
        ep0.setup_bytes_ready,
        "8e keeps the stale bytes latch set after answering"
    );
    ep0.diepint_xfercompl_latched = true;
    ep0.service(1); // IN data-phase done -> status OUT armed (in_was_data false now)
    ep0.diepint_xfercompl_latched = true;
    ep0.service(1); // IN status done -> window reopens (setup_handled=false)
    assert!(
        ep0.setup_bytes_ready,
        "no re-arm happened while TRANSACTION_BUSY; bytes latch still set"
    );
    assert!(!ep0.setup_handled, "window reopened by the status IN");
    ep0.doepint_setup_latched = true; // the stale SETUP latch surfaces now
    ep0.service(1);
    println!("B-8e-shape: answers = {:?}", ep0.answers);
    assert_eq!(
        ep0.answers.len(),
        2,
        "8e shape must reproduce the double answer for the belt hypothesis"
    );
}

#[test]
fn reset_clears_all_latches() {
    // handle_reset clears every latch; a later stale latch is real hardware
    // behaviour the model cannot dismiss.
    let mut ep0 = Ep0::default();
    ep0.rx(1, true);
    // reset:
    ep0.transaction_busy = false;
    ep0.setup_handled = false;
    ep0.setup_bytes_ready = false;
    ep0.setup_armed = true;
    ep0.doepint_setup_latched = true; // stale
    ep0.service(1);
    assert_eq!(
        ep0.answers.len(),
        1,
        "post-reset stale latch answered twice"
    );
}

fn main() {
    // Standalone compile for host inspection; tests carry the assertions.
    println!("ep0 model: cargo test -p fullerene-kernel --test ep0_setup_model");
}
