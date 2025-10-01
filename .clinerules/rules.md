- For ISO creation tasks, use the isobemak crate.
  - The isobemak crate is located at 'home/dev/isobemak'.
  - Depending on the output of 'cargo run -p flasks', QEMU may fail to start. In such cases, you may edit the contents of 'home/dev/isobemak'.

- For UEFI-related tasks, the subcrate 'petroleum' handles this role. Use it actively to reduce code.
  - Also, if 'petroleum' itself has bugs, feel free to edit and fix them.

- Also, keep the use of asm macros to a minimum. The same applies to unsafe code. However, do not include the existing bootloader crate or uefi crate in your dependencies. Implement using Rust's core functions whenever possible.

- Additionally, all subcrates are treated as the default binary crate.