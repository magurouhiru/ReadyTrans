//! アプリ本体: ホットキー → 範囲選択 → キャプチャ → OCR → 翻訳 → オーバーレイ表示。
//!
//! 画面全体を覆う透明・最前面・クリック透過のウィンドウを1枚だけ作り、
//! 範囲選択も訳文の表示もそこに描く。ゲームには一切触れない。

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, StrokeKind, Vec2, ViewportCommand};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, HWND_TOPMOST, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SetWindowDisplayAffinity, SetWindowPos,
    WDA_EXCLUDEFROMCAPTURE,
};

use crate::cache::TranslationCache;
use crate::config::{Config, OverlayConfig, load_config};
use crate::image::Region;
use crate::layout::{TextBlock, group_lines};
use crate::ocr::WindowsOcr;
use crate::translator::Translator;
use crate::capture;

// ---------------------------------------------------------------------------
// 重い処理（キャプチャ・OCR・翻訳）。別スレッドで動かす

enum Job {
    Run { region: Region, force: bool },
}

enum UiMsg {
    Show { region: Region, blocks: Vec<TextBlock>, translations: Vec<Option<String>> },
    NoText(Region),
    Failed { region: Region, message: String },
    Notify(String),
    Finished,
}

struct Worker {
    tx: Sender<UiMsg>,
    ctx: egui::Context,
    preparing: Arc<Mutex<Option<String>>>,
    last_text: Option<String>,
}

impl Worker {
    fn send(&self, msg: UiMsg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }

    fn run(mut self, cfg: Config, base_dir: PathBuf, jobs: Receiver<Job>) {
        // OCR（WinRT）をこのスレッドで使えるようにする
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let ocr = WindowsOcr::new(&cfg.ocr.language, cfg.ocr.upscale);
        if let Err(e) = &ocr {
            log::error!("{e}");
            self.send(UiMsg::Notify(e.clone()));
        }
        let cache = Arc::new(TranslationCache::open(&base_dir.join("cache.jsonl")));
        let translator = Translator::new(&cfg.llm, &cfg.profile, Some(cache));
        self.warmup(&translator);

        for job in jobs {
            let Job::Run { region, force } = job;
            let result = match &ocr {
                Ok(ocr) => self.work(ocr, &translator, region, force),
                Err(e) => Err(e.clone()),
            };
            if let Err(message) = result {
                log::error!("処理に失敗しました: {message}");
                self.send(UiMsg::Failed { region, message });
            }
            self.send(UiMsg::Finished);
        }
    }

    fn set_preparing(&self, message: Option<String>) {
        *self.preparing.lock().unwrap() = message;
    }

    fn warmup(&self, translator: &Translator) {
        let model = translator.model().to_string();
        let mut notified = false;
        let downloaded = translator
            .ensure_model(|msg| {
                if !notified {
                    self.send(UiMsg::Notify(msg.clone())); // ダウンロード開始を一度だけ通知する
                    notified = true;
                }
                self.set_preparing(Some(msg));
            })
            .unwrap_or_else(|e| {
                log::warn!("翻訳モデルの確認・ダウンロードに失敗しました: {e}");
                self.send(UiMsg::Notify(format!("翻訳モデルを用意できませんでした: {e}")));
                false
            });
        self.set_preparing(Some(format!("翻訳モデル {model} を読み込み中…")));
        match translator.warmup() {
            Ok(()) => {
                log::info!("翻訳モデル {model} を読み込みました");
                if downloaded {
                    self.send(UiMsg::Notify("翻訳の準備ができました".into()));
                }
            }
            Err(e) => log::warn!("翻訳モデルの事前読み込みに失敗しました: {e}"),
        }
        self.set_preparing(None);
    }

