# Fullerene ソース全体の改善案（2026-07-04）

## 目的

Fullerene の Rust ソース全体を横断し、安全性、プロセス分離、責務分割、テスト容易性、再現可能性の観点から今後の改善案を整理した。

この文書は実装済み変更の記録ではなく、今後の改修候補と推奨順序を示す。行数が多いという理由だけでは分割せず、所有権、ライフサイクル、同期範囲、テスト境界が異なる箇所を分離対象としている。

## 調査範囲と概況

- 対象: workspace 内の Rust ソース 299 ファイル
- 合計: 約 73,720 行
- 対象クレート: `fullerene-kernel`、`nitrogen`、`petroleum`、`lattice`、`solvent`、`bonder`、`nozzle`、`toluene`、`resonance`、`genome`、`bellows`、`flasks`、`chronoline`、`carrier`
- 除外: バイナリアセット、ファームウェア本体、OVMF、生成物、submodule 内部

### 規模とテストの偏り

| クレート | Rust ファイル | 行数 | `unsafe` 系の出現目安 | `#[test]` |
|---|---:|---:|---:|---:|
| `nitrogen` | 56 | 19,099 | 397 | 46 |
| `fullerene-kernel` | 83 | 19,010 | 280 | 13 |
| `petroleum` | 79 | 15,493 | 297 | 39 |
| `lattice` | 23 | 6,579 | 0 | 28 |
| `solvent` | 8 | 4,524 | 6 | 2 |
| `bonder` | 9 | 2,517 | 8 | 2 |
| `nozzle` | 8 | 1,958 | 0 | 4 |
| `carrier` | 4 | 285 | 0 | 0 |

`unsafe` 系の値は `unsafe` ブロック、`unsafe fn`、`asm!` の静的検索による目安であり、危険度そのものではない。ただし `nitrogen`、`petroleum`、カーネルの安全境界を優先監査すべきことは明確である。

### 特に大きいモジュール

| ファイル | 行数 | 混在している主な責務 |
|---|---:|---|
| `fullerene-kernel/src/drivers/fat.rs` | 1,893 | block device、partition、cache、FAT32、exFAT、VFS adapter |
| `solvent/src/lib.rs` | 1,570 | runtime、input、event、clock、render、terminal、window orchestration |
| `fullerene-kernel/src/syscall/handlers.rs` | 1,437 | process、FS、memory、event、thread、window、device、IPC、time |
| `nitrogen/src/usb/xhci/context.rs` | 1,427 | controller init、ring、command、event、transfer、resource cleanup |
| `nitrogen/src/iwlwifi.rs` | 1,423 | firmware、MMIO、TX/RX、802.11 state、DHCP/WPA orchestration |
| `solvent/src/explorer.rs` | 974 | state、layout、hit test、render、file actions |
| `nitrogen/src/storage/rtsx.rs` | 959 | controller、register access、media state、transfer |

## 優先順位

- **P0**: メモリ安全性、プロセス分離、データ破損につながるため先に対応する。
- **P1**: P0 の再発防止と、機能追加を安全にするための構造改善。
- **P2**: 開発速度、再現性、性能、ドキュメント品質の改善。

## 改善案一覧

