# ANE-LX2J (Huawei P20 Lite, Kirin 659) への Fullerene 移植

[English version](ANE_LX2J_PORT.md)

ブートローダー解除
([ANE_LX2J_BOOTLOADER_UNLOCK_20261002.ja.md](ANE_LX2J_BOOTLOADER_UNLOCK_20261002.ja.md))
の続き。以下の内容はすべて 2026-10-02 未明に実機または実機のデバイスツリーから
測定したもので、データシートからの推測は含みません。

## ブート契約（実測値）

`kernel` パーティションは裸の arm64 `Image` ではなく **Android boot image
（ヘッダ v0）** で、デバイスツリーは `dts` パーティションから `x0` で渡されます。

| フィールド | 値 | 出典 |
|---|---|---|
| マジック | `ANDROID!` | 実機 kernel パーティションのダンプ |
| `kernel_size` | gzip 圧縮ペイロード（純正: 11,681,787） | ダンプ |
| `kernel_addr` | `0x00480000` | ダンプ — LK が展開後の Image を置き、そこへジャンプする |
| `tags_addr` | `0x07e00000` | ダンプ |
| `page_size` | `2048` | ダンプ |
| `ramdisk_size` | `0` | ramdisk は別パーティション |
| cmdline | `loglevel=4 coherent_pool=512K ... buildvariant=user` | ダンプ |

中の arm64 `Image` はその先頭バイトから実行され、`code0` は 64 バイトの
Image ヘッダを飛ばして +0x40 へ分岐します。つまり Rust ペイロードのリンク
アドレスは `0x00480000 + 0x40 = 0x00480040` です。

推測ではなく証明済み: 純正カーネルを分解し、`mkane_bootimg.py`（v0 ヘッダ、
2048 バイトページ、gzip ペイロード）で再パックし、`fastboot flash kernel` で
焼いたところ **起動しました**。ヘッダの全フィールドは `kernel_size` を除いて
往復で保存されます（当然の差分）。

## 移植で追加したもの

| 要素 | 場所 |
|---|---|
| プラットフォーム名 | `flasks build --arch aarch64 --platform ane` → `FULLERENE_AARCH64_PLATFORM=ane` |
| リンクベース | `fullerene-kernel/build.rs` の `aarch64_linker_script` → `0x00480040` |
| エントリ定数 | `entry::LINK_ENTRY` → `0x0048_0040`（再配置デルタの基準） |
| 早期コンソール | `platform::ane::UART_BASE` = `0xfdf02000`（`/amba/uart@fdf02000`、pl011、status=ok） |
| DTB フォールバック | なし — LK が x0 で渡す。フォールバックはペリフェラル空間を指してしまう |
| 黒箱ウィンドウ | `blackbox.rs`: `pstore-mem` @ `0x34800000`、サイズ `0x100000` |
| 最初のマーク | `blackbox::mark_entry()` → コード `E`（最初のデバイスレジスタ書き込みより前） |
| 予約メモリのノード名 | `pstore-mem` で検索（Bramble のツリーでは `ramoops_region`） |

## 観測チャネル: 予約 RAM（他に読めるものが無いため）

この基板にはホストから到達できるシリアルが無く、無人実行では画面も見えません。
あるのはデバイスツリーの完全な `ramoops` ノードです。

```text
/reserved-memory/pstore-mem   reg = <0x0 0x34800000 0x0 0x100000>
/ramoops                      compatible = "ramoops"
    record-size  = 0x20000      console-size = 0x80000
    ftrace-size  = 0            pmsg-size    = 0x20000
    dump-oops    = 1            ecc-size     = 0
```

Linux の `fs/pstore/ram.c` はこの順にゾーンを配置する
（`dump_mem_sz = size - console_size - ftrace_size - pmsg_size`）ので:

```text
0x00000  dmesg レコード 3 × 0x20000
0x60000  コンソールゾーン 0x80000
0xE0000  pmsg ゾーン 0x20000   ← 永続テキストログ
0x100000 終端
```

