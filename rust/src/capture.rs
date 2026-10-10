//! 画面の一部を撮る。Windows Graphics Capture（OBS と同じ OS 標準の仕組み）を使い、
//! 使えないときは GDI（BitBlt）で撮る。座標はメイン画面の物理ピクセル。

use std::sync::mpsc;
use std::time::Duration;

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SRCCOPY, SelectObject,
};
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings,
    SecondaryWindowSettings, Settings,
};

use crate::image::{Bgra, Region};

type Reply = mpsc::SyncSender<Result<Bgra, String>>;

struct OneShot {
    region: Region,
    reply: Option<Reply>,
}

impl GraphicsCaptureApiHandler for OneShot {
    type Flags = (Region, Reply);
    type Error = String;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { region: ctx.flags.0, reply: Some(ctx.flags.1) })
    }

    fn on_frame_arrived(&mut self, frame: &mut Frame, control: InternalCaptureControl) -> Result<(), Self::Error> {
        let Some(reply) = self.reply.take() else { return Ok(()) };
        let r = self.region;
        let (fw, fh) = (frame.width(), frame.height());
        let (x1, y1) = ((r.x.max(0) as u32).min(fw), (r.y.max(0) as u32).min(fh));
        let (x2, y2) = (((r.x + r.w as i32).max(0) as u32).min(fw), ((r.y + r.h as i32).max(0) as u32).min(fh));
        let result = frame
            .buffer_crop(x1, y1, x2, y2)
            .map(|buf| {
                let mut tmp = Vec::new();
                let data = buf.as_nopadding_buffer(&mut tmp).to_vec();
                Bgra { w: x2 - x1, h: y2 - y1, data }
            })
            .map_err(|e| e.to_string());
        let _ = reply.send(result);
        control.stop();
        Ok(())
    }
}

fn grab_wgc(region: Region) -> Result<Bgra, String> {
    let monitor = Monitor::primary().map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::sync_channel(1);
    let make = |border| {
        Settings::new(
            monitor,
            CursorCaptureSettings::WithoutCursor,
            border,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            (region, tx.clone()),
        )
    };
    // 黄色い枠を消す設定は古い Windows では使えないので、だめなら枠ありで撮る
    let control = match OneShot::start_free_threaded(make(DrawBorderSettings::WithoutBorder)) {
        Ok(c) => c,
        Err(_) => OneShot::start_free_threaded(make(DrawBorderSettings::Default)).map_err(|e| e.to_string())?,
    };
    let result = rx.recv_timeout(Duration::from_secs(3)).map_err(|_| "画面キャプチャがタイムアウトしました".to_string());
    let _ = control.stop();
    result?
}

fn grab_gdi(region: Region) -> Result<Bgra, String> {
    let (w, h) = (region.w as i32, region.h as i32);
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let ok = BitBlt(mem, 0, 0, w, h, Some(screen), region.x, region.y, SRCCOPY | CAPTUREBLT);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // 上から下の順で受け取る
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut data = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(mem, bmp, 0, h as u32, Some(data.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        ok.map_err(|e| e.to_string())?;
        if lines == 0 {
            return Err("GetDIBits に失敗しました".into());
        }
        for px in data.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Ok(Bgra { w: w as u32, h: h as u32, data })
    }
}

pub fn grab(region: Region) -> Result<Bgra, String> {
    match grab_wgc(region) {
        Ok(img) if img.w > 0 && img.h > 0 => Ok(img),
        Ok(_) => Err("範囲が画面の外です".into()),
        Err(e) => {
            log::warn!("Windows Graphics Capture で撮れなかったので GDI で撮ります: {e}");
            grab_gdi(region)
        }
    }
}