| ID | 優先度 | 改善案 | 効果 | 規模 |
|---|---|---|---|---|
| P0-1 | P0 | fd／handle をプロセスごとに分離 | 権限分離、資源リーク防止 | 大 |
| P0-2 | P0 | user copy をページテーブル検証付きAPIへ統一 | 不正ポインタによるkernel fault防止 | 大 |
| P0-3 | P0 | `&'static mut` と可変globalの所有権を明示 | aliasing／data race防止 | 大 |
| P0-4 | P0 | block cache の境界・退避ロジック修正 | panic／誤配送／性能劣化防止 | 小 |
| P1-1 | P1 | 文字列エラーを型付きエラーへ統一 | 誤変換防止、保守性向上 | 中 |
| P1-2 | P1 | syscall ABI を独立クレート化 | kernel／SDKの仕様ずれ防止 | 中 |
| P1-3 | P1 | 巨大モジュールをContext境界で分割 | 変更影響と認知負荷の低減 | 大 |
| P1-4 | P1 | global callback／hookをContext所有へ移行 | 初期化順序とテスト性改善 | 大 |
| P1-5 | P1 | FS機能契約と未実装機能を明確化 | 呼び出し側の誤認防止 | 中 |
| P1-6 | P1 | timer／trace の並行性と時刻意味論を定義 | drift／race防止 | 中 |
| P1-7 | P1 | stub syscall の成功偽装をなくす | アプリ側の状態破損防止 | 中 |
| P1-8 | P1 | headless・fake device テストを拡充 | ハードウェアなしで回帰検知 | 大 |
| P2-1 | P2 | CIとtoolchainを再現可能にする | nightly更新事故の抑制 | 中 |
| P2-2 | P2 | workspace依存バージョンを統一 | 型重複、ビルド時間、容量削減 | 小〜中 |
| P2-3 | P2 | 固定4K back buffer等のメモリ使用を見直す | 常駐メモリ削減 | 中 |
| P2-4 | P2 | capability／対応状況を文書と機械判定で同期 | 実装と説明のずれ防止 | 小 |

## P0: 最優先

### P0-1. fd／kernel object handle をプロセスごとに分離する

対象:

- `fullerene-kernel/src/syscall/handlers.rs`
- `fullerene-kernel/src/process.rs`
- `fullerene-kernel/src/linux/runtime.rs`

現状:

- native syscall の `FD_TABLE` と `HANDLE_TABLE` は全プロセス共有である。
- ソースコメントにも、整数 handle を推測すれば別プロセスのobjectへアクセスできる問題が明記されている。
- fd、event、thread、window、device、channel、pipe、timer の寿命がprocess寿命と結び付いていない。
- `HandleTransfer` は対象processのnamespaceへ移さず、global tableへ戻している。
- pipe作成は2つの64bit handleを1つの`u64`へ詰めており、32bitを超えると切り捨てられる。

提案:

1. `ProcessResources` を導入し、`FdTable`、`HandleTable`、cwd、権限を `Process` が所有する。
2. handleを `index + generation` 形式にし、解放後の古いhandle再利用を検出する。
3. 各handleに権限bit（read、write、signal、duplicate、transfer等）を持たせる。
4. process終了時に全fd／handleをcloseし、waiter登録も解除する。
5. transferは送信元から削除して送信先へ挿入する処理を1つのtransactionとして行う。
6. 複数handleの返却はuser buffer上の `#[repr(C)]` 構造体を使う。

完了条件:

- 2 processが同じ数値のfdを独立して利用できる。
- 他processのhandleを指定すると `BadHandle` または `PermissionDenied` になる。
- process終了後にfd、timer、waiter、window handleが残らない。
- duplicate／transfer／revokeの権限テストがある。

### P0-2. user memory accessを1つの検証済みAPIへ統一する

対象:

- `petroleum/src/common/memory.rs`
- `fullerene-kernel/src/syscall/interface.rs`
- `fullerene-kernel/src/syscall/handlers.rs`
- `fullerene-kernel/src/linux/runtime.rs`
- `fullerene-kernel/src/linux/*`

現状:

- `validate_user_buffer` はアドレス範囲がuser領域かを確認するが、各ページが現在のprocessへmap済みか、書き込み可能かは確認しない。
- `user_slice`／`user_slice_mut` は検証後に `&'static [u8]`／`&'static mut [u8]` を返すため、実際のprocess address spaceより長い寿命を型で許している。
- Linux互換層の `copy_user_string`、`copy_from_user`、`copy_to_user` はnull確認後に直接volatile accessし、ソース内でも未検証であることが明記されている。
- `copy_from_user` は上限超過をerrorにせず、65,536 byteへ暗黙に切り詰める。

提案:

