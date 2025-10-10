- For ISO creation tasks, use the isobemak crate.
- For UEFI-related tasks, the subcrate 'petroleum' handles this role. Use it actively to reduce code.
  - Also, if 'petroleum' itself has bugs, feel free to edit and fix them.

- Also, keep the use of asm macros to a minimum. The same applies to unsafe code. However, do not include the existing bootloader crate or uefi crate in your dependencies. Implement using Rust's core functions whenever possible.

- Additionally, all subcrates are treated as the default binary crate. This implies that each subcrate should be configured in its `Cargo.toml` to be buildable and runnable as a standalone binary (e.g., by setting `crate-type = ["bin"]` or ensuring it can be executed directly).
- Do not execute the same command three or more times consecutively. A "command" refers to any executable operation, including shell commands, function calls, or macro invocations. Avoid long, repetitive sequences of similar operations. For example, a long series of identical `port.write()` calls, as seen in `graphics.rs`, should be refactored for clarity and conciseness, perhaps by using constants or helper functions.

- To check the changes, use the `cargo clean && cargo run -p flasks` command.

- Use helper functions, helper macros, structures, and type generics wherever possible to keep the number of lines of code low.

- Let's move forward by integrating redundant .rs files with extremely few lines of code into appropriate locations.