    fn work(&mut self, ocr: &WindowsOcr, translator: &Translator, region: Region, force: bool) -> Result<(), String> {
        let start = Instant::now();
        let image = capture::grab(region)?;
        log::info!("キャプチャ: {region:?} → 画像 {}x{}", image.w, image.h);
        let blocks = group_lines(&ocr.recognize(&image)?);
        log::info!("OCR 完了（{:.2} 秒）: {} 件", start.elapsed().as_secs_f64(), blocks.len());
        for b in &blocks {
            log::info!("  OCR: {:?} @ ({:.0}, {:.0}, {:.0} x {:.0})", b.text, b.x, b.y, b.w, b.h);
        }
        let text = blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n");
        if !force && self.last_text.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        self.last_text = Some(text);
        if blocks.is_empty() {
            self.send(UiMsg::NoText(region));
            return Ok(());
        }
        // 訳せた段落から順に表示する（まだのものは「…」）
        let texts: Vec<String> = blocks.iter().map(|b| b.text.clone()).collect();
        let mut translations: Vec<Option<String>> = vec![None; blocks.len()];
        self.send(UiMsg::Show { region, blocks: blocks.clone(), translations: translations.clone() });
        let start = Instant::now();
        log::info!("翻訳開始: {} 件", blocks.len());
        translator.translate_each(&texts, |i, ja| {
            translations[i] = Some(ja);
            self.send(UiMsg::Show { region, blocks: blocks.clone(), translations: translations.clone() });
        })?;
        log::info!("翻訳完了（{:.2} 秒）", start.elapsed().as_secs_f64());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 画面（オーバーレイ・範囲選択・トレイ・ホットキー）

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Select,
    Retranslate,
    ToggleAuto,
    Hide,
    Quit,
}

#[derive(Default)]
struct Overlay {
    region: Option<Region>,
    status: Option<(String, bool)>,
    items: Vec<(TextBlock, Option<String>)>,
    clear_at: Option<Instant>,
}

struct Selection {
    start: Option<Pos2>,
    end: Option<Pos2>,
}

struct Tray {
    _icon: TrayIcon,
    auto_item: CheckMenuItem,
    ids: Vec<(MenuId, Action)>,
}

pub struct App {
    cfg: Config,
    jobs: Sender<Job>,
    msgs: Receiver<UiMsg>,
    actions: Receiver<Action>,
    preparing: Arc<Mutex<Option<String>>>,
    _hotkeys: Option<GlobalHotKeyManager>,
    tray: Option<Tray>,
    hwnd: Option<HWND>,
    expanded: Option<bool>,
    busy: bool,
    region: Option<Region>,
    auto: bool,
    last_auto: Instant,
    overlay: Overlay,
    selection: Option<Selection>,
    toasts: Vec<(String, Instant)>,
}

fn font_candidates(family: &str) -> Vec<PathBuf> {
    let dir = Path::new(r"C:\Windows\Fonts");
    let mut list = Vec::new();
    let lower = family.to_lowercase();
    if [".ttf", ".ttc", ".otf"].iter().any(|e| lower.ends_with(e)) {
        list.push(dir.join(family)); // 絶対パスならそのまま使われる
    }
    if lower.contains("meiryo") || family.contains("メイリオ") {
        list.push(dir.join("meiryo.ttc"));
    }
    for name in ["YuGothM.ttc", "YuGothR.ttc", "meiryo.ttc", "msgothic.ttc"] {
        list.push(dir.join(name));
    }
    list
}

/// 日本語を表示できるよう、Windows に入っているフォントを読み込む。
fn install_japanese_font(ctx: &egui::Context, family: &str) {
    let Some((path, bytes)) = font_candidates(family).into_iter().find_map(|p| std::fs::read(&p).ok().map(|b| (p, b)))
    else {
        log::warn!("日本語フォントが見つかりませんでした");
        return;
    };
    log::info!("フォント: {}", path.display());
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("ja".into(), Arc::new(egui::FontData::from_owned(bytes)));
    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(fam).or_default().insert(0, "ja".into());
    }
    ctx.set_fonts(fonts);
}

fn build_tray(cfg: &Config) -> Result<Tray, String> {
    let menu = Menu::new();
    let select = MenuItem::new("範囲を選んで訳す", true, None);
    let auto_item = CheckMenuItem::new("自動モード", true, false, None);
    let hide = MenuItem::new("表示を消す", true, None);
    let quit = MenuItem::new("終了", true, None);
    menu.append_items(&[&select, &auto_item, &hide, &PredefinedMenuItem::separator(), &quit])
        .map_err(|e| e.to_string())?;
    let ids = vec![
        (select.id().clone(), Action::Select),
        (auto_item.id().clone(), Action::ToggleAuto),
        (hide.id().clone(), Action::Hide),
        (quit.id().clone(), Action::Quit),
    ];
    // 青い四角のアイコン
    let rgba: Vec<u8> = (0..32 * 32).flat_map(|_| [40u8, 140, 220, 255]).collect();
    let icon = Icon::from_rgba(rgba, 32, 32).map_err(|e| e.to_string())?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(format!("ReadyTrans（{} / {}）", cfg.profile.name, cfg.llm.model))
        .with_icon(icon)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(Tray { _icon: tray, auto_item, ids })
}