1. `UserAddressSpace`、`UserPtr<T>`、`UserSlice` をkernel側へ導入する。
2. 現在processのpage tableを使い、範囲内の全pageについて present／user／writable／NX を用途別に確認する。
3. syscall境界ではborrowを返さず `copy_from_user`／`copy_to_user` でkernel所有bufferへコピーする。
4. 大きな転送はpage単位または固定chunkで処理し、上限超過は `E2BIG` 等で明示的に失敗させる。
5. native ABIとLinux ABIのcopy処理を同じ実装へ集約する。

完了条件:

- null、範囲overflow、user/kernel境界跨ぎ、未map page、read-only page、page跨ぎをテストする。
- user memory由来の `&'static` 参照が公開APIに残らない。
- 不正ポインタでkernel panic／page faultが発生せず、適切なerrnoを返す。

### P0-3. `&'static mut` と可変globalの所有権を明示する

主な対象:

- `petroleum/src/page_table/constants.rs`
- `petroleum/src/page_table/*`
- `fullerene-kernel/src/gui.rs`
- `fullerene-kernel/src/contexts/framebuffer.rs`
- `solvent/src/lib.rs`
- `resonance/src/tracing.rs`
- `fullerene-kernel/src/gdt.rs`
- `fullerene-kernel/src/graphics/*`

現状:

- `SyncUnsafeCell` 内のframe allocatorを複数回 `&'static mut` として取得できる。
- framebuffer callbackがkernel lockの外へ `&'static mut [u32]` を返す。
- `solvent` は framebuffer address を整数で `LAST_FB` に保存し、後から別のmutable sliceを再構築する。
- 静的検索上、`static mut` の出現はカーネル28箇所、`petroleum`15箇所である（コメント内の言及を含む目安）。
- `resonance` trace bufferはatomicなindexと非atomicな `static mut` 配列を組み合わせ、local interrupt停止だけでsnapshotしている。SMPや別CPUからのrecordには対応できない。

提案:

1. frame allocatorは `FrameAllocatorContext` のguard経由で借用し、同時に1つのmutable borrowだけを許す。
2. framebufferは `with_framebuffer(|guard| ...)` のclosure APIにし、lock／mapping guardより参照が長生きしないようにする。
3. `solvent` のcursor fast pathも同じ `FramebufferGuard` を受け取り、生addressの保存をやめる。
4. boot-only globalは `Once` または初期化後immutableな構造へ変換する。
5. trace bufferはsingle-core限定を型／cfgで固定するか、sequence付きslotやlockでmulti-writer安全にする。
6. crate単位で `#![deny(unsafe_op_in_unsafe_fn)]` を段階導入し、各unsafe boundaryに `# Safety` と不変条件を書く。

完了条件:

- safe APIから複数の `&'static mut` を取得できない。
- framebuffer、frame allocator、traceに対する同時access方針が型またはlockで表現される。
- `static mut` の残存箇所にboot phase／CPU／interruptの前提が記述される。

### P0-4. block cacheの境界チェックと退避方式を修正する

対象: `fullerene-kernel/src/drivers/fat.rs`

現状:

- cache hit時は `buf` の長さを確認する前に `buf[..bps]` へcopyするため、短いbufferでpanicする。
- cache miss時はdevice read後にbuffer不足を検出するため、失敗した呼び出しでもcache状態が変わる。
- multi-sector read／writeは全体buffer長を確認せずsliceを作る。
- `evict_slot` は「round-robin」と書かれているが、満杯になると常にslot 0を返す。
- partition offsetとLBA加算にoverflow／device末尾の検査がない。

提案:

1. I/O前に `count * sector_size` を `checked_mul` し、buffer長とLBA範囲を検証する。
2. cache hit／missより前にbuffer不足を返す。
3. `next_victim` を持つ実際のround-robin、または小さなLRUへ変更する。
4. cache lineにvalid bitを持たせ、`0xFFFF_FFFF` をsentinelとして使わない。
5. fake block deviceでhit、miss、eviction、write invalidation、短いbuffer、LBA overflowをテストする。

完了条件:

