# WASM画像ビューア ハング調査記録

調査日: 2026-07-25

## 結論

JPEGデコーダや `show_image` host callback がハングの主因ではなかった。

`225x225` の小さいJPEGに対して、ビューアが次の処理を行っていたことが原因だった。

```text
DynamicImage::thumbnail(800, 600)
225x225 -> 600x600
```

`image 0.25.10` の `DynamicImage::thumbnail()` は、小さい画像を指定範囲内に収めるだけでなく、指定サイズまで拡大する挙動になっていた。その結果、225x225画像が `600x600 / 1,080,000 bytes` のRGBバッファへ膨張し、WASMインタプリタ上の画像処理でハングしていた。

小さい画像は拡大せず元画像を使うよう修正した。修正後は同じ画像が次のサイズで `show_image` へ渡る。

```text
225x225 / 151,875 bytes
```

## 再現条件

代表サンプル:

```text
/home/placeless/images.jpeg
JPEG/JFIF, 225x225, 3 components, 12,054 bytes
SHA-256: 2a1ff09ccd569bd1a1abb1fbccdf51cea786eb4cba4524212cfcbd8421fde600
```

シェルから、VFSへマウントしたSD上の画像を次のように開く。

```text
wasm /apps/viewer.wasm /mnt/sda/images.jpeg
```

ファイルマネージャーから開く場合も、同じ `/apps/viewer.wasm` とWASI runtimeを通る。

## 調査時の反証

- 画像は225x225であり、8MP/40MP制限には該当しない。
- 8MP制限へ下げる実験ではハングが解消しなかった。
- 同じWASMと画像をWASI host harnessへ渡すと、修正前でも次まで完走した。

```text
RUN
SHOW_IMAGE 600x600 1080000
DONE 0
```

このため、WASM起動、JPEG decode単体、`show_image` host callback単体だけでは再現しなかった。

## 最終的な停止位置

実機のKlog Liveで次の状態を確認した。

```text
viewer: decode exit
```

その後に存在する処理は次の通りだった。

```rust
let thumbnail = img.thumbnail(MAX_IMAGE_WIDTH, MAX_IMAGE_HEIGHT);
let pixels = thumbnail.to_rgb8().into_raw();
show_image(...);
```

したがって、停止位置は `show_image` より前、`thumbnail()` または `to_rgb8()` の間に限定された。

## 追加した診断ログ

以下の境界をKlogへ記録する。

- WASM binaryの読み出し前後
- 対象ファイルのVFS読み出し前後
- WASM stdout/stderr
- image dimensions
- JPEG decode前後
- thumbnail前後
- `to_rgb8` 前後
- `show_image` 入口・出口
- window作成前後
- surface取得・RGB blit前後
- invalidate前後
- VFS copyのread/write回数、バイト数、FNV指紋、先頭末尾バイト

主な実装箇所:

- `toluene/viewer/src/main.rs`
- `fullerene-kernel/src/shell.rs`
- `fullerene-kernel/src/fs.rs`
- `fullerene-kernel/src/contexts/vfs.rs`
- `solvent/src/window_api.rs`

## Klog Liveに関する注意

WASMは現在、シェルから同期実行される。通常のイベントループはWASM実行中に進まないため、Klogを書き込むだけではKlog Live画面が更新されない。

そのため診断中は、WASM診断イベントごとに次を行う。

1. Klogへ書き込む
2. Klog Liveをdirty化する
3. カーネル側からGUIを明示的に再描画する

この処理は診断用であり、WASMの別タスク実行モデルへ戻す変更ではない。

## ビルド成果物の注意

`fullerene-kernel/build.rs` がビルド時に `toluene/viewer` をビルドし、`OUT_DIR/viewer.wasm` としてUEFI kernelへ埋め込む。

確認には次を使う。

```bash
cargo build -p fullerene-kernel --target x86_64-unknown-uefi
```

実機へ投入するkernelは次のビルド成果物を使う。

```text
target/x86_64-unknown-uefi/debug/fullerene-kernel.efi
```

`toluene/viewer/target/.../viewer.wasm` には過去世代が残る場合があるため、単独で実機のWASM世代判定に使わない。UEFIビルド時の `target/x86_64-unknown-uefi/debug/build/fullerene-kernel-*/out/viewer.wasm` と比較する。

## 検証結果

修正後のWASI host harness:

```text
viewer: read complete path=/sample.jpeg bytes=12054
viewer: dimensions complete 225x225
viewer: decode enter
viewer: decode exit
viewer: thumbnail enter source=225x225 limit=800x600
viewer: thumbnail exit 225x225
viewer: to_rgb8 enter
viewer: to_rgb8 exit bytes=151875
viewer: show_image enter 225x225 bytes=151875
SHOW_IMAGE 225x225 151875
viewer: show_image exit result=0
DONE 0
```

実施済みの確認:

```text
cargo check -p fullerene-kernel -p solvent -p wasi_runtime
cargo test -p fullerene-kernel contexts::vfs::tests::copy_path_streams_complete_files_across_mounts -- --exact
cargo build -p fullerene-kernel --target x86_64-unknown-uefi
```

## 再発時の切り分け順

1. 実機が新しい `fullerene-kernel.efi` で起動していることを確認する。
2. Klog Liveを開いてからWASMを実行する。
3. `read complete` が出るか確認する。
4. `dimensions complete` と `decode exit` の間を確認する。
5. `thumbnail exit` が入力画像の寸法を超えていないか確認する。
6. `to_rgb8 exit` のバイト数が `width * height * 3` と一致するか確認する。
7. `show_image enter` 後まで進んだ場合は、window/surface境界を調べる。

今回のサンプルでは、`MAX_IMAGE_PIXELS` の値を変更するより、thumbnailで小画像を拡大しないことが有効だった。
