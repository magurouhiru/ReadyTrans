//! readytrans.log とコンソールにログを出す。

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

struct Logger {
    file: Mutex<Option<File>>,
}

fn now() -> String {
    #[cfg(windows)]
    {
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
        )
    }
    #[cfg(not(windows))]
    {
        let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        format!("{}.{:03}", d.as_secs(), d.subsec_millis())
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        // 使っているライブラリの細かいログは出さない
        metadata.level() <= log::Level::Warn || metadata.target().starts_with("readytrans")
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("{} {} {}\n", now(), record.level(), record.args());
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
        eprint!("{line}");
    }

    fn flush(&self) {}
}

pub fn init(path: &Path) {
    let file = File::create(path).ok();
    let logger = Box::leak(Box::new(Logger { file: Mutex::new(file) }));
    let _ = log::set_logger(logger);
    log::set_max_level(log::LevelFilter::Info);
}
