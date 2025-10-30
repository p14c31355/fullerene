- Use the isobemak crate for ISO creation tasks.
- The sub-crate "petroleum" handles UEFI-related tasks. Please make full use of it to reduce the amount of code.
- Also, if there are any bugs in "petroleum" itself, feel free to edit and fix them.

- Also, please minimize the use of asm macros. This also applies to unsafe code. However, do not include existing bootloader or UEFI crates in your dependencies. Implement your code using Rust core functions whenever possible.

- Furthermore, all subcrates are treated as default binary crates. This means that each subcrate must be configured in `Cargo.toml` so that it can be built and run as a standalone binary (e.g., by setting `crate-type = ["bin"]` or by making it directly executable).
- As an exception, the petroleum crate is the only crate that is treated as a library crate.
- Do not run the same command more than three times in a row. A "command" is any executable operation, such as a shell command, function call, or macro call. Avoid long, repetitive sequences of similar operations. For example, long, identical `port.write()` calls, such as those found in `graphics.rs`, should be refactored for clarity and conciseness, for example by using constants or helper functions.

- Use the `cargo clean && cargo run -q -p flasks` command to review your changes.

- Use helper functions, helper macros, structs, and type generics wherever possible to reduce lines of code.

- Consolidate redundant `.rs` files in appropriate places with minimal code.

- Consolidate all code duplicated between sub-crates into the `petroleum` sub-crate.

- Maximize the functionality of external crates other than `uefi` and `bootloader` to reduce lines of code.