- 不正bufferやLBAでpanicせず型付きerrorを返す。
- 全slotが順に退避対象になる。
- error時にcacheとdeviceの状態が変わらない。

## P1: 構造改善

### P1-1. 文字列エラーを型付きエラーへ統一する

対象: `genome`、`fullerene-kernel`、`nitrogen`、`petroleum`、`solvent`

現状:

- `Result<..., &'static str>` はカーネル99箇所、`nitrogen` 78箇所、`genome` 24箇所、`petroleum` 22箇所で使われている。
- VFS errorは文字列matchで `FsError` やLinux errnoへ再変換される。
- typoや新しいerror文字列が追加されてもcompilerが変換漏れを検出できない。

提案:

- `FsError`、`BlockError`、`DriverError`、`MemoryError` を各leaf crateで定義する。
- 上位層では `From` 実装により `SyscallError`／errnoへ変換する。
- hardware固有情報が必要な場合は小さなcontext値をvariantへ保持する。
- 表示文字列は `Display` に閉じ込め、制御フローに文字列を使わない。

### P1-2. syscall ABIを独立したleaf crateへ分離する

対象:

- `petroleum/src/common/syscall.rs`
- `fullerene-kernel/src/syscall/*`
- `toluene/src/sys.rs`
- `fullerene-kernel/src/linux/numbers.rs`

現状:

- Fullerene syscall番号は `petroleum`、dispatchはkernel、wrapperは `toluene` に分散している。
- 引数の意味や返却用構造体がコードコメント頼みである。
- Linux ABI定数は別途大きな手書き一覧を持つ。

提案:

- `fullerene-abi` crateを追加し、syscall番号、error番号、`#[repr(C)]` DTO、versionを定義する。
- kernelとSDKの双方がこのcrateだけに依存する。
- pointerを含むDTOにはsize／alignmentのcompile-time testを追加する。
- `AbiVersion`／capability query syscallを用意し、未対応機能をuserlandが判定できるようにする。

### P1-3. 巨大モジュールをContextとライフサイクルで分割する

推奨分割:

| 現在 | 分割候補 |
|---|---|
| `drivers/fat.rs` | `block_device`、`partition`、`cache`、`fat32`、`exfat`、`directory`、`file_handle`、VFS adapter |
| `syscall/handlers.rs` | `dispatch`、`process`、`fs`、`memory`、`event`、`thread`、`window`、`device`、`ipc`、`time` |
| `solvent/lib.rs` | `runtime_context`、`input_loop`、`event_loop`、`clock_service`、`render_loop`、`terminal_service` |
| `usb/xhci/context.rs` | `controller_init`、`command`、`event`、`control_transfer`、`bulk_transfer`、`resources` |
| `iwlwifi.rs` | `device`、`firmware`、`registers`、`tx`、`rx`、`connection_state` |

FAT実装はkernel policyから独立しているため、最終的には `genome-fat` のような独立crateにし、kernelはblock device capabilityを渡すだけにするのが望ましい。

### P1-4. callback／hook／runtime globalをContext所有へ移す

対象:

- `solvent::SOLVENT_CALLBACKS`、`RUNTIME`、`EVENT_QUEUE`、`DISPATCHER`
- `nozzle::FS_HOOKS`、`SYS_HOOKS`
- Wi-Fi、storage controller、block device registryのglobal
- `carrier::SHARED_HISTORY`

提案:

- `RuntimeContext` に `Services` interfaceをconstructor injectionする。
- terminal historyとpipe bufferはterminal session単位で所有する。
- hook未設定をruntime errorにせず、必要capabilityを型でconstructorへ要求する。
- device registryは `DeviceManagerContext` が所有し、take／returnの明示的なlease APIを提供する。

これにより、hidden initialization orderとテスト間のglobal stateリセットを減らせる。

### P1-5. filesystemの機能契約を明確にする

現状:

