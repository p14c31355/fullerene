# Fullerene Memory and I/O Management Inventory

This document records the memory-management and related file-I/O designs that
have existed in Fullerene, and distinguishes the current implementation from
the Linux-like design that is still planned.  It is intentionally an inventory
of behavior and boundaries, not a claim that every future design described
here is already implemented.

## Historical designs

### Fixed contiguous heap

The original kernel allocator was a `linked_list_allocator` over one fixed
contiguous region.  The initial region was increased over time as GUI, WASM,
JPEG, and filesystem features needed more working memory.  A large allocation
could fail even when the sum of free blocks was sufficient, because the
allocator required one suitably sized contiguous hole.

The old extension API, `extend_global_heap`, was an explicit operation.  Callers
had to know when a larger region was needed and had to ensure that the memory
after the allocator's current top was valid and mapped.  It did not provide a
general allocation-failure recovery policy.

### Pre-check and manual JPEG growth

At one point JPEG decoding was handled by checking the image header, estimating
the decoded buffer size, and explicitly extending the kernel heap before
decoding.  This reduced predictable JPEG failures, but it coupled a subsystem
decision to the global allocator and did not protect other decoders or VFS
operations from fragmentation.

### Static extension backing

The kernel later moved to one zero-initialized `TOTAL_HEAP_BUFFER`.  The intent
was to keep the extra bytes in the PE/UEFI zero-filled image area instead of
occupying ISO file bytes.  BIOS and UEFI initialization did not originally use
exactly the same backing path; this was a source of platform-specific risk and
has now been unified.

### Buffered file operations

Older copy and WASM file paths often read an entire file into a `Vec<u8>` and
wrote it in one operation.  This made a filesystem operation depend on the
kernel heap and imposed artificial file-size limits.  It also made removable
media failures look like `DiskFull` when the real problem was an intermediate
buffer allocation.

## Current implementation

### Kernel heap sizes

The current constants are:

```text
initial committed heap:  12 MiB
automatic extension:    128 MiB maximum
reserved backing total: 140 MiB
```

The initial 12 MiB is published in `HEAP_END` during boot.  The extension is
not counted as usable heap until a growth transaction succeeds.

`petroleum::page_table::heap::GrowingHeap` wraps the linked-list allocator.
Its allocation policy is deliberately bounded:

1. Try the allocation in the currently exposed heap.
2. If it fails, calculate one page-rounded extension request.
3. Extend once, verify that the allocator top and size actually advanced.
4. Retry the allocation once.
5. Return null if the extension is exhausted or made no progress.

There is no allocator-side retry loop.  The allocation error handler is a
kernel-fatal fallback and does not attempt to allocate or retry again.

### Page-fault continuation

The reserved extension backing is adjacent to the initial heap.  In the idle
kernel/shell context, a non-present page fault inside that narrowly validated
extension range may map the corresponding backing page and return from the
fault.  The CPU then resumes the faulting instruction.  This is the intended
continuation model: page-fault recovery, not an unbounded allocator loop.

The recovery path is intentionally conservative:

- it only accepts the reserved heap-extension address range;
- it only runs for the idle shell context;
- it uses non-blocking memory-manager acquisition;
- it refuses recovery if the manager lock is already held;
- it leaves user-process and ambiguous kernel faults on the termination/halt
  path.

The current implementation is not yet a general Linux-style per-process
virtual-memory allocator.  The backing is still a kernel-owned contiguous
range, and the page-fault continuation is not a user-process heap service.

### Fault containment and the “last safe footprint”

CPU faults in user mode (`#PF`, `#GP`, `#UD`, divide faults, and similar
exceptions) are not resumed.  The exception frame is recorded as a
`FaultRecord` containing the reason, RIP, RSP, fault address, and error code.
The process is marked terminated, the recovery trampoline switches to a
healthy scheduler context, and normal process cleanup releases its resources.

This is a recovery boundary, not UB detection.  Undefined behavior that does
not produce a hardware or runtime fault cannot be reliably detected by the
kernel.

### WASM memory and error boundaries

The WASM viewer runs synchronously in the kernel shell and uses the global
kernel allocator for host-side `wasmi`, VFS callback, and compositor work.  It
currently has bounded media policies, including image byte/pixel limits and a
64 KiB host file-read cache.  JPEG/QOI decoding is performed in the WASM
viewer, while `show_image` copies a bounded RGB result into the compositor.

Error numbers must not be compared across layers without translation:

| Layer | Example | Meaning of `28` |
|---|---|---|
| Fullerene `SystemError` | `DiskFull` | disk/full target resource |
| Linux errno | `ENOSPC` | no space left on device |
| WASI errno | `EINVAL` | invalid argument |

Therefore a log saying “OS error 28” is incomplete unless it also identifies
the ABI layer.  `FsError::InvalidInput` is translated to WASI `EINVAL`, while
`FsError::DiskFull` is translated to WASI `ENOSPC` and Linux `ENOSPC`.

### VFS and CP/MV context

The canonical VFS is the `VfsContext` inside `KernelContext`.  Free functions
in `fullerene-kernel::contexts::vfs` route through that one context; they are
not a second filesystem instance.  File copying uses bounded streaming I/O
and should resolve an existing destination directory to
`destination/source_basename`.

The current code still has two related operation surfaces, but they now share
the same destination resolution and streaming semantics:

- `copy_path` free-function routing, which performs destination-directory
  resolution and streams between mounts;
- `VfsContext::copy_path`/`move_path`, which are the lower-level methods used
  by that routing and also resolve an existing destination directory.

CP and MV regressions must still be tested together, including same-mount,
cross-mount, destination-directory, replacement, and recursive-directory
cases, because both commands depend on the same copy-then-remove contract.

## Target direction

The Linux-like target design is:

```text
virtual heap reservation
        |
        +-- initially mapped pages
        +-- page fault -> allocate/map one page -> resume instruction
        +-- reservation exhausted -> domain-specific OOM
```

The longer-term allocator should separate ownership domains, for example:

```text
frame allocator
  +-- kernel heap
  +-- WASM instance arena
  +-- file cache
  +-- GUI/GPU buffers
  +-- DMA pool
```

That separation is required before a WASM or decoder OOM can safely terminate
only the offending application.  Until then, global kernel allocation failure
remains fatal by design, and callers must enforce bounded inputs before doing
large decodes or buffered writes.

## Validation checklist

Any future memory or VFS change should verify:

- boot reports 12 MiB committed heap, not the 140 MiB reservation;
- a successful extension advances `HEAP_END` exactly once;
- an exhausted extension returns without retrying forever;
- a recoverable idle kernel heap PF resumes the faulting instruction;
- a user fault records its trap frame and reaps the process;
- JPEG, QOI, and MP4 errors identify their ABI layer;
- `cp` and `mv` agree on destination-directory semantics;
- cross-mount copies preserve source bytes and do not depend on a whole-file
  allocation.
