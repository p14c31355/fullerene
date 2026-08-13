# Haber–Bosch daemon (HBD)

HBD is Nitrogen's hardware-state convergence layer. Nitrogen drivers own
hardware mechanisms—MMIO, DMA, rings, firmware commands, and port resets—while
HBD owns policy: it observes the current state, evaluates explicit constraints,
selects a safe next action, and observes again.

The solver is deliberately small and deterministic for the `no_std` runtime.
Every solve has finite `max_actions`, `max_resets`, and `max_retries` budgets.
`Satisfied`, `Unsatisfied`, `Unknown`, and `Violated` are distinct. A violated
specification constraint terminates the solve and is reported instead of being
hidden by an unbounded retry loop.

## Backend boundaries

`nitrogen::hbd` contains the generic solver, action/transition model, constraint
status, and report format. The xHCI backend adapts `XhciContext` observations:
controller capability/running state, root-port `PORTSC`, USB2/USB3 protocol,
link/speed, devices, root-port/hub ancestry, routes, endpoints, and canonical
logical paths. Its actions call the existing bounded xHCI port polling and
reset mechanisms.

The iwlwifi backend observes the existing incremental initialization phase and
the public Wi-Fi manager snapshot. Firmware/device discovery, firmware-ready
state, and link state remain observations; unsupported or absent phases are not
invented by the generic solver.

## Shell interface

From the Fullerene shell:

```text
hbd status
hbd solve xhci
hbd solve iwlwifi
hbd solve all
hbd report
```

`solve all` collects independent reports. A failure in one backend does not
discard the report from another. `status` is read-only; `report` returns the
latest compact machine-readable summaries, while solve output includes the
human-readable stage, constraints, actions, and transitions.

HBD is post-boot diagnostic/convergence policy. Driver failure does not make
desktop boot contingent on HBD success.
