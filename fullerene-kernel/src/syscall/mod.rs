pub mod abi;
pub mod cap;
pub mod device;
pub mod dispatch;
pub mod event;
pub mod fs;
pub mod ipc;
pub mod memory;
pub mod process;
pub mod thread;
pub mod time;
pub mod types;
pub mod window;

pub mod interface;
pub mod user;

// Re-export public API for backward compatibility
pub use dispatch::*;
pub use interface::*;
pub use types::*;

#[cfg(test)]
mod tests {
    #[test]
    fn test_syscall_numbers() {
        assert_eq!(fullerene_abi::SyscallNumber::Exit.as_u64(), 1);
        assert_eq!(fullerene_abi::SyscallNumber::Write.as_u64(), 4);
        assert_eq!(fullerene_abi::SyscallNumber::Read.as_u64(), 3);
    }

    #[test]
    fn abi_query_preserves_the_version_only_call() {
        assert_eq!(
            super::abi::syscall_abi_query(core::ptr::null_mut(), 0),
            Ok(fullerene_abi::AbiVersion::CURRENT.pack())
        );
    }
}

#[cfg(test)]
mod support_matrix {
    use alloc::vec::Vec;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Support {
        Full,
        Partial,
        Stub,
    }

    struct SyscallInfo {
        number: u64,
        name: &'static str,
        support: Support,
        notes: &'static str,
    }

    const SYSCALLS: &[SyscallInfo] = &[
        SyscallInfo {
            number: 0,
            name: "abi_query",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 1,
            name: "exit",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 2,
            name: "fork",
            support: Support::Full,
            notes: "COW page tables",
        },
        SyscallInfo {
            number: 3,
            name: "read",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 4,
            name: "write",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 5,
            name: "open",
            support: Support::Full,
            notes: "read-only only",
        },
        SyscallInfo {
            number: 6,
            name: "close",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 7,
            name: "wait",
            support: Support::Partial,
            notes: "non-blocking only",
        },
        SyscallInfo {
            number: 20,
            name: "getpid",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 21,
            name: "get_process_name",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 22,
            name: "yield",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 30,
            name: "map_memory",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 31,
            name: "unmap_memory",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 32,
            name: "protect_memory",
            support: Support::Stub,
            notes: "returns NotSupported",
        },
        SyscallInfo {
            number: 33,
            name: "query_memory",
            support: Support::Stub,
            notes: "returns empty data",
        },
        SyscallInfo {
            number: 40,
            name: "create_event",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 41,
            name: "wait_event",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 42,
            name: "signal_event",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 43,
            name: "subscribe_event",
            support: Support::Stub,
            notes: "returns NotSupported",
        },
        SyscallInfo {
            number: 50,
            name: "create_thread",
            support: Support::Full,
            notes: "shares page table with parent",
        },
        SyscallInfo {
            number: 51,
            name: "join_thread",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 52,
            name: "detach_thread",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 53,
            name: "exit_thread",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 60,
            name: "create_window",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 61,
            name: "destroy_window",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 62,
            name: "resize_window",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 63,
            name: "present_window",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 64,
            name: "get_window_event",
            support: Support::Stub,
            notes: "returns empty data",
        },
        SyscallInfo {
            number: 70,
            name: "enumerate_devices",
            support: Support::Partial,
            notes: "PCI only",
        },
        SyscallInfo {
            number: 71,
            name: "open_device",
            support: Support::Stub,
            notes: "returns handle but no real device",
        },
        SyscallInfo {
            number: 72,
            name: "device_ioctl",
            support: Support::Stub,
            notes: "returns NotSupported",
        },
        SyscallInfo {
            number: 80,
            name: "channel_create",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 81,
            name: "channel_send",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 82,
            name: "channel_recv",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 83,
            name: "pipe_create",
            support: Support::Full,
            notes: "uses user buffer for handles",
        },
        SyscallInfo {
            number: 90,
            name: "handle_transfer",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 91,
            name: "handle_duplicate",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 92,
            name: "handle_revoke",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 100,
            name: "clock_gettime",
            support: Support::Full,
            notes: "MONOTONIC only",
        },
        SyscallInfo {
            number: 101,
            name: "timer_create",
            support: Support::Full,
            notes: "",
        },
        SyscallInfo {
            number: 102,
            name: "sleep",
            support: Support::Partial,
            notes: "busy-wait for <1ms",
        },
        SyscallInfo {
            number: 103,
            name: "uptime",
            support: Support::Full,
            notes: "",
        },
    ];

    #[test]
    fn syscall_numbers_are_unique() {
        let mut seen: Vec<u64> = Vec::with_capacity(SYSCALLS.len());
        for info in SYSCALLS {
            assert!(
                !seen.contains(&info.number),
                "duplicate syscall number: {}",
                info.number
            );
            seen.push(info.number);
        }
    }

    #[test]
    fn syscall_numbers_are_sorted() {
        let mut prev: Option<u64> = None;
        for n in SYSCALLS.iter().map(|s| s.number) {
            if let Some(p) = prev {
                assert!(
                    p < n,
                    "syscall numbers must be strictly increasing: {} >= {}",
                    p,
                    n
                );
            }
            prev = Some(n);
        }
    }

    #[test]
    fn count_full() {
        let full = SYSCALLS
            .iter()
            .filter(|s| s.support == Support::Full)
            .count();
        assert!(full > 20, "expected at least 20 fully implemented syscalls");
        assert!(SYSCALLS.iter().all(|s| !s.name.is_empty()));
        assert!(
            SYSCALLS
                .iter()
                .filter(|s| s.support != Support::Full)
                .all(|s| !s.notes.is_empty())
        );
    }
}
