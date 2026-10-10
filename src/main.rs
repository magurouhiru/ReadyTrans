// exe をダブルクリックしたときに黒いコンソールを出さない
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
// Windows 以外では単体テストだけを動かすので、使わない部分の警告を出さない
#![cfg_attr(not(windows), allow(dead_code))]

mod cache;
mod config;
mod image;
mod layout;
mod logger;
mod translator;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod ocr;
#[cfg(windows)]
mod selftest;

#[cfg(windows)]
fn error_dialog(message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::HSTRING;
    unsafe {
        MessageBoxW(None, &HSTRING::from(message), &HSTRING::from("ReadyTrans"), MB_OK | MB_ICONERROR);
    }
}

#[cfg(windows)]
fn main() {
    // 設定・キャッシュ・ログは exe の隣に置く。初回は編集できるよう設定のひな形を書き出す
    let base_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    if let Err(e) = config::write_defaults(&base_dir) {
        eprintln!("設定ファイルを書き出せませんでした: {e}");
    }
    logger::init(&base_dir.join("readytrans.log"));
    log::info!("ReadyTrans {} を起動します（{}）", env!("CARGO_PKG_VERSION"), base_dir.display());

    if std::env::args().any(|a| a == "--self-test") {
        std::process::exit(selftest::run(&base_dir));
    }
    if let Err(e) = app::run(base_dir.clone()) {
        // exe ではコンソールが無いので、起動できない理由をダイアログで見せる
        log::error!("起動に失敗しました: {e}");
        error_dialog(&format!(
            "起動できませんでした:\n{e}\n\n詳しくは {} を見てください。",
            base_dir.join("readytrans.log").display()
        ));
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ReadyTrans は Windows 専用です。");
    std::process::exit(1);
}
