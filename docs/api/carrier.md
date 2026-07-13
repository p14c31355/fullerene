# Carrier — Public Trait API (v0.1)

> **Status: DRAFT — Subject to Freeze**

---

## 1. Terminal — I/O Abstraction

`carrier::terminal::Terminal`

```rust
pub trait Terminal {
    fn write_str(&mut self, s: &str);
    fn read_byte(&mut self) -> Option<u8>;
    fn input_available(&self) -> bool { false }
    fn set_stdin(&mut self, _data: alloc::string::String) {}
    fn take_stdout(&mut self) -> Option<alloc::string::String> { None }
    fn take_stdin(&mut self) -> Option<alloc::string::String> { None }
    fn arm_pipe_stdout(&mut self) {}
    fn clear_pipe_stdin(&mut self) {}
}
```

**v0.1 freeze scope**: These 8 methods. Methods with default implementations are designed as extension points.

---

## 2. Pipe Mechanism

`Terminal` trait methods:

| Method | Role |
|---|---|
| `set_stdin()` | Provide pipe stdin or pass input to next pipeline stage |
| `arm_pipe_stdout()` | Enable pipe stdout |
| `take_stdout()` | Retrieve pipe buffer |
| `clear_pipe_stdin()` | Clear pipe stdin |

---

## 3. Command Dispatch

`carrier::exec` module:

```rust
pub fn dispatch(commands: &[&dyn Command], terminal: &mut dyn Terminal, line: &str) -> bool;
```

The final pipe stage writes directly to the terminal without buffering (streaming dispatch).

---

## Changelog

| Date | Change |
|---|---|
| 2026-07-13 | v0.1 initial |
