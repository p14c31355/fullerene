use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // TTF font from the ttf-noto-sans crate (bundled, no network needed)
    let ttf_path = out_dir.join("LiberationSans-Regular.ttf");
    fs::write(&ttf_path, ttf_noto_sans::REGULAR).expect("write Noto Sans TTF");

    // 3. Pre-render SVG icons to RGBA pixel data (64×64 each)
    render_svg_icons(&out_dir);

    // 4. Pre-render wallpapers
    render_wallpapers(&out_dir);

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icons");
    println!("cargo:rerun-if-changed=assets/wallpapers");
}

fn render_svg_icons(out_dir: &Path) {
    let icons_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("icons");
    let svgs = [
        "shell.svg", "terminal.svg", "editor.svg", "clock.svg", "files.svg", "settings.svg",
        "about.svg",
    ];
    for svg_name in &svgs {
        let svg_path = icons_dir.join(svg_name);
        let svg_data = match fs::read(&svg_path) {
            Ok(d) => d,
            Err(_) => {
                println!("cargo:warning=SVG icon not found: {}", svg_path.display());
                continue;
            }
        };
        let rtree = match usvg::Tree::from_data(&svg_data, &usvg::Options::default()) {
            Ok(t) => t,
            Err(e) => {
                println!("cargo:warning=SVG parse error for {}: {}", svg_name, e);
                continue;
            }
        };
        let size = tiny_skia::IntSize::from_wh(64, 64).unwrap();
        let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height()).unwrap();
        resvg::render(
            &rtree,
            tiny_skia::Transform::default(),
            &mut pixmap.as_mut(),
        );
        let stem = svg_name.trim_end_matches(".svg");
        let bin_name = format!("icon_{}.rgba", stem);
        fs::write(out_dir.join(&bin_name), pixmap.data()).expect("write icon pixels");
    }
}

// ── Render an SVG file to a wallpaper pixel-data source file ───

fn render_svg_file(out_dir: &Path, name: &str, svg_path: &Path) {
    let svg_data = match fs::read(svg_path) {
        Ok(d) => d,
        Err(e) => {
            println!("cargo:warning=wallpaper {} read error: {}", name, e);
            return;
        }
    };
    let rtree = match usvg::Tree::from_data(&svg_data, &usvg::Options::default()) {
        Ok(t) => t,
        Err(e) => {
            println!("cargo:warning=wallpaper {} SVG parse error: {}", name, e);
            return;
        }
    };

    let w = rtree.size().width().ceil() as u32;
    let h = rtree.size().height().ceil() as u32;
    let mut pixmap = tiny_skia::Pixmap::new(w, h).unwrap();
    resvg::render(
        &rtree,
        tiny_skia::Transform::default(),
        &mut pixmap.as_mut(),
    );

    let pixel_count = (w * h) as usize;
    let prefix = name.to_uppercase();
    let mut code = format!(
        "const {}_W: u32 = {};\nconst {}_H: u32 = {};\nstatic {}_PIXELS: [u32; {}] = [",
        prefix, w, prefix, h, prefix, pixel_count
    );
    for y in 0..h {
        if y % 64 == 0 {
            code.push('\n');
        }
        for x in 0..w {
            let src_idx = (y * w + x) as usize * 4;
            let r = pixmap.data()[src_idx];
            let g = pixmap.data()[src_idx + 1];
            let b = pixmap.data()[src_idx + 2];
            write!(code, "0x{:02X}{:02X}{:02X},", r, g, b).unwrap();
        }
    }
    code.push_str("];\n");
    let filename = format!("wallpaper_{}.rs", name);
    fs::write(out_dir.join(&filename), code.as_bytes()).expect("write wallpaper pixel data");
    println!("cargo:notice=Rendered {} wallpaper ({}×{})", name, w, h);
}

// ── Wallpaper rendering dispatch ──────────────────────────

fn render_wallpapers(out_dir: &Path) {
    let wd = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("wallpapers");

    for name in &["beach", "mountain", "city", "fullerene"] {
        let mut svg_path = wd.join(name);
        svg_path.set_extension("svg");
        if !svg_path.exists() {
            println!(
                "cargo:error=wallpaper source SVG not found: {}",
                svg_path.display()
            );
            return;
        }
        render_svg_file(out_dir, name, &svg_path);
    }

    let png_path = wd.join("fullerene_sharp.png");
    if !png_path.exists() {
        println!(
            "cargo:error=wallpaper source PNG not found: {}",
            png_path.display()
        );
        return;
    }
    if let Ok(png_data) = fs::read(&png_path)
        && let Ok(pixmap) = tiny_skia::Pixmap::decode_png(&png_data)
    {
        let w = pixmap.width();
        let h = pixmap.height();
        let pixel_count = (w * h) as usize;
        let mut code = format!(
            "const SHARP_W: u32 = {};\nconst SHARP_H: u32 = {};\nstatic SHARP_PIXELS: [u32; {}] = [",
            w, h, pixel_count
        );
        for y in 0..h {
            if y % 64 == 0 {
                code.push('\n');
            }
            for x in 0..w {
                let src_idx = (y * w + x) as usize * 4;
                let r = pixmap.data()[src_idx];
                let g = pixmap.data()[src_idx + 1];
                let b = pixmap.data()[src_idx + 2];
                write!(code, "0x{:02X}{:02X}{:02X},", r, g, b).unwrap();
            }
        }
        code.push_str("];\n");
        fs::write(out_dir.join("wallpaper_sharp.rs"), code.as_bytes())
            .expect("write wallpaper PNG data");
        println!(
            "cargo:notice=Rendered fullerene_sharp wallpaper ({}×{})",
            w, h
        );
    }
}
