use jpeg_encoder::ColorType;
use jpeg_encoder::Encoder as JpegEncoder;
use rayon::prelude::*;
use std::fs;
use std::time::Instant;

mod image;

fn main() {
    let start = Instant::now();

    // ディレクトリ内容を取得
    let files: Vec<_> = fs::read_dir("./data")
        .unwrap()
        .filter_map(|x| x.ok())
        .collect();

    files.par_iter().enumerate().for_each(|(_index, entry)| {
        // ファイル名を取得
        let img_name = &entry.file_name().into_string().unwrap();

        // 画像を開く
        let open_data = format!("{}{}", "./data/", img_name);
        let img = image::open(open_data).unwrap();

        // jpeg圧縮率50%で画像を保存
        let img_out_name = format!("{}{}", "./data/OUT_", img_name);

        let encoder = JpegEncoder::new_file(img_out_name, 50).unwrap();
        let _ = encoder.encode(
            &img.to_rgb8(),
            img.width() as u16,
            img.height() as u16,
            ColorType::Rgb,
        );
    });

    let end = start.elapsed();
    println!(
        "処理時間：{}.{:03}秒",
        end.as_secs(),
        end.subsec_nanos() / 1_000_000
    );
}
