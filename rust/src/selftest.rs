//! `--self-test`: 画面を使わずに、OCR・設定・キャプチャが動くかを確かめる。
//! GitHub Actions の Windows でも実行して、配布する exe が壊れていないことを確認する。

use std::io::Write;
use std::path::Path;

use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection,
    CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DeleteDC, DeleteObject, OUT_DEFAULT_PRECIS, PatBlt, SelectObject,
    TextOutW, WHITENESS,
};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::core::w;

use crate::config::load_config;
use crate::image::{Bgra, Region};
use crate::layout::group_lines;
use crate::ocr::WindowsOcr;
use crate::translator::Translator;

/// 白地に黒で文字を書いた画像を作る。
fn draw_text(text: &str, w: i32, h: i32) -> Result<Bgra, String> {
    unsafe {
        let dc = CreateCompatibleDC(None);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).map_err(|e| e.to_string())?;
        let old_bmp = SelectObject(dc, bmp.into());
        let _ = PatBlt(dc, 0, 0, w, h, WHITENESS);
        let font = CreateFontW(
            40, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0,
            w!("Arial"),
        );
        let old_font = SelectObject(dc, font.into());
        let wide: Vec<u16> = text.encode_utf16().collect();
        let _ = TextOutW(dc, 20, 30, &wide);
        let mut data = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
        SelectObject(dc, old_font);
        SelectObject(dc, old_bmp);
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(dc);
        for px in data.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Ok(Bgra { w: w as u32, h: h as u32, data })
    }
}

pub fn run(base_dir: &Path) -> i32 {
    let mut report = Vec::new();
    let mut failed = false;
    let mut check = |name: &str, required: bool, result: Result<String, String>| {
        let line = match &result {
            Ok(detail) => format!("[OK]   {name}: {detail}"),
            Err(e) if required => format!("[FAIL] {name}: {e}"),
            Err(e) => format!("[WARN] {name}: {e}"),
        };
        if result.is_err() && required {
            failed = true;
        }
        log::info!("{line}");
        report.push(line);
    };

    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }

    let cfg = load_config(base_dir);
    check(
        "設定",
        true,
        cfg.as_ref().map(|c| format!("プロファイル {} / モデル {}", c.profile.name, c.llm.model)).map_err(Clone::clone),
    );

    let ocr_result = (|| {
        let ocr = WindowsOcr::new("en-US", 2.0)?;
        let img = draw_text("Press E to open the stash", 640, 100)?;
        let blocks = group_lines(&ocr.recognize(&img)?);
        let text = blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join(" ");
        if text.to_lowercase().contains("stash") { Ok(format!("{text:?}")) } else { Err(format!("読めた文字: {text:?}")) }
    })();
    check("OCR", true, ocr_result);

    if let Ok(cfg) = &cfg {
        let t = Translator::new(&cfg.llm, &cfg.profile, None);
        check(
            "翻訳AI への接続",
            false,
            t.installed_models().map(|m| format!("{} 個のモデル", m.len())),
        );
    }

    check(
        "画面キャプチャ",
        false,
        crate::capture::grab(Region { x: 0, y: 0, w: 64, h: 64 }).map(|img| format!("{}x{}", img.w, img.h)),
    );

    let summary = if failed { "自己診断: 失敗" } else { "自己診断: 成功" };
    log::info!("{summary}");
    report.push(summary.to_string());
    if let Ok(mut f) = std::fs::File::create(base_dir.join("selftest.log")) {
        let _ = writeln!(f, "{}", report.join("\n"));
    }
    if failed { 1 } else { 0 }
}
