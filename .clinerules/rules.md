# Fullerene Project Rules for Cline

## 1. Overall Policy (Highest Priority)
- **This project aims for a "safe, readable, and maintainable no_std OS kernel."**
- Minimize code size and thoroughly eliminate duplication.
- Minimize unsafe/asm. Maximize the use of Rust's core/alloc.
- Always write code that is "easy for later readers to understand."

## 2. Code Structure and Dependencies
- Actively utilize **petroleum** (common utilities, UEFI-related, serial output, etc.). Bugs can be fixed immediately.
- Treat other sub-crates (bellows, fullerene-kernel, etc.) as **binary crates** (appropriately configured in Cargo.toml).
- Actively utilize external crates other than `uefi` / `bootloader` crates to reduce code.
- Use isobemak for ISO creation.

## 3. Coding Style
- **Strictly adhere to the DRY principle**: Move duplicate code immediately to `petroleum`.
- Actively utilize helper functions, macros, generics, and structs to reduce the number of lines.
- Refactor long, repetitive operations (e.g., consecutive `port.write()` calls) into constants or helper functions.
- **Do not repeat the same command/operation more than 3 times consecutively** (refactoring is mandatory).
- Split files appropriately. However, merge redundant `.rs` files.

## 4. Unsafe / Low-Level Code
- Use **minimum** asm! macros and unsafe blocks.
- Implement with safe Rust + core libraries whenever possible.
- When using unsafe, clearly explain "why it's necessary" and "the basis for safety" with comments.

## 5. Testing and Verification Flow
- Always verify functionality with `cargo run -q -p flasks` after changes.

- Prioritize testing with QEMU.

## 6. Documentation and Comments
- Always include doc comments for important functions and structures.
- Update docs/ when the architecture changes.
- Be specific with TODOs.

## 7. Prohibited Actions
- Do not add new dependencies to existing bootloader/UEFI crates.
- Avoid unnecessary code duplication.
- Avoid long magic numbers/hardcode (prioritize constants).