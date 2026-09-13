# Bramble evidence archive

This directory contains the complete Bramble records moved out of the
default `docs/` context surface. Their evidence content is preserved; only
relative links were adjusted for the new location. They retain commands,
timestamps, hashes, source audits, and negative A/B results.

Use these compact entry points first:

- [`../../docs/CONTEXT_STATUS.md`](../../docs/CONTEXT_STATUS.md) — current state and routing
- [`../../docs/HARDWARE_aarch64.md`](../../docs/HARDWARE_aarch64.md) — compact hardware contract

Archives (gzip-compressed to stay outside the default Markdown context):

- [`RUN_INDEX.md`](RUN_INDEX.md) — compact family-to-run routing index
- [`CONTEXT_STATUS_FULL.md.gz`](CONTEXT_STATUS_FULL.md.gz) — historical status ledger
- [`HARDWARE_BRAMBLE_SUMMARY_FULL.md.gz`](HARDWARE_BRAMBLE_SUMMARY_FULL.md.gz) — legacy cross-platform Bramble table
- [`HARDWARE_aarch64_FULL.md.gz`](HARDWARE_aarch64_FULL.md.gz) — complete AArch64 hardware ledger

Inspect without unpacking, for example:

`gzip -dc evidence/bramble/CONTEXT_STATUS_FULL.md.gz | less`

Restore a plaintext copy only when needed with `gzip -dk <archive>.gz`.

Do not load the decompressed archive files wholesale into an LLM context. Retrieve a
targeted Run ID, source-audit topic, or line range when exact evidence is
needed.
