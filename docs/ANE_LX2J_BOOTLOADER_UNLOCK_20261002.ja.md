# ANE-LX2J ブートローダー解除 — 2026-10-02

[English version](ANE_LX2J_BOOTLOADER_UNLOCK_20261002.md)

AArch64 ポートは現在 Pixel 4a 5G (Bramble) を `fastboot boot` で対象にしている。
次の対象は Huawei P20 Lite (ANE-LX2J, Kirin 659 / hi6250)。この端末は Bramble の
ような一時起動の経路を持たないため、Fullerene は `kernel` パーティションを経由
する必要があり、そのためにはブートローダーの解除が前提になる。本書は、ソフト
ウェアのみで解除に到達するまでの経緯と、解除後に開いたブート契約を記録する。

## 対象

| 項目 | 値 |
| --- | --- |
| 端末 | Huawei P20 Lite / ANE-LX2J / Kirin 659 (hi6250) |
| シリアル | `SCV7N18927000473` |
| 解除時のビルド | `ANE-LX2J 8.0.0.151(C635)`、Android 8.0.0 |
| カーネル | `4.4.23+ #1 SMP PREEMPT Fri Mar 1 00:11:18 CST 2019` |
| 手段 | ソフトウェアのみ。分解なし、テストポイントなし、有償解除サービスなし |
| 費用 | ファームウェア 4 本（マーケットプレイスのクレジットで約 240 円） |

## なぜ解除が壁だったか

ブートローダーは `fastboot oem unlock` に対して 3 つのゲートを順に要求する。

1. `nvme` パーティションの `FBLOCK` が 0 であること。
2. Android の開発者向けオプションで「OEM ロック解除」が有効であること。
3. 入力したコードのハッシュが `USRKEY`（同じく `nvme` 内）の値と一致すること。

いずれもパーティションの中身であり、書き換えには root が要る。そしてこの端末
では、root を得てもブロックデバイスに触れないという壁があった。

### SELinux の壁

CVE-2019-2215 で `uid=0` と全ケイパビリティは得られるが、ドメインは
`u:r:shell:s0` のまま。`/dev/block/bootdevice/by-name/*`、`/dev/nve0`、`nvme`
パーティションのいずれも open に失敗する。`setenforce`、`/sys/fs/selinux/enforce`、
`/proc/kallsyms`（root でも全てゼロ）、`dmesg`、`/proc/kcore` も閉じている。

エクスプロイトが持つ迂回は、自プロセスの SID に属する AV キャッシュのノードを
「許可」に書き換えるもの。それには 1 つのアドレスが要る。

```c
kaslr_offset   = kernel_read_ulong(task_struct + SCHED_CLASS_OFFSET) - FAIR_SCHED_CLASS;
avc_cache_addr = kaslr_offset + AVC_CACHE;
```

ここを読み解くと、**必要なのは `AVC_CACHE - FAIR_SCHED_CLASS` の差だけ**だと分かる。
KASLR スライドはコンパイル時に埋め込んだ `FAIR_SCHED_CLASS` から導かれ、同じ定数が
`AVC_CACHE` 側で相殺される。絶対アドレスも 2 MiB 整列の検証も不要で、探索不能な
問題が「測定できる 1 つの数」に変わる。

### その数の測定

マーケットプレイスからファームウェアを 4 本入手して展開し、`KERNEL` ブロックを
`vmlinux-to-elf` でシンボル付き ELF に復元して比較した。

| パッケージ | カーネルビルド日 | `avc_cache - fair_sched_class` |
| --- | --- | --- |
| 8.0.0.110(C635) | 2018-04-18 | `0x132e838` |
| 8.0.0.202(C719) | 2018-05-16 | `0x134a838` |
| 8.0.0.151(C01) all cn | 2018-06-11 | `0x134a838` |
| 8.0.0.127(C719) / kddi_jp | 2019-04-25 | **`0x134c838`** |
| エクスプロイト内蔵 | 不明 | `0x135c838`（一度も成功せず） |

端末が動かしているカーネルは 2019-03-01 ビルドで、上の表の下 2 行の間にある。
この期間でレイアウトはほとんど動かないため、まず `0x134c838` を実機で試したと
ころ成功した。SELinux はもう止められず、`nvme` パーティションへの `dd` が通った。

なお、この値はビルド日付だけで決まるものではない。リンク後のイメージの
`.text` / `.bss` 配置の性質であり、今回の 4 本では 1 か月違いのカーネルが同じ差を
持つ場合もあった。

### 競合の規則（実測）

| エクスプロイトへの変更 | 結果 |
| --- | --- |
| なし | 勝つ |
| 定数の即値のみ | 勝つ（掃引に使用） |
| 呼び出し先の差し替え、argv 対応、printf 追加 | 全敗 |

リンカ配置を動かし得ない変更だけが生き残る。競合はシステム負荷にも依存し、静か
な端末では安定して勝ち、起動直後や adb を連打している間は負ける。

## 解除そのもの