fn register_hotkeys(cfg: &Config, tx: Sender<Action>, ctx: egui::Context) -> (Option<GlobalHotKeyManager>, Vec<String>) {
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => return (None, vec![format!("ホットキーを使えません: {e}")]),
    };
    let h = &cfg.hotkeys;
    let mut map: Vec<(u32, Action)> = Vec::new();
    let mut failed = Vec::new();
    for (text, action) in [
        (&h.select_and_translate, Action::Select),
        (&h.retranslate, Action::Retranslate),
        (&h.toggle_auto, Action::ToggleAuto),
        (&h.hide, Action::Hide),
    ] {
        match HotKey::from_str(text) {
            Ok(key) if manager.register(key).is_ok() => map.push((key.id(), action)),
            Ok(_) => failed.push(text.clone()),
            Err(e) => failed.push(format!("{text}（{e}）")),
        }
    }
    let tx = Mutex::new(tx);
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.state != HotKeyState::Pressed {
            return;
        }
        if let Some((_, action)) = map.iter().find(|(id, _)| *id == e.id) {
            let _ = tx.lock().unwrap().send(*action);
            ctx.request_repaint();
        }
    }));
    (Some(manager), failed)
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Config, base_dir: PathBuf) -> Self {
        let ctx = cc.egui_ctx.clone();
        install_japanese_font(&ctx, &cfg.overlay.font_family);
        ctx.set_visuals(egui::Visuals::dark());

        let (job_tx, job_rx) = mpsc::channel();
        let (msg_tx, msg_rx) = mpsc::channel();
        let preparing = Arc::new(Mutex::new(Some("翻訳モデルを準備中…".to_string())));
        let worker = Worker { tx: msg_tx, ctx: ctx.clone(), preparing: preparing.clone(), last_text: None };
        let worker_cfg = cfg.clone();
        std::thread::spawn(move || worker.run(worker_cfg, base_dir, job_rx));

        let (action_tx, action_rx) = mpsc::channel();
        let (hotkeys, failed) = register_hotkeys(&cfg, action_tx.clone(), ctx.clone());

        let tray = match build_tray(&cfg) {
            Ok(t) => Some(t),
            Err(e) => {
                log::warn!("トレイアイコンを作れませんでした: {e}");
                None
            }
        };
        let menu_tx = Mutex::new(action_tx);
        let ids: Vec<(MenuId, Action)> = tray.as_ref().map(|t| t.ids.clone()).unwrap_or_default();
        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if let Some((_, action)) = ids.iter().find(|(id, _)| *id == e.id) {
                let _ = menu_tx.lock().unwrap().send(*action);
                menu_ctx.request_repaint();
            }
        }));

        let mut app = Self {
            jobs: job_tx,
            msgs: msg_rx,
            actions: action_rx,
            preparing,
            _hotkeys: hotkeys,
            tray,
            hwnd: None,
            expanded: None,
            busy: false,
            region: None,
            auto: false,
            last_auto: Instant::now(),
            overlay: Overlay::default(),
            selection: None,
            toasts: Vec::new(),
            cfg,
        };
        if failed.is_empty() {
            app.toast(format!("起動しました。{} で範囲を選んで訳します。", app.cfg.hotkeys.select_and_translate));
        } else {
            app.toast(format!("次のホットキーは他のアプリが使っているため登録できませんでした: {}", failed.join(", ")));
        }
        app
    }

    fn toast(&mut self, text: String) {
        log::info!("通知: {text}");
        self.toasts.push((text, Instant::now() + Duration::from_secs(5)));
    }

    // --- 操作 ---

    fn handle_action(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Select => self.start_select(ctx),
            Action::Retranslate => {
                if let Some(region) = self.region {
                    self.run(region, true);
                }
            }
            Action::ToggleAuto => self.set_auto(!self.auto),
            Action::Hide => self.overlay = Overlay::default(),
            Action::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
        }
    }

    fn start_select(&mut self, ctx: &egui::Context) {
        self.overlay = Overlay::default();
        self.selection = Some(Selection { start: None, end: None });
        ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn end_select(&mut self, ctx: &egui::Context, region: Option<Region>) {
        self.selection = None;
        ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(true));
        if let Some(region) = region {
            self.region = Some(region);
            self.run(region, true);
        }
    }

    fn set_auto(&mut self, on: bool) {
        if on && self.region.is_none() {
            self.toast(format!("先に範囲を選んでください（{}）", self.cfg.hotkeys.select_and_translate));
            self.auto = false;
        } else {
            self.auto = on;
            log::info!("自動モード: {}", if on { "オン" } else { "オフ" });
        }
        if let Some(t) = &self.tray {
            t.auto_item.set_checked(self.auto);
        }
    }

    fn run(&mut self, region: Region, force: bool) {
        let preparing = self.preparing.lock().unwrap().clone();
        if let Some(message) = preparing {
            // モデルのダウンロード・読み込みが終わるまでは進み具合だけ見せる
            if force {
                self.show_status(region, message, false);
            }
            return;
        }
        if self.busy {
            return;
        }
        self.busy = true;
        if force {
            self.show_status(region, "読み取り中…".into(), false);
        }
        let _ = self.jobs.send(Job::Run { region, force });
    }

    fn show_status(&mut self, region: Region, text: String, error: bool) {
        self.overlay.region = Some(region);
        self.overlay.status = Some((text, error));
        self.overlay.clear_at = None;
    }

    fn handle_msg(&mut self, msg: UiMsg) {
        match msg {
            UiMsg::Show { region, blocks, translations } => {
                if translations.iter().all(Option::is_some) {
                    log::info!("表示: {} 件", blocks.len());
                }
                self.overlay = Overlay {
                    region: Some(region),
                    status: None,
                    items: blocks.into_iter().zip(translations).collect(),
                    clear_at: None,
                };
            }
            UiMsg::NoText(region) => {
                self.show_status(region, "文字が見つかりませんでした".into(), true);
                self.overlay.clear_at = Some(Instant::now() + Duration::from_secs(2));
            }
            UiMsg::Failed { region, message } => {
                self.show_status(region, format!("エラー: {message}"), true);
                self.toast(format!("エラー: {message}"));
            }
            UiMsg::Notify(text) => self.toast(text),
            UiMsg::Finished => self.busy = false,
        }
    }

    // --- ウィンドウ ---

    /// 何も表示しないときはウィンドウを 1px に縮め、ゲームの上を覆わないようにする。
    fn set_expanded(&mut self, ctx: &egui::Context, expanded: bool) {
        let Some(hwnd) = self.hwnd else { return };
        if self.expanded == Some(expanded) {
            return;
        }
        self.expanded = Some(expanded);
        // 画面全体をぴったり覆うと、ドライバが「全画面のゲーム」とみなして透明にならず真っ暗になる。
        // 下を 1px だけ空けて、普通のウィンドウとして重ねる
        let (w, h) = if expanded {
            unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN) - 1) }
        } else {
            (1, 1)
        };
        unsafe {
            if let Err(e) = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, w, h, SWP_NOACTIVATE) {
                log::warn!("ウィンドウの位置を変えられませんでした: {e}");
            }
        }
        log::info!("オーバーレイ: {w}x{h}");
        ctx.request_repaint();
    }

    fn init_window(&mut self, frame: &eframe::Frame) {
        if self.hwnd.is_some() {
            return;
        }
        let Ok(handle) = frame.window_handle() else { return };
        let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
        let hwnd = HWND(h.hwnd.get() as *mut _);
        self.hwnd = Some(hwnd);
        // 自分のウィンドウが画面キャプチャに写り込まないようにする（Windows 10 2004 以降）
        unsafe {
            if let Err(e) = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) {
                log::warn!("キャプチャ除外を設定できませんでした: {e}");
            }
        }
    }

    // --- 描画 ---

    fn to_points(&self, ppp: f32, region: Region, b: &TextBlock) -> Rect {
        Rect::from_min_size(
            Pos2::new((region.x as f32 + b.x) / ppp, (region.y as f32 + b.y) / ppp),
            Vec2::new(b.w / ppp, b.h / ppp),
        )
    }

    fn paint_selection(&mut self, ui: &mut egui::Ui) -> Option<Option<Region>> {
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let painter = ui.painter();
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        let sel = self.selection.as_mut()?;

        let (pressed, down, released, pos, escape, secondary) = ctx.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.primary_released(),
                i.pointer.latest_pos(),
                i.key_pressed(egui::Key::Escape),
                i.pointer.secondary_pressed(),
            )
        });
        if escape || secondary {
            return Some(None);
        }
        if pressed {
            sel.start = pos;
            sel.end = pos;
        } else if down && sel.start.is_some() {
            sel.end = pos.or(sel.end);
        }

        let dim = Color32::from_black_alpha(90);
        let rect = match (sel.start, sel.end) {
            (Some(a), Some(b)) => Some(Rect::from_two_pos(a, b)),
            _ => None,
        };
        match rect {
            Some(r) => {
                // 選んだ範囲の外側だけを暗くする
                painter.rect_filled(Rect::from_min_max(screen.min, Pos2::new(screen.max.x, r.min.y)), 0.0, dim);
                painter.rect_filled(Rect::from_min_max(Pos2::new(screen.min.x, r.max.y), screen.max), 0.0, dim);
                painter.rect_filled(Rect::from_min_max(Pos2::new(screen.min.x, r.min.y), Pos2::new(r.min.x, r.max.y)), 0.0, dim);
                painter.rect_filled(Rect::from_min_max(Pos2::new(r.max.x, r.min.y), Pos2::new(screen.max.x, r.max.y)), 0.0, dim);
                painter.rect_stroke(r, 0.0, Stroke::new(2.0, Color32::from_rgb(80, 200, 255)), StrokeKind::Outside);
            }
            None => {
                painter.rect_filled(screen, 0.0, dim);
            }
        }
        painter.text(
            Pos2::new(20.0, 20.0),
            egui::Align2::LEFT_TOP,
            "訳したい範囲をドラッグしてください（Esc か右クリックで中止）",
            FontId::proportional(18.0),
            Color32::WHITE,
        );

        if released && let Some(r) = rect {
            let ppp = ctx.pixels_per_point();
            if r.width() * ppp < 8.0 || r.height() * ppp < 8.0 {
                return Some(None);
            }
            return Some(Some(Region {
                x: (r.min.x * ppp) as i32,
                y: (r.min.y * ppp) as i32,
                w: (r.width() * ppp) as u32,
                h: (r.height() * ppp) as u32,
            }));
        }
        None
    }

    fn paint_overlay(&self, ui: &egui::Ui) {
        let ctx = ui.ctx();
        let ppp = ctx.pixels_per_point();
        let painter = ui.painter();
        let cfg = &self.cfg.overlay;
        let o = &self.overlay;
        let Some(region) = o.region else { return };
        let r = self.to_points(ppp, region, &TextBlock { text: String::new(), x: 0.0, y: 0.0, w: region.w as f32, h: region.h as f32 });

        if let Some((text, error)) = &o.status {
            let color = if *error { Color32::from_rgb(255, 120, 120) } else { Color32::from_rgb(255, 220, 120) };
            let rect = Rect::from_min_size(Pos2::new(r.min.x, (r.min.y - 28.0).max(0.0)), Vec2::new(r.width().max(260.0), 26.0));
            draw_box(painter, cfg, rect, text, color);
        }

        let text_of = |ja: &Option<String>| match ja {
            None => "…".to_string(),
            Some(s) if s.is_empty() => "（訳せませんでした）".to_string(),
            Some(s) => s.clone(),
        };
        if cfg.mode == "panel" && !o.items.is_empty() {
            let text = o.items.iter().map(|(_, ja)| text_of(ja)).collect::<Vec<_>>().join("\n");
            let rect = Rect::from_min_size(Pos2::new(r.min.x, r.max.y + 6.0), Vec2::new(r.width(), 40.0));
            draw_box(painter, cfg, rect, &text, Color32::WHITE);
        } else {
            for (b, ja) in &o.items {
                draw_box(painter, cfg, self.to_points(ppp, region, b), &text_of(ja), Color32::WHITE);
            }
        }
    }

    fn paint_toasts(&self, ui: &egui::Ui) {
        let screen = ui.ctx().content_rect();
        let mut y = screen.max.y - 60.0;
        for (text, _) in self.toasts.iter().rev() {
            let rect = Rect::from_min_size(Pos2::new(screen.max.x - 440.0, y - 40.0), Vec2::new(420.0, 40.0));
            let used = draw_box(ui.painter(), &self.cfg.overlay, rect, text, Color32::WHITE);
            y = used.min.y - 8.0 - (used.height() - rect.height());
        }
    }
}

