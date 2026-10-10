//! Windows 標準の OCR（Windows.Media.Ocr）で英語を読む。追加インストール不要。

use image::{ImageBuffer, Rgba, imageops};
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::core::HSTRING;

use crate::image::Bgra;
use crate::layout::TextBlock;

pub struct WindowsOcr {
    engine: OcrEngine,
    upscale: f64,
}

impl WindowsOcr {
    pub fn new(language: &str, upscale: f64) -> Result<Self, String> {
        let lang = Language::CreateLanguage(&HSTRING::from(language)).map_err(|e| e.to_string())?;
        let engine = OcrEngine::TryCreateFromLanguage(&lang).map_err(|_| {
            format!(
                "{language} の OCR が使えません。Windows の設定 → 時刻と言語 → 言語 で英語を追加し、OCR（光学式文字認識）を入れてください。"
            )
        })?;
        Ok(Self { engine, upscale: upscale.max(1.0) })
    }

    /// 画像から行ごとの文字と位置（元画像の座標）を返す。
    pub fn recognize(&self, img: &Bgra) -> Result<Vec<TextBlock>, String> {
        // 小さい文字は拡大した方がよく読める。ただし OCR が受け付ける大きさまで
        let max = OcrEngine::MaxImageDimension().unwrap_or(10000) as f64;
        let scale = self.upscale.min(max / img.w.max(img.h) as f64).max(0.1);
        let (w, h) = (((img.w as f64) * scale) as u32, ((img.h as f64) * scale) as u32);
        let data = if (scale - 1.0).abs() > 1e-3 {
            let src: ImageBuffer<Rgba<u8>, &[u8]> =
                ImageBuffer::from_raw(img.w, img.h, &img.data[..]).ok_or("画像の大きさが正しくありません")?;
            imageops::resize(&src, w, h, imageops::FilterType::CatmullRom).into_raw()
        } else {
            img.data.clone()
        };

        let run = || -> windows::core::Result<Vec<TextBlock>> {
            let writer = DataWriter::new()?;
            writer.WriteBytes(&data)?;
            let buffer = writer.DetachBuffer()?;
            let bitmap = SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, w as i32, h as i32)?;
            let result = self.engine.RecognizeAsync(&bitmap)?.join()?;
            let mut lines = Vec::new();
            for line in result.Lines()? {
                let (mut x1, mut y1, mut x2, mut y2) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for word in line.Words()? {
                    let r = word.BoundingRect()?;
                    x1 = x1.min(r.X);
                    y1 = y1.min(r.Y);
                    x2 = x2.max(r.X + r.Width);
                    y2 = y2.max(r.Y + r.Height);
                }
                if x1 > x2 {
                    continue;
                }
                let s = scale as f32;
                lines.push(TextBlock {
                    text: line.Text()?.to_string(),
                    x: x1 / s,
                    y: y1 / s,
                    w: (x2 - x1) / s,
                    h: (y2 - y1) / s,
                });
            }
            Ok(lines)
        };
        run().map_err(|e| format!("OCR に失敗しました: {e}"))
    }
}
