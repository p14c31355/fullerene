# Bramble run index

Use this small index to choose a targeted section from the full ledgers. It
is a routing view, not a replacement for the exact evidence.

| Evidence family | Representative runs / records | Current conclusion |
| --- | --- | --- |
| Direct USB2 reference control | `1700055.0`, `1137476.0` | HS attach, then zero-payload address-0 descriptor failure |
| USB2 PHY ownership / SUSPHY / timing | `1042386.0`, `1051403.0`, `1176368.0`, `256005.0`, `259639.0`, `294979.0`, `299942.0`, `312633.0`, `321699.0`, `330525.0`, `337851.0`, `342866.0`, `348929.0`, `363507.0`, `368463.0`, `374656.0`, `381615.0`, `385191.0` | SOF-gate, post-U0 PHY-retry, PHY-interface preservation, Android-order clock re-arm, HS core-clock, long-window raw-link checks, Type-C IRQ routing, internal event-queue readout, Run/Stop guard, pre-connect SETUP A/B, and corrected progress-readout timing still show no movement past the pre-descriptor boundary |
| DWC3 reset, Run/Stop, event, DBM, and gadget-start | `1285856.0`, `1305888.0`, `1379304.0`, `1398849.0`, `1412154.0`, `1533820.0`, `279359.0`, `283504.0`, `287432.0`, `291548.0`, `308523.0`, `317617.0`, `321699.0`, `330525.0`, `337851.0`, `342866.0`, `348929.0`, `353079.0`, `363507.0`, `368463.0`, `374656.0`, `381615.0`, `385191.0` | qpr1-style final-Run/Stop start, event/EP0/SOF progress diagnostics, Type-C parent-IRQ routing, raw-link-state timing gates, internal DWC3 queue readouts, Run/Stop guard, pre-connect SETUP A/B, and corrected progress-readout timing still show no usable Fullerene EP0 response |
| SuperSpeed / QMP / lane / Type-C variants | `1649885.0`, `166686.0`, `1693171.0` | No host-visible Fullerene SuperSpeed identity |
| Android-init / transient identity experiments | 2026-09-06 force-debuggable policy record | `1234:0001` appeared only after the prohibited Android configfs rebind |
| Normal Android-init handoff boundary | `96250.0`, `114346.0`, `133532.0`, `150913.0` | Post-DTB, pre-DTB, exception-traced pre-DTB, and the automated two-candidate replay all reached only Fastboot; no Fullerene attach; DTB ordering and the retained sync-exception trace did not yet discriminate |
| Candidate safety and recovery automation | `1941227.0`, `1967837.0`, `1999537.0`, `2179479.0`, `150913.0`, `256005.0`, `259639.0`, `279359.0`, `283504.0`, `287432.0`, `291548.0`, `294979.0`, `299942.0`, `308523.0`, `312633.0`, `317617.0`, `321699.0`, `330525.0`, `337851.0`, `342866.0`, `348929.0`, `353079.0`, `363507.0`, `368463.0`, `374656.0`, `381615.0`, `385191.0` | Absent-device gates, transient USB recovery retry, automatic ADB→Fastboot recovery, and RAM-only boot rules remain enforced |

For exact commands, timestamps, artifact hashes, source comparisons, and all
other runs, search [`CONTEXT_STATUS_FULL.md.gz`](CONTEXT_STATUS_FULL.md.gz) first;
use [`HARDWARE_aarch64_FULL.md.gz`](HARDWARE_aarch64_FULL.md.gz) for DT, ABL/XBL,
PHY, and hardware-ledger topics.
