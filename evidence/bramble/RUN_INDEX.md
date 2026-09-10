# Bramble run index

Use this small index to choose a targeted section from the full ledgers. It
is a routing view, not a replacement for the exact evidence.

| Evidence family | Representative runs / records | Current conclusion |
| --- | --- | --- |
| Direct USB2 reference control | `1700055.0`, `1137476.0` | HS attach, then zero-payload address-0 descriptor failure |
| USB2 PHY ownership / SUSPHY / timing | `1042386.0`, `1051403.0`, `1176368.0` | No movement past the pre-descriptor boundary |
| DWC3 reset, Run/Stop, event, DBM, and gadget-start | `1285856.0`, `1305888.0`, `1379304.0`, `1398849.0`, `1412154.0`, `1533820.0` | No usable Fullerene EP0 response |
| SuperSpeed / QMP / lane / Type-C variants | `1649885.0`, `166686.0`, `1693171.0` | No host-visible Fullerene SuperSpeed identity |
| Android-init / transient identity experiments | 2026-09-06 force-debuggable policy record | `1234:0001` appeared only after the prohibited Android configfs rebind |
| Candidate safety and recovery automation | `1941227.0`, `1967837.0`, `1999537.0`, `2179479.0` | Absent-device gates and RAM-only boot rules remain enforced |

For exact commands, timestamps, artifact hashes, source comparisons, and all
other runs, search [`CONTEXT_STATUS_FULL.md`](CONTEXT_STATUS_FULL.md) first;
use [`HARDWARE_aarch64_FULL.md`](HARDWARE_aarch64_FULL.md) for DT, ABL/XBL,
PHY, and hardware-ledger topics.