永続ログは Bramble 版モジュールそのもの: pstore レコード 1 件 = 1 バイトで、
`persistent_ram_buffer` ヘッダ（`sig = "DBGC"`、`start = 0`、`size = n`）の
後ろに追記します。これで純正カーネルの ramoops がゾーンを再初期化せず保持し、
次の起動（root を取った Android、または Fullerene 自身）が読み出せます。

定数は Bramble のものではなくプラットフォームの値です:
`RAMOOPS_REGION_BASE`、`CONSOLE_ZONE_OFFSET`、`PMSG_ZONE_OFFSET`、および
2 つのゾーンサイズは `fullerene_aarch64_ane` で cfg 分岐しています。

## 起動ラダー

| 段 | 問い | 答え方 |
|---|---|---|
| L0 | ローダは我々のイメージへ入ったか | pmsg ゾーンの `E` レコードの有無 |
| L1 | 最初のレジスタ書き込みを越えて生き延びたか | `E` の後のレコード（`b` = DT 取得、`r` = 領域確認） |
| L2 | DT 解析とランタイム開始まで進んだか | 以降のステージコード |
| L3 | デバイス側が何か立ち上がったか | USB 列挙の変化、コンソールミラーの内容 |

`mark_entry()` は UART 書き込み（この起動で最初のレジスタアクセス）より
**前** に置いてあります。L0 はその後に全部死んでも答えられるようにするためです。

## ホスト側プリフライト（実機不要）

`ane_qemu_preflight.sh` は実 Image を QEMU の汎用 ARM virt マシンで走らせます。
QEMU がこの基板を模擬していないことが、この検査の要点です: カーネルが
`0xfdf02000` で初期化する PL011 はそこに存在しないため、最初のレジスタ書き込みが
*拒否* され、QEMU がそのアドレスを記録します。そこへ到達するためには、エントリ
stub、static-PIE 再配置、BSS ゼロ化、Rust エントリ、`mark_entry` のすべてが
完走している必要があります。マーク自身のアクセスは QEMU の未割当低メモリに
落ちて黙って捨てられるので、最初に *拒否* されるアクセスは UART のものになります。

ビルド済みイメージでの実測:

```text
最初の拒否アクセス: 0xFDF02030      (PL011 の CR、UART_BASE + 0x30)
Data Abort  FAR 0xfdf02030  ELR 0x400d1724    (イメージは 0x40080000 に配置)
```

`0x400d1724` は `aarch64_rust_entry` 内でインライン展開された `mark_entry` の
直後にある `str wzr, [x8]` で、そのマークの逆アセンブルにはこのデバイス自身の
定数（`mov w9, #0x34800000`、`#0xe0000` ゾーンオフセット、`0x43474244` "DBGC"
シグネチャ、`0x45` = 'E'）が現れます。つまり、コンパイルされたイメージが移植の
早期経路を実行することの機械的な確認であり、qemu-system-aarch64 のあるホストなら
どこでも走ります。

## 最初のテスト: 30 命令のプローブ

`ane_mark_probe.sh` は、ただ 1 つのことだけをするペイロードを組み立てます:
pmsg ゾーンに `persistent_ram_buffer` レコード（`"\n001:E"`、サイズ 6）を書いて
停止する。MMU もスタックも再配置も BSS も C も使わない。これは「ローダが我々の
イメージを受理して先頭命令に入った」ことを、その後の Fullerene の挙動すべてから
切り離して確かめるためで、初回の実機実行を誤読できないようにします。

正しさは仮定せずホストで検査します。同じソースを 2 回アセンブルし、片方はデバイスの
pmsg ゾーン向け（`MARK_BASE=0x348e0000`）、もう片方はエミュレート RAM 向け
（`MARK_BASE=0x400e0000`）にして `qemu-system-aarch64` で走らせ、ゲストのメモリを
QMP 経由で読み戻して比較します:

```text
00000000400e0000: 0x44 0x42 0x47 0x43 0x00 0x00 0x00 0x00   "DBGC"、start 0
00000000400e0008: 0x06 0x00 0x00 0x00 0x0a 0x30 0x30 0x31   サイズ 6、"\\n001"
expected == got  -> PASS
```

（QEMU のインターフェースで実測した 2 点: HMP の `pmemsave` は絶対パスを拒否し、
`size` にサフィックスは付きません。そのため検査は QMP の
`human-monitor-command` 経由で `xp /64bx` を使います。）

成果物: `artifacts/fullerene-ane-mark.img`（gzip）と `-raw.img`。電源再投入後に
期待されるレコードは `\n001:E`、サイズ 6、`0x348e0000`。

## コマンド

```bash
# ソースから検証済みの書き込み可能イメージまで、コマンド 1 本
#   flasks ビルド（platform ane）→ パッケージ → 機械検証
bash ~/dev/p20-root/ane_build.sh

# 同じ手順を個別に
cargo run -q -p flasks -- build --arch aarch64 --platform ane   # .Image を生成
bash ~/dev/p20-root/ane_pack.sh <the .Image>                    # v0 + gzip、生と lz4 の対照も
python3 ~/dev/p20-root/validate_ane_image.py <the ELF> artifacts/fullerene-ane.img <the .Image>

# ホスト側プリフライト: QEMU の ARM virt マシンで走らせ、実行が最初に触る
# デバイスアドレスを確認する（下の節を参照）
bash ~/dev/p20-root/ane_qemu_preflight.sh <the .Image>

# 30 命令の L0 プローブ（QEMU で自己検証済み。実機ではこれを最初に試す）
bash ~/dev/p20-root/ane_mark_probe.sh

# 実機 1 回（既定は fastboot boot = 純正カーネルは焼かれたまま）
bash ~/dev/p20-root/ane_experiment.sh artifacts/fullerene-ane.img first-gzip --wait 150

# その起動が残したものを読み出し（4.4.23 exploit の root シェル経由）
bash ~/dev/p20-root/ane_read_pstore.sh
```

## 実機セッションの前提（一度だけ手が必要）

前回の root シェル作業の副作用で、端末は Android 起動途中で固まったままです
（`12d1:107e`、MTP + Mass Storage のみ、adb なし）。復帰手順: 電源ボタン
長押し 10 秒 → Android 起動 → USB デバッグを再度有効化（「ファイル転送」モード
＋端末側の許可ダイアログ）。以降のスクリプトはボタン操作なしで無人実行できます。

## 次にやること

1. 実機初回: `fastboot boot artifacts/fullerene-ane-mark.img`（30 命令プローブ）
   → 電源再投入 → pstore 窓を読む。`\n001:E` が出ればローディング契約は端から端まで
   証明され、その後で本命の `fullerene-ane.img` に進みます。差分の対照として
   純正起動パターンを先に取得（`capture_boot_pattern.sh`）。
2. ペイロードが拒否された場合: `fullerene-ane-raw.img`（非 gzip）を試す —
   ローダの展開契約だけは、この流れで唯一未測定の点です。
3. `E` だけが出た場合: L1 を切り分ける（UART 書き込みを後ろへ動かす、または
   別の PL011 を選ぶ）。
4. その後ランタイム: ANE の USB は `hisi-usb2phy` @0xfe000000 +
   `usb_otg_ahbif@ff200000`（DWC3 ではないため Bramble の USB ドライバは
   そのままでは使えません）、パネルは `boe_otm1911a_5p84_1080p_video`、
   バックライトは `panel_blpwm` @0xffd75000。
5. ペイロード形式が実測できたら、パッケージングをリポジトリ内へ移します:
   flasks の `patch_bramble_boot_image` の v0 版を作り、
   `flasks build --platform ane --boot-template <ストック kernel ダンプ> --boot-output <img>`
   で書き込み可能ファイルが直接出るようにします。それまで保留するのは意図的で、
   圧縮契約を推測のままリポジトリに固定すると、間違った既定値がそのまま残るためです。