1. `hisi-nve w FBLOCK 0` — `nvme` 内の 7 コピーすべてを 0 にし、読み戻しで確認。
2. Android の「OEM ロック解除」トグル。当初はグレーアウトしていた。海外の事例
   どおり、**インターネットに接続してしばらく待つ**と押せるようになった。この ROM
   には `persistent_data_block` サービスも oemlock HAL も無く、対応する NV 状態を
   書けるのはこのトグルだけである。
3. `USRKEY` — **`hisi-nve` の書き込みは、この端末では何も起きない。**
   `Hashing USRKEY...` と表示したうえで旧値が読み戻る。有効な方法は、`nvme` を
   ダンプして値フィールドに `SHA256(<コード>)` を直接置き、イメージを `dd` で書き
   戻すこと。

   ```
   エントリ配置 : 名前(8 バイト) + 12  ->  値 (104 バイト)
   USRKEY コピー: 7 個、0x29d84 + k*0x20000
   ```

   読み戻しで 7 コピーすべて一致を確認した。

4. `fastboot oem unlock 0123456789ABCDEF` →
   `(bootloader) The device will reboot and do factory reset...` で解除。

### 確認

| プロパティ | 解除前 | 解除後 |
| --- | --- | --- |
| `ro.boot.flash.locked` | `1` | `0` |
| `ro.boot.verifiedbootstate` | `GREEN` | `ORANGE` |

どちらも起動時にブートローダーがカーネルへ渡す値で、`0` / `ORANGE` が解除状態。

`FBLOCK=0` 単独ではブートローダーの状態は動かない。これは「低速経路の安全検査」
— 解除の話に入る前に `check password failed` を出していたもの — を取り除き、上の
2 ゲートへ進めるようにする。必要だが十分ではない。

## AArch64 ポートのためのブート契約

この端末の `kernel` パーティションは、裸の arm64 `Image` ではない。
Android boot image である。

| フィールド | 値 |
| --- | --- |
| マジック | `ANDROID!` |
| `kernel_size` | `0x00b23ffb` (11,681,787) — gzip 圧縮されたペイロード |
| `kernel_addr` | `0x00480000` — LK がカーネルをここにロードする |
| `tags_addr` | `0x07e00000` |
| `page_size` | `2048` |
| `ramdisk_size` | `0` — Huawei は ramdisk を別パーティションに分けている |
| cmdline | `loglevel=4 coherent_pool=512K page_tracker=on slub_min_objects=12 unmovable_isolate1=2:192M,3:224M,4:256M printktimer=0xfff0a000,0x534,0x538 androidboot.selinux=enforcing buildvariant=user` |

デバイスツリーもカーネルには付加されていない。LK が `dts` パーティション
(`mmcblk0p34`、29,360,128 バイト) から供給する。

Fullerene の aarch64 イメージ生成はすでに Linux arm64 `Image` ヘッダ
(`ARM\x64` マジック、`text_offset 0x80000`) を出力している。この端末で起動するには
加えて次を満たす必要がある。

- LK が使うロードアドレスに合わせてリンクすること（ヘッダ上は `0x00480000`。
  実際の配置は実機で確認するまでは確定としない）
- 同じページサイズと、LK の cmdline と衝突しない cmdline を持つ `ANDROID!`
  ヘッダで包むこと
- デバイスツリーは付加ブロブではなく `x0` で渡される前提にすること

エントリ時の条件自体は Linux プロトコルと一致しており、Bramble ポートが既に
従っているものと同じである。

## 成果物

バックアップ、成功したエクスプロイト、確認記録:

```
~/dev/p20-root/backups/ANE-LX2J_20261002/
    nvme_BEFORE_FBLOCK_write.bin   工場状態（何も変更する前）
    nvme_after_FBLOCK0.bin         FBLOCK を消した状態
    nvme_usrkey_installed.bin      実際に書き込んだイメージ（FBLOCK=0 + 自前キー）
    nvme_CURRENT_unlocked.bin      解除後に端末から取得
    frp_original.bin               無傷
    misc.bin  oeminfo.bin
    cve-2019-2215_working_0x134c838
    kernel_8.0.0.110.elf
    unlock_verification.txt
    MD5SUMS.txt                    ディレクトリ内で `md5sum -c` できる
```

復元:

```bash
dd if=nvme_usrkey_installed.bin of=/dev/block/bootdevice/by-name/nvme bs=4096
sync
```

一次資料: 端末自身の `/proc/version` が示すカーネルツリー、CVE-2019-2215 の
PoC、`hisi-nve`、上表のファームウェア 4 本。

## 次にやること

1. LK のカーネルロードアドレスとエントリ時のレジスタ状態を実機で確認する。
2. Fullerene の aarch64 イメージを、この端末向けの `ANDROID!` boot image に
   詰め替える。
3. シリアルの無い環境での bring-up: まずフレームバッファ、次に USB コントローラ
   を立ち上げ、Bramble で使っているパニック経路を再利用できるようにする。
