# Stretch Goal Runtime Contracts

Fullerene supports large applications through an explicit package/port
contract instead of embedding third-party binaries in the kernel repository.
This keeps licensing, updates, and binary provenance reviewable.

## Installing and launching ports

The Nozzle shell exposes the production package flow:

```text
app catalog
app install <catalog-name> <elf-path>
app list
app run <catalog-name>
app remove <catalog-name>
```

Installation verifies a 64-bit ELF image and records its runtime (`native` or
`linux`) in `/packages/<name>/manifest.txt`. Launching dispatches to the native
isolated ELF loader or Linux compatibility runtime as declared by the catalog.

The catalog includes FREEDOOM, Fullerene Present, NetSurf, KDE Plasma and Xfce
sessions, VSCodium, Cargo, and rustc.

The repository intentionally does not redistribute these projects' release
binaries or game data. A build/release pipeline may place reviewed artifacts
on a mounted volume and install them with the commands above.

## Self-hosted workflows

Self-hosted compiling uses the `cargo` and `rustc` Linux-ELF ports. Presentation
and coding workflows use `fullerene-present` and `vscodium`. Desktop
environments are selected by launching an installed session package, so more
environments can be added without linking their policy into Solvent.

## SMP

Nitrogen parses legacy Local APIC and x2APIC MADT entries and exposes the
architectural INIT-SIPI-SIPI sequence. The kernel records discovered and online
processors through `smp`, and `cpuinfo` reports both values. APs only become
online after their platform trampoline calls `mark_processor_online`; this
prevents the scheduler from claiming CPUs that firmware or a trampoline failed
to start.

## Runtime observability

`metrics` and the graphical System Info window report boot duration, latest and
maximum frame time, heap current/high-water usage, and DMA
current/high-water usage. The Log Viewer displays the kernel log ring without a
serial console.
