# Carrier — Public Trait API (v0.1)

> **Status: DRAFT — 凍結予定**

---

## 1. Terminal — I/O抽象

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

**v0.1 凍結範囲**: この8メソッド。デフォルト実装を持つメソッドは拡張ポイントとして設計されている。

---

## 2. パイプ機構

`Terminal` trait methods:

| メソッド | 役割 |
|---|---|
| `arm_pipe_stdout()` | パイプ stdout を有効化 |
| `take_stdout()` | パイプバッファを取得 |
| `clear_pipe_stdin()` | パイプ stdin をクリア |

---

## 3. コマンドディスパッチ

`carrier::exec` モジュール:

```rust
pub fn dispatch(commands: &[&dyn Command], terminal: &mut dyn Terminal, line: &str) -> bool;
```

最終パイプステージはバッファリングなしで直接terminalに書き込む (ストリーミング dispatch)。

---

## 変更履歴

| 日付 | 変更 |
|---|---|
| 2026-07-13 | v0.1 初版 |