- FAT adapterの `mkdir` と `unlink` は未実装である。
- `genome` に `InodeType::Symlink` と解決処理はあるが、symlink作成／readlink APIがない。
- FAT／exFATのfile offsetとsizeは主に `u32` で、exFATの大容量file仕様と一致しない。
- `FileSystem` traitは `Option` と文字列errorが混在し、未実装、未存在、権限不足を区別しにくい。

提案:

- traitを型付き `Result` へ統一する。
- `FileSystemCapabilities` でread-only、mkdir、unlink、symlink、large-file等を公開する。
- 未実装操作は必ず `NotSupported` を返す。
- symlinkを完成させるか、公開enumから一時的に外して契約を実装に合わせる。
- offset／size／LBAを `u64` へ統一し、変換点でchecked conversionする。

### P1-6. timerとtraceの意味論を固定する

対象: `chronoline/src/lib.rs`、`resonance/src/tracing.rs`

現状:

- repeating timerは期限超過時に「以前のdeadline + interval」ではなく「現在時刻 + interval」へ再登録されるため、遅延のたびに位相がずれる。
- interval 0を登録すると、同じ時刻で永久にexpiredになり得る。
- timer queueはsorted `Vec` へinsertし、先頭を `remove(0)` するためO(n)である。
- trace bufferはsingle-core前提がAPIに表れていない。

提案:

- repeating timerに `FixedRate` と `FixedDelay` を明示する。
- interval 0を登録時errorにする。
- catch-up回数の上限とmissed tick policyを定義する。
- timer数が増える場合はmin-heapへ変更する。
- traceはsequence番号付きsnapshotを採用し、SMP非対応ならcompile-time cfgと文書で固定する。

### P1-7. stub syscallの成功偽装をなくす

対象: `fullerene-kernel/src/linux/*`、`fullerene-kernel/src/syscall/handlers.rs`

現状:

- Linux互換層では `mount`、`umount2`、`truncate`、`fsync`、一部uid／capability系等が何もせず成功を返す。
- native syscallにもdevice、event subscription、RTC等のstubがある。
- 呼び出し側は処理済みと判断し、後段で不整合が発生し得る。

提案:

- 副作用を実装していないsyscallは原則 `ENOSYS`／`NotSupported` を返す。
- compatibility目的で成功が必要な場合は、対象binaryと理由を表へ記録する。
- syscall support matrixをtest data化し、dispatch tableと文書を同じ定義から生成する。

### P1-8. hardwareなしで実行できるテスト境界を増やす

優先対象:

- `carrier`: pipeline parse、unknown command、stdin/stdout cleanup、command停止
- `solvent`: input event → state transition、dirty rect、clock、terminal session
- FAT／block cache: memory-backed fake block device
- syscall: fake process address spaceと2 processのresource isolation
- `nitrogen`: register backendをtrait化したstate-machine test
- `lattice`: deterministic scene snapshot／PPM hash

特に `carrier` は純粋ロジック中心だがテストが0件、`solvent` は4,524行に対して2件である。driver全体をmockするのではなく、MMIO/DMA境界の外側にあるstate machineだけをhost test可能にする。

## P2: 開発基盤と性能

### P2-1. toolchainとCIを再現可能にする

現状:

- `rust-toolchain.toml` とCIは日付なしの `nightly` を使うため、同じcommitでも日によって結果が変わる。
- CIはhost `cargo check` とUEFI buildのみで、format、Clippy、unit test、QEMU smokeを実行しない。
- toolchain componentsに `rustfmt` と `clippy` が含まれていない。

提案:

1. `nightly-YYYY-MM-DD` へ固定し、更新専用PRで上げる。
2. componentsへ `rustfmt` と `clippy` を追加する。
3. CIを `fmt`、host test、Clippy、UEFI build、headless QEMU smokeへ分ける。
4. QEMU smokeはtimeoutと `isa-debug-exit` を使い、boot stage到達をserial logで確認する。
5. driver／real hardware testは別jobまたは手動matrixに分離する。

### P2-2. workspace dependencyを統一する

現状:

- `spin` 0.10と0.12が混在する。
- `x86_64` 0.14と0.15が混在する。
- `volatile` 0.4と0.6が混在する。
- version／edition／authorsをworkspace継承するcrateと直接記述するcrateが混在する。

提案:

- rootへ `[workspace.dependencies]` を追加し、共通crateを `workspace = true` で参照する。
- 複数versionが必要な場合は理由と移行期限をコメントする。
- package metadata、license、repository、rust-versionを `[workspace.package]` へ集約する。
- `cargo tree -d` をCIで記録し、重複増加をreview可能にする。

### P2-3. 常駐メモリとhot path allocationを計測して減らす

候補:

- `solvent` の固定 `3840 * 2160` back bufferは約33MiBを常駐させる。実解像度に合わせた確保、tile buffer、dirty-region bufferを検討する。
- framebufferとback bufferの二重保持をboot時メモリ量に応じて切り替える。
- shellの二重 `format!`、clock文字列clone、render hot pathの一時 `Vec` を計測後に削減する。
- fd／handle／processの線形探索は、正しい所有権モデルへ移行した後にslot map等を検討する。
- boot時間、frame時間、heap high-water mark、DMA使用量を計測できるcounterを追加する。

最適化は測定値を添えて行い、unsafe化による短縮は避ける。

### P2-4. 実装済みcapabilityと文書を同期する

提案:

- FS、Linux syscall、native syscall、driver、QEMU／実機のsupport matrixを `docs/` に置く。
- `Supported`、`Partial`、`Stub`、`NotSupported` を区別する。
- matrixの元データをRustまたはTOMLで管理し、文書とテストを生成する。
- READMEのfeature一覧はsupport matrixへlinkし、stubを完成機能として表現しない。

## 推奨ロードマップ

### Phase 1: 安全性の短期改善

1. P0-4 block cacheの境界修正とfake device test
2. P0-1の第一段階として `ProcessResources` とper-process fd tableを導入
3. handleへowner PID検査を追加し、その後per-process tableへ移行
4. P0-2の `UserPtr`／copy-in/out基盤を作り、Linux層から置換

### Phase 2: 所有権とAPIの整理

1. P0-3 frame allocator／framebuffer guard
2. P1-1 typed error
3. P1-2 `fullerene-abi`
4. P1-5 FS capabilityとlarge-file対応方針

### Phase 3: 分割とテスト

1. syscall handlerを機能別moduleへ分割
2. FATをblock/cache/FAT32/exFATへ分割
3. `RuntimeContext` へSolvent globalを集約
4. xHCI／iwlwifiのstate machineをhost test可能にする
5. CIにhost testとQEMU smokeを追加

### Phase 4: 性能とSMP準備

1. trace／timerの並行性モデル確定
2. back bufferとrender hot pathの計測・最適化
3. remaining `static mut` のCPU／phase所有権を再監査

## Issueへ分割しやすい最初のタスク

1. `BlockCache::read_sector` の事前buffer検証とround-robin実装
2. `BlockCache` 用 `FakeBlockDevice` と境界テスト追加
3. Linux `copy_from_user` の暗黙truncateをerror化
4. `UserSlice` APIの設計文書とpage跨ぎtest作成
5. native `FD_TABLE` を `ProcessResources` へ移す
6. handle lookupにowner PID検査を追加
7. `carrier` pipeline／dispatchのunit test追加
8. `chronoline` のinterval 0拒否とfixed-rate／fixed-delay仕様決定
9. `rust-toolchain.toml` のnightly日付固定
10. CIへ `cargo fmt --check` とhost unit test job追加

## 判断基準

今後の改修では、次の順序を優先する。

```text
memory safety / process isolation
    → explicit ownership and lifecycle
    → typed contracts
    → deterministic tests
    → module size reduction
    → performance tuning
```

単にファイルを小さくする、globalを別ファイルへ移す、unsafeをwrapperで隠すだけでは改善とみなさない。所有者、同期範囲、失敗時のrollback、テスト可能な境界が明確になったことを完了条件とする。