/// 枠に収まるまで文字を小さくし、それでも無理なら下に伸ばして描く。描いた枠を返す。
fn draw_box(painter: &egui::Painter, cfg: &OverlayConfig, rect: Rect, text: &str, color: Color32) -> Rect {
    let bg = Color32::from_black_alpha((255.0 * cfg.background_opacity.clamp(0.0, 1.0)) as u8);
    let wrap = (rect.width() - 8.0).max(32.0);
    let mut size = cfg.font_size.max(11.0);
    let galley = loop {
        let g = painter.layout(text.to_string(), FontId::proportional(size), color, wrap);
        if g.size().y + 6.0 <= rect.height() || size <= 11.0 {
            break g;
        }
        size -= 1.0;
    };
    let rect = Rect::from_min_size(rect.min, Vec2::new(rect.width().max(40.0), rect.height().max(galley.size().y + 6.0)));
    painter.rect_filled(rect, CornerRadius::same(4), bg);
    painter.galley(rect.min + Vec2::new(4.0, 3.0), galley, color);
    rect
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(action) = self.actions.try_recv() {
            log::info!("操作: {action:?}");
            self.handle_action(ctx, action);
        }
        while let Ok(msg) = self.msgs.try_recv() {
            self.handle_msg(msg);
        }
        let now = Instant::now();
        if self.overlay.clear_at.is_some_and(|t| now >= t) {
            self.overlay = Overlay::default();
        }
        self.toasts.retain(|(_, until)| now < *until);

        if self.auto && let Some(region) = self.region {
            let interval = Duration::from_millis(self.cfg.auto.interval_ms.max(100));
            if !self.busy && self.selection.is_none() && now.duration_since(self.last_auto) >= interval {
                self.last_auto = now;
                self.run(region, false);
            }
            ctx.request_repaint_after(interval);
        }
        if let Some(t) = self.overlay.clear_at {
            ctx.request_repaint_after(t.saturating_duration_since(now));
        }
        if let Some(t) = self.toasts.iter().map(|(_, t)| *t).min() {
            ctx.request_repaint_after(t.saturating_duration_since(now));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.init_window(frame);
        let ctx = ui.ctx().clone();
        let showing = self.selection.is_some()
            || self.overlay.status.is_some()
            || !self.overlay.items.is_empty()
            || !self.toasts.is_empty();
        self.set_expanded(&ctx, showing);

        if self.selection.is_some() {
            if let Some(result) = self.paint_selection(ui) {
                if let Some(r) = result {
                    log::info!("範囲: {r:?}");
                }
                self.end_select(&ctx, result);
            }
            ctx.request_repaint();
            return;
        }
        self.paint_overlay(ui);
        self.paint_toasts(ui);
    }
}

pub fn run(base_dir: PathBuf) -> Result<(), String> {
    let cfg = load_config(&base_dir)?;
    log::info!("プロファイル: {}（{}）", cfg.profile.name, cfg.profile.key);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ReadyTrans")
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_mouse_passthrough(true)
            .with_taskbar(false)
            .with_active(false)
            .with_resizable(false)
            .with_has_shadow(false)
            .with_position(Pos2::ZERO)
            .with_inner_size(Vec2::splat(1.0))
            .with_clamp_size_to_monitor_size(false),
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native("ReadyTrans", options, Box::new(move |cc| Ok(Box::new(App::new(cc, cfg, base_dir)))))
        .map_err(|e| format!("画面を作れませんでした: {e}"))
}
