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

## Klog Liveのハング中再描画

WASMに限らず、同期処理やハングした処理が通常のイベントループを止めると、Klogを書き込むだけではKlog Live画面が更新されない。

Klog Liveを開いている間は、タイマー割込みがKlogの変更世代を確認し、通常のイベントループやcompositorが止まっていても、既存Klog Liveウィンドウのクライアント領域へ直接再描画する。

1. Klogリングを `try_lock` で読む。
2. ロック取得に失敗した場合は待たずにスキップする。
3. 取得できた最新ログを、既存ウィンドウのタイトルバー下にあるクライアント領域へ、直接マップ済みのframebufferを使って描画する。

通常時は従来通りcompositorがKlog Liveウィンドウを描画する。直接再描画は最大100列×29行に制限し、ログが更新された時だけ実行する。別のウィンドウ、枠、タイトルバーは作成・描画しないため、Klog Liveを閉じた後に残骸が残らない。

この仕組みで救えるのは、CPUがタイマー割込みを受け続け、framebufferの直接書き込みが可能な場合である。割込み禁止、CPU例外、framebuffer/MMIO自体の停止、ページテーブル破壊などは対象外になる。また、ログを書いていない無限ループでは新しい内容は増えないが、最後に描画されたKlog Liveの内容は保持される。

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

## MP4の追加切り分け

MP4では、サイズ確認後に停止するケースがある。現行のviewerソースには
`MP4 size OK` というログ文字列は存在しないため、実機へ投入した
`viewer.wasm` が古い世代でないかを先に確認する。

現行版は次の境界を順番にKlogへ出す。

```text
viewer: mp4 header enter/exit
viewer: mp4 video track scan enter/exit
viewer: mp4 sample read enter/exit
viewer: mp4 decoder config enter/exit
viewer: mp4 avcc parse enter/exit
viewer: mp4 decode nal enter/frame exit
viewer: mp4 create_window enter/exit
viewer: mp4 update_window enter/exit
```

`mp4 header enter` の後で止まる場合はMP4 atom解析、`decode nal enter` の後で
止まる場合はH.264デコーダ、`create_window` 以降ならWASM host callbackまたは
ウィンドウ描画を調べる。viewer単体だけでなく、次のUEFIビルドで生成された
成果物を実機へ投入する。

なお、Klog Liveで次のように見える場合はMP4処理前である。

```text
WASM DIAG READ ENTIRE BEGIN PATH ... FILE SIZE OK
```

これはkernelの `read_entire_file()` が `file_size()` を終えた直後の表示で、
`open_file()` または最初のVFS readへ進んでいない状態を示す。現行版では
その前後とread call/offsetも記録する。

実機での `Bad Apple!!` MP4（9,121,103 bytes）では、次のログまで進んだ。

```text
read_entire read begin ... call=256 offset=1044480
```

対応する `read exit ... total=1048576` が出ていないため、現時点の停止点は
WASMやMP4 decoderではなく、SD上のファイルを1MiB付近まで読むFAT/exFATまたは
RTSX SD controllerの読み出し経路である。

対策としてviewerの `.mp4` 経路は、ファイル全体を `std::fs::read()` でWASM
メモリへ取り込まず、seek可能なWASI `File` を `mp4::Mp4Reader` に直接渡す。
これにより、先頭の `moov` と最初のサンプルだけを必要な位置から読み、不要な
9MB全体読み出しを避ける。WASI側の `path_open` も全体キャッシュではなく、
ファイルサイズだけを取得して64KiB単位の遅延レンジ読み出しを行うよう変更した。

```bash
cargo build -p fullerene-kernel --target x86_64-unknown-uefi --offline
```

## MP4がハングせず再生されなかった原因

遅延読み出し版を実機で実行したところ、次の段階まで成功した。

```text
viewer: mp4 header exit
viewer: mp4 video track scan exit id=1
viewer: mp4 sample count=6572
viewer: mp4 sample read enter
viewer: mp4 sample read failed
```

`mp4` crateのsample IDは1始まりであり、viewerが最初のサンプルを
`read_sample(track_id, 0)` と指定していたため、MP4が正常でも最初のフレームを
取得できなかった。`read_sample(track_id, 1)`へ修正し、失敗時にはエラー内容も
表示するようにした。修正版のviewer build IDは
`2026-07-25-mp4-sample-index-2`。

その後の実機ログではsampleのNAL処理入口まで進んだが、最初のsampleだけを
処理した場合はdecoderがまだフレームを返さないことも判明した。これは
`rust_h264`が次のpicture開始時に前のpictureを返す設計で、最初のpictureは
保留されたままになるためである。sample NAL処理後に`decoder.flush()`を呼び、
保留中の480x360フレームを取得してwindowへ表示するよう修正した。

ネイティブWASIハーネスでは、修正版は次の順で完了した。

```text
viewer: mp4 decoder flush frame exit 480x360
viewer: mp4 create_window exit id=0
viewer: mp4 update_window exit
First frame decoded (480x360)
DONE code=0 elapsed=3.1s
```

この修正版のviewer build IDは`2026-07-25-mp4-flush-3`。

なお、ネイティブWASIハーネスでは同じNAL処理が約3秒で完了するため、`NAL
decode enter`後の停止は無限ループではなく、同期WASM実行がshell/compositorを
占有している状態と判断した。viewerのrelease profileは速度優先（`opt-level=3`）
へ変更し、shellの`wasm` commandは`spawn_wasm_app`で別kernel taskへ投入する。
これにより、デコーダが実機上で時間を要してもshellとKlog Liveの更新を塞がない。
