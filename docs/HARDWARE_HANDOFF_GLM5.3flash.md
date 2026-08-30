# GLM5.3flash 引き継ぎプロンプト

以下を次のエージェントへの引き継ぎ情報として使用する。

## ミッション

`/home/placeless/dev/fullerene` で、Pixel 4a 5G / Bramble / SM7250
（serial `26191JECB00076`）に対して、非破壊の `fastboot boot` だけで
Fullerene の USB2 HS gadget `1234:0001` を列挙させる。成功条件は host の
`kernel.log` に次の行が現れること。

```text
New USB device found, idVendor=1234
```

ユーザーは「成功まで続ける」としていたが、2026-08-30 に GLM5.3flash へ
引き継ぐため一旦停止した。成功条件はまだ未達である。

禁止事項:

- `fastboot flash` / `erase` / partition write
- `git reset --hard` / `git checkout --`
- 明示指示のない commit
- 端末の手動操作

Harness が `adb reboot bootloader` を自動実行する。各 run は通常約 38 秒で
Android に戻る。現在の最後の run は途中で Ctrl-C したため、再開時は端末の
状態を read-only に確認し、通常の harness に戻すこと。

## 現在の症状

標準 direct handoff では host が T+10〜11 に USB2 HS attach を認識するが、
最初の descriptor read が失敗する。

```text
usb 1-9: new high-speed USB device number N using xhci_hcd
usb 1-9: device descriptor read/64, error -110
usb 2-1: New USB device found, idVendor=18d1, idProduct=4ee7
```

端末は通常 `bootreason=watchdog` で Android に復帰する。SDIS blip、QSCRATCH
pull-up drop、PSCI/PS_HOLD、APSS WDT readout、UART、GDBGLTSSM の host-visible
readout は既に死んだチャネルなので再利用しない。信頼できる観測は host journal
の attach / descriptor error / Android return の時刻、`boot-reason.txt`、および
成功時の USB identity だけである。

## 標準 run

```bash
env -u FULLERENE_AARCH64_USB_SIGNAL_DMA_POST_RUNSTOP \
cargo run -q -p flasks --bin bramble-usb -- loop \
  --direct-handoff --no-smmu --start-after-connect \
  --signal-probe --u0-arm-probe --smmu-disable --skip-typec-spmi \
  --observe-secs 1 --enum-timeout 20 --hold 1 --fastboot-wait 30
```

失敗時も `kernel.log`、`kernel-final.log`、`boot-reason.txt` を run directory
から抽出する。成功時は exact な `1234` 行を確認して直ちに報告する。

## 完了済みの実験

以下はすべて非破壊 `fastboot boot` で、`1234:0001` には到達していない。

- `159403`, `162325`, `164782`, `166911`, `168899`, `171036`: `u0-status*`
  gate 各値。gate の真偽は -110 デバイスでは host-visible に判別できず、全て
  同じ境界。
- `173229`: `armstat` readout。変化なし。
- `176046`, `184402`: `always` gate、観測 14 秒 / 10 秒。disconnect readout
  は得られず、host 結果は変化なし。
- `180978`: 一時的な `u0stat` QSCRATCH pull-up cycle。追加 host event なし。
- `192480`: direct path の BCR-before-reset (`FULLERENE_AARCH64_USB_HSPHY_BEFORE_RESET=1`)。
  HS attach → `-110` → Android `18d1:4ee7`。
- `196912`: 16-bit UTMI A/B。DWC3 の実際の `PHYIF` bit 3 と
  `USBTRDTIM=5` を設定。`-110` から `-71` に変化し、HS attach を複数回再試行
  して port power-cycle したが、`1234` にはならなかった。これは failure
  boundary が動いた重要な結果。
- `199296`: BCR-before-reset + `FULLERENE_AARCH64_USB_DWC31_DCTL_ONLY_RESET=1`。
  DWC_usb31 の `DCTL.CSFTRST` だけを残し、`GCTL.CORESOFTRESET` と
  `GUSB2/3PIPECTL.PHYSOFTRST` を省略。結果は HS attach → `-110` → Android。

### 未完了の最後の run

