# ANE-LX2J FullereneOS 移植ロードマップ (2026-10-06 昼)

## 到達点 (実機検証済み)

- fastboot 一式 (download/flash/erase) — カーネル自身が eMMC を書換え・消去。実クライアント rc=0。
- genome ストレージスタック全開通: eMMC → `EmmcBlockDevice` → `PartView` →
  `gpt::scan` (56) → ext4 マウント → readdir (system ルート 25 件) →
  `/build.prop` 全文 (1,906 B) + FNV。v96-v99 全 green。
- 400kHz 運用 (48KB/s)。HS 化は PHY 域で保留 (v93/v94 の教訓: コマンド完了でもデータ来ず)。

## 進行中

- v100/v102 の userdata 20GB F2FS マウントは **クラッシュループ** (8MB heap でも)。
  → v103 で **オンデマンド化** (`oem udmount`、クラッシュ自己限定)。udmount の結果は
  リングラップ+キュー汚染で回収できず → **クロック高速化 (PHY) 後まで保留**。
- **v104 安定版 = 現在の基準正** (`oem log raw` 追加、全域リング読取可)。
- **v105c = フル bin + aarch64-user-launchd が ANE で初回起動成功** (kernel up +
  PROBE_DONE)。ただしその後 eRecovery 着地 (原因未確定、復帰手順の途中経過と錯綜)。
  v106 (計装版: "launchd: calling run" + staging-length) = ビルド済・未投入。
- **離席中の安全網**: 常設キャッチャー (8h) + USB 変化監視 (8h) 稼働。combo で復帰後は
  v104 が自動焼き戻しされる。
- 恒久ログは 57KB ラップ式リング (`oem log raw` で全域到達可)。

## 大物アーク (順次)

- A. **ANE → OS ハンドオフ**: v106 計装で launchd の到達点を確定 → 段階統合
  (initramfs/fs::init の ANE 適合、eMMC マウント配線 (`#[cfg(bramble)]` の緩和))。
- B. **flasks の full-bin 配備切替** (手動レシピ確立済: PIC/PIE + rust-lld + 専用 target dir)。
- C. **userdata f2fs**: PHY 高速化後に再開 (ホスト側解析: `oem dump` 構想)。
- D. **速度 (PHY)**: 保留。arasan set_uhs_signaling/PHY_CTRL 移植 + ADMA2 検討。
- E. **SD (dwmmc1)**: 情報入り次第。

## ループ規律 (守るもの)

- サイクル直列化 (ログリーダー保持中に次サイクル禁止)。client-first、pyusb を client 前に置かない。
- 起動中の oem log 読取は 110 秒待つ。救済 = fb_recover_ep1.py (SET_CONFIGURATION 0→1)。
- **危険テストの前は必ずキャッチャー武装** (fb_safety_catcher.sh <last-good>)。flash 後は
  auto-reboot 必須 (LK 居座り対策)。
- **手動 aarch64 ビルド = flasks レシピ必須** (PIC/PIE + rust-lld + 専用 target dir)。
- **HEAP 注意** (8MB)。GB 級 fs の mount は枠内で。
- 検証 = 実ビルド exit + 実機ログ。probe は --usb-gadget-handoff-probe 必須。

