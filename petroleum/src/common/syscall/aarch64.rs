//! The ANE has no userspace syscall instruction wired yet.
//!
//! The existing user lands (toluene/ligand) call the x86 side's
//! `common::syscall::syscall`; nothing on the ANE calls into this module
//! today, so there is nothing to provide here.
