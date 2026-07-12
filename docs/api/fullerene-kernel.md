# Fullerene Kernel — Public API (v0.1)

> **Status: DRAFT — 凍結予定**
>
> カーネルがユーザー空間および他のクレートに公開するABIとデータ構造。

---

## 1. システムコールABI

`petroleum::common::syscall::*`

| 呼び出し規約 |
|---|
| `syscall` 命令 (x86-64) |
| rax = syscall number, rdi/rsi/rdx/r10/r8/r9 = args |
| 戻り値: rax, エラーは rax にエンコード |

### Syscall numbers

`petroleum/src/common/syscall.rs`:

| # | Name | 説明 |
|---|------|------|
| 0 | `Uptime` | システム起動からのµs |
| 1 | `GetPid` | 現在のプロセスPID |
| 2 | `ClockGetTime` | 壁時計時刻 |
| 3 | `Exit` | プロセス終了 |
| 4 | `Write` | fdへの書き込み |
| 5 | `Read` | fdからの読み取り |
| 6 | `Open` | ファイルを開く |
| 7 | `Close` | fdを閉じる |
| 8 | `Spawn` | 新プロセス生成 |
| 9 | `WaitPid` | 子プロセス完了待ち |
| 10 | `Mmap` | メモリマッピング |
| 11 | `Munmap` | マッピング解除 |
| 12 | `SchedYield` | 明示的CPU譲渡 |
| 13 | `CreateThread` | スレッド作成 |
| 14 | `ExitThread` | スレッド終了 |
| 15 | `SendEvent` | イベント送信 |
| 16 | `RecvEvent` | イベント受信 |

### エラー体系

負の戻り値 = エラー (EINVAL, ENOENT, EACCES, ENOMEM, EAGAIN, ...)。

---

## 2. VDSO (Read-Only Metadata Page)

`0x7000_0000_0000` に固定的にマップ。ゼロコピーで読み取り専用のカーネルメタデータにアクセス可能。

| Offset | 型 | 内容 |
|---|---|---|
| 0 | `AtomicU64` | time_us — 壁時計 (µs) |
| 8 | `AtomicU64` | uptime_us — 起動からの経過時間 (µs) |
| 16 | `u64` | pid — 現在のプロセスPID |

カーネルは `Ordering::Release` で書き込み、ユーザー空間は `Ordering::Acquire` で読み取り。

---

## 3. プロセス管理

### Process

`fullerene-kernel::process::Process`

```rust
pub struct Process {
    pub pid: u64,
    pub state: ProcessState,
    pub name: String,
    pub registers: [u64; 32],
    pub page_table: PhysAddr,
    // ... (内部実装)
}
```

### SchedulerContext

`fullerene-kernel::scheduler_context::SchedulerContext`

```rust
pub static SCHEDULER: spin::Mutex<SchedulerContext>;
```

`SCHEDULER` は `KERNEL` ロックから独立した唯一のグローバル。

---

## 4. Klog

| マクロ | 説明 |
|---|---|
| `klog_fmt!(fmt, ...)` | カーネルログ出力 (framebuffer + serial) |
| `boot_stage!(BootStage::X)` | ブート段階マーカー (パニック画面の色) |

---

## 変更履歴

| 日付 | 変更 |
|---|---|
| 2026-07-13 | v0.1 初版 |
