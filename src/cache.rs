//! 同じ英文を毎回訳さないためのキャッシュ。1行1件の JSON（JSON Lines）で追記していく。

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Entry {
    key: String,
    source: String,
    target: String,
}

pub struct TranslationCache {
    map: Mutex<HashMap<(String, String), String>>,
    file: Mutex<Option<File>>,
}

impl TranslationCache {
    pub fn open(path: &Path) -> Self {
        let mut map = HashMap::new();
        if let Ok(f) = File::open(path) {
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                // 壊れた行（書きかけで終了した等）は読み飛ばす
                if let Ok(e) = serde_json::from_str::<Entry>(&line) {
                    map.insert((e.key, e.source), e.target);
                }
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(path).ok();
        if file.is_none() {
            log::warn!("キャッシュファイルを開けません: {}", path.display());
        }
        Self { map: Mutex::new(map), file: Mutex::new(file) }
    }

    #[cfg(test)]
    pub fn memory() -> Self {
        Self { map: Mutex::new(HashMap::new()), file: Mutex::new(None) }
    }

    pub fn get(&self, key: &str, source: &str) -> Option<String> {
        let map = self.map.lock().unwrap();
        map.get(&(key.to_string(), source.to_string())).filter(|t| !t.is_empty()).cloned()
    }

    pub fn put(&self, key: &str, source: &str, target: &str) {
        if target.is_empty() {
            return; // 空の訳は保存しない（失敗を次回も使い回さないように）
        }
        self.map.lock().unwrap().insert((key.into(), source.into()), target.into());
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            let entry = Entry { key: key.into(), source: source.into(), target: target.into() };
            if let Ok(line) = serde_json::to_string(&entry) {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_and_skips_empty() {
        let dir = std::env::temp_dir().join(format!("readytrans-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.jsonl");
        let _ = std::fs::remove_file(&path);
        {
            let c = TranslationCache::open(&path);
            c.put("m|p", "Hello", "こんにちは");
            c.put("m|p", "Empty", "");
        }
        let c = TranslationCache::open(&path);
        assert_eq!(c.get("m|p", "Hello").as_deref(), Some("こんにちは"));
        assert_eq!(c.get("m|p", "Empty"), None);
        assert_eq!(c.get("other|p", "Hello"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