`204105` は `FULLERENE_AARCH64_USB_ENBLSLPM=1` の A/B。fastboot boot は受理され、
host log には次まで記録された。

```text
usb 1-9: new high-speed USB device number 26 using xhci_hcd
usb 1-9: device descriptor read/64, error -110
```

Android 復帰待ちの途中でユーザー指示により Ctrl-C したため、`boot-reason.txt`
は未取得で、run は未完了扱いとする。run directory は
`tmp/fullerene-bramble-loop.204105.0`。

## 現在の未 commit 変更

ユーザーの既存変更を保持すること。現在の `git status` は次の 4 ファイルが
modified。

- `docs/HARDWARE.md`: 直近の gate / BCR / PHYIF / DCTL-only 実験結果を追記済み。
- `fullerene-kernel/build.rs`: `FULLERENE_AARCH64_USB_PHYIF_16BIT` と
  `FULLERENE_AARCH64_USB_ENBLSLPM` の build-time cfg 転送を追加。
- `fullerene-kernel/src/arch/aarch64/usb/config.rs`: 16-bit UTMI では
  `PHYIF=1` と `USBTRDTIM=5`、ENBLSLPM A/B では bit 8 を設定。
- `fullerene-kernel/src/arch/aarch64/usb/mmio.rs`: UTMI 16-bit timing (`5 << 10`)
  の定数を追加。

コード変更後に以下は実行済みで、exit 0。既存の多数の warning はあるが、今回の
変更による compile error はない。

```bash
cargo fmt --all
cargo check -q -p flasks --bin bramble-usb
env FULLERENE_AARCH64_PLATFORM=bramble \
  FULLERENE_AARCH64_USB_GADGET_HANDOFF_PROBE=1 \
  FULLERENE_AARCH64_USB_GADGET_HANDOFF_DIRECT=1 \
  FULLERENE_AARCH64_USB_GADGET_HANDOFF_NO_SMMU=1 \
  FULLERENE_AARCH64_USB_GADGET_HANDOFF_START_AFTER_CONNECT=1 \
  FULLERENE_AARCH64_USB_EP0_SIGNAL_PROBE=1 \
  FULLERENE_AARCH64_USB_SMMU_DISABLE=1 \
  FULLERENE_AARCH64_USB_U0_ARM_PROBE=1 \
  FULLERENE_AARCH64_USB_SKIP_TYPEC_SPMI=1 \
  FULLERENE_AARCH64_USB_PROBE_SINGLE_ATTEMPT=1 \
  FULLERENE_AARCH64_USB_ENBLSLPM=1 \
  cargo check -q -p fullerene-kernel --features aarch64 \
    --bin fullerene-kernel-aarch64-usb-probe --target aarch64-unknown-none
```

## レジスタ上の注意

objective file に「PHYIF bit 8」とあるが、これはこの DWC3 定義では誤り。
`GUSB2PHYCFG` の `PHYIF` は bit 3、bit 8 は `ENBLSLPM` である。公式 Linux
DWC3 定義も同じで、16-bit UTMI は `PHYIF=1` / `USBTRDTIM=5`。従って、bit 8
を試す場合は PHYIF と呼ばず ENBLSLPM A/B として記録すること。

## 再開時の候補

1. `204105` の ENBLSLPM A/B を通常の 38 秒完走 run として再実行し、結果を
   `docs/HARDWARE.md` に追記する。
2. `196912` の `-71` 改善を切り分けるため、16-bit `PHYIF=1` と timing の
   組み合わせを必要なら A/B する。ただし `-71` は成功ではない。
3. objective に残っている候補を実行する前に、`docs/HARDWARE.md` の第五・第六
   session を確認すること。event-DMA の pre/post-Run/Stop と SWDD alternate/
   skip は既に記録済みで、stale objective の「未実施」と矛盾する。
4. 新しい診断を追加する場合も、最終判定は host の exact `1234` identity とし、
   死んだ disconnect/readout チャネルを成功証拠にしない。

この引き継ぎ時点で goal はユーザー指示により一旦停止。成功扱いにも blocked 扱い
にもしていない。
