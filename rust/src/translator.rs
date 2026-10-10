//! ローカル LLM（Ollama または OpenAI 互換 API）で英語を日本語に訳す。
//!
//! 段落ごとに1回ずつ問い合わせ、返事は少しずつ受け取る（ストリーミング）。
//! 長い文でも、AI が書き続けている限り時間切れにならない。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::cache::TranslationCache;
use crate::config::{LlmConfig, Profile};

pub const SYSTEM_PROMPT: &str = "\
あなたはゲームの翻訳者です。ゲーム画面から OCR で読み取った英語を自然な日本語に訳します。
- 訳文だけを返してください。説明や引用符は付けないでください。
- OCR の読み間違いらしい文字は文脈から推測して直してから訳してください。
- 数字、記号、キー表記（[E]、LMB など）はそのまま残してください。";

pub const TRANSLATEGEMMA_PROMPT: &str = "\
You are a professional English (en) to Japanese (ja) translator. Your goal is to accurately convey \
the meaning and nuances of the original English text while adhering to Japanese grammar, vocabulary, \
and cultural sensitivities.
Produce only the Japanese translation, without any additional explanations or commentary. \
Please translate the following English text into Japanese:


";

pub fn build_system_prompt(profile: &Profile) -> String {
    let mut parts = vec![SYSTEM_PROMPT.to_string()];
    if !profile.instructions.is_empty() {
        parts.push(format!("\nゲーム固有の指示:\n{}", profile.instructions));
    }
    if !profile.glossary.is_empty() {
        let lines: Vec<String> = profile
            .glossary
            .iter()
            .map(|(en, ja)| if ja.is_empty() { format!("- {en} → 英語のまま残す") } else { format!("- {en} → {ja}") })
            .collect();
        parts.push(format!("\n用語集（必ず従う）:\n{}", lines.join("\n")));
    }
    parts.join("\n")
}

/// タグ省略時は :latest として比べる（Ollama の扱いに合わせる）。
pub fn normalize_model_name(name: &str) -> String {
    if name.rsplit('/').next().unwrap_or(name).contains(':') { name.to_string() } else { format!("{name}:latest") }
}

/// モデル名から問い合わせ方を決める。翻訳専用モデルは決まった書式でないとうまく訳せない。
pub fn detect_style(model: &str) -> &'static str {
    let name = model.to_lowercase();
    if name.contains("translategemma") {
        "translategemma"
    } else if name.contains("lfm2") && name.contains("enjp") {
        "lfm2"
    } else {
        "chat"
    }
}

pub fn clean_output(text: &str) -> String {
    text.trim().trim_matches(|c| c == '"' || c == '「' || c == '」').trim().to_string()
}

pub struct Translator {
    llm: LlmConfig,
    profile_key: String,
    cache: Option<Arc<TranslationCache>>,
    pub style: String,
    system_prompt: String,
    think_off: AtomicBool,
    agent: ureq::Agent,
}

fn agent(read_timeout: Duration) -> ureq::Agent {
    // read の時間切れは「次のデータが届くまで」の待ち時間。ストリーミングなので書き続けている限り切れない
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(5)).timeout_read(read_timeout).build()
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock)
}

impl Translator {
    pub fn new(llm: &LlmConfig, profile: &Profile, cache: Option<Arc<TranslationCache>>) -> Self {
        let style = if llm.style != "auto" { llm.style.clone() } else { detect_style(&llm.model).to_string() };
        log::info!("翻訳モデル {}（問い合わせ方: {style}）", llm.model);
        Self {
            llm: llm.clone(),
            profile_key: profile.key.clone(),
            cache,
            style,
            system_prompt: build_system_prompt(profile),
            think_off: AtomicBool::new(true),
            agent: agent(Duration::from_secs_f64(llm.timeout_seconds.max(1.0))),
        }
    }

    pub fn model(&self) -> &str {
        &self.llm.model
    }

    fn base(&self) -> &str {
        self.llm.base_url.trim_end_matches('/')
    }

    fn http_error(&self, e: ureq::Error) -> String {
        match e {
            ureq::Error::Status(code, resp) => {
                let body = resp.into_string().unwrap_or_default();
                format!("翻訳AIがエラーを返しました（{code}）: {body}")
            }
            ureq::Error::Transport(t) => {
                format!("翻訳AI（{}）に接続できません。Ollama が起動しているか確認してください: {t}", self.llm.base_url)
            }
        }
    }

    fn timeout_message(&self) -> String {
        format!(
            "翻訳AIから {:.0} 秒以上返事がありませんでした。ゲームと AI で VRAM が足りていない可能性があります（`ollama ps` で確認できます）。",
            self.llm.timeout_seconds
        )
    }

    pub fn installed_models(&self) -> Result<Vec<String>, String> {
        let resp = self.agent.get(&format!("{}/api/tags", self.base())).call().map_err(|e| self.http_error(e))?;
        let data: Value = resp.into_json().map_err(|e| e.to_string())?;
        let mut names: Vec<String> = data["models"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["name"].as_str()).map(normalize_model_name).collect())
            .unwrap_or_default();
        names.sort();
        Ok(names)
    }

    /// 設定のモデルが Ollama に無ければダウンロードする。ダウンロードしたら true。
    pub fn ensure_model(&self, mut progress: impl FnMut(String)) -> Result<bool, String> {
        if self.llm.backend != "ollama" {
            return Ok(false);
        }
        let installed = self.installed_models()?;
        log::info!("Ollama にあるモデル: {}", if installed.is_empty() { "なし".into() } else { installed.join(", ") });
        if installed.contains(&normalize_model_name(&self.llm.model)) {
            return Ok(false);
        }

        let model = &self.llm.model;
        log::info!("{model} が無いのでダウンロードします");
        progress(format!("翻訳モデル {model} をダウンロード中…"));
        // ダウンロードは時間がかかるので、データが届く間隔だけで時間切れを判断する
        let resp = agent(Duration::from_secs(300))
            .post(&format!("{}/api/pull", self.base()))
            .send_json(json!({"model": model, "stream": true}))
            .map_err(|e| self.http_error(e))?;
        // モデルは複数のファイル（層）に分かれていて、層ごとに total / completed が届く。
        // 全体の進み具合は、これまでに見た層の合計で計算する
        let mut totals: HashMap<String, u64> = HashMap::new();
        let mut completed: HashMap<String, u64> = HashMap::new();
        let mut last_shown: i64 = -10;
        let gb = |b: u64| b as f64 / (1u64 << 30) as f64;
        for line in BufReader::new(resp.into_reader()).lines() {
            let line = line.map_err(|e| format!("モデルのダウンロードが途切れました: {e}"))?;
            if line.trim().is_empty() {
                continue;
            }
            let data: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if let Some(err) = data["error"].as_str() {
                return Err(format!("モデルのダウンロードに失敗しました: {err}"));
            }
            if let (Some(digest), Some(total)) = (data["digest"].as_str(), data["total"].as_u64()) {
                totals.insert(digest.into(), total);
                completed.insert(digest.into(), data["completed"].as_u64().unwrap_or(0));
                let (total, done): (u64, u64) = (totals.values().sum(), completed.values().sum());
                let pct = (done * 100).checked_div(total).unwrap_or(0) as i64;
                if pct >= last_shown + 10 {
                    last_shown = pct;
                    let msg = format!("翻訳モデル {model} をダウンロード中… {pct}%（{:.1} / {:.1} GB）", gb(done), gb(total));
                    log::info!("{msg}");
                    progress(msg);
                }
            }
            if data["status"].as_str() == Some("success") {
                break;
            }
        }
        // 層ごとの進み具合の区切りによっては 100% の行が出ないので、最後に必ず出す
        let total: u64 = totals.values().sum();
        let msg = format!("翻訳モデル {model} をダウンロード中… 100%（{:.1} / {:.1} GB）", gb(total), gb(total));
        log::info!("{msg}");
        progress(msg);
        log::info!("{model} のダウンロードが終わりました");
        Ok(true)
    }

    /// モデルを先にメモリへ読み込んでおく（最初の翻訳が遅くならないように）。
    pub fn warmup(&self) -> Result<(), String> {
        if self.llm.backend != "ollama" {
            return Ok(());
        }
        self.agent
            .post(&format!("{}/api/generate", self.base()))
            .send_json(json!({"model": self.llm.model, "keep_alive": "30m"}))
            .map_err(|e| self.http_error(e))?;
        Ok(())
    }

    fn cache_key(&self) -> String {
        format!("{}|{}", self.llm.model, self.profile_key)
    }

    /// 訳せたものから順に on_done(番号, 訳文) を呼ぶ。キャッシュにあるものは先にまとめて返す。
    pub fn translate_each(&self, texts: &[String], mut on_done: impl FnMut(usize, String)) -> Result<(), String> {
        let key = self.cache_key();
        let mut todo = Vec::new();
        for (i, text) in texts.iter().enumerate() {
            match self.cache.as_ref().and_then(|c| c.get(&key, text)) {
                Some(cached) => on_done(i, cached),
                None => todo.push(i),
            }
        }
        if todo.len() < texts.len() {
            log::info!("キャッシュから {} 件", texts.len() - todo.len());
        }
        for i in todo {
            let start = Instant::now();
            let ja = clean_output(&self.chat(&texts[i])?);
            log::info!(
                "  訳 {}/{}（{:.2} 秒）: {:?} → {:?}",
                i + 1,
                texts.len(),
                start.elapsed().as_secs_f64(),
                texts[i],
                ja
            );
            if let Some(c) = &self.cache {
                c.put(&key, &texts[i], &ja);
            }
            on_done(i, ja);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn translate(&self, texts: &[String]) -> Result<Vec<String>, String> {
        let mut out = vec![String::new(); texts.len()];
        self.translate_each(texts, |i, ja| out[i] = ja)?;
        Ok(out)
    }

    fn messages(&self, text: &str) -> (Value, f64) {
        match self.style.as_str() {
            // 用語集やゲームごとの指示は使えない（決まった1通のメッセージで訳すモデル）
            "translategemma" => (json!([{"role": "user", "content": format!("{TRANSLATEGEMMA_PROMPT}{text}")}]), self.llm.temperature),
            "lfm2" => (
                json!([{"role": "system", "content": "Translate to Japanese."}, {"role": "user", "content": text}]),
                0.5,
            ),
            _ => (
                json!([{"role": "system", "content": self.system_prompt}, {"role": "user", "content": text}]),
                self.llm.temperature,
            ),
        }
    }

    fn chat(&self, text: &str) -> Result<String, String> {
        // 返事が終わらなくなる（同じ文を繰り返す等）のを防ぐため、出力の長さに上限をつける
        let max_tokens = 128 + text.chars().count() * 4;
        let (messages, temperature) = self.messages(text);
        let base = self.base();

        if self.llm.backend == "ollama" {
            loop {
                let think_off = self.think_off.load(Ordering::Relaxed);
                let mut body = json!({
                    "model": self.llm.model,
                    "messages": messages,
                    "stream": true,
                    "options": {"temperature": temperature, "num_predict": max_tokens},
                    "keep_alive": "30m",
                });
                if think_off {
                    // Gemma 4 など考えるモードを持つモデルは、考える分だけ遅くなるので切る
                    body["think"] = json!(false);
                }
                let resp = match self.agent.post(&format!("{base}/api/chat")).send_json(body) {
                    Ok(r) => r,
                    Err(ureq::Error::Status(400, r)) if think_off => {
                        let body = r.into_string().unwrap_or_default();
                        if body.contains("think") {
                            log::info!("このモデルは think の指定に対応していないので外します");
                            self.think_off.store(false, Ordering::Relaxed);
                            continue;
                        }
                        return Err(format!("翻訳AIがエラーを返しました（400）: {body}"));
                    }
                    Err(ureq::Error::Transport(t)) if t.to_string().contains("timed out") => {
                        return Err(self.timeout_message());
                    }
                    Err(e) => return Err(self.http_error(e)),
                };
                return self.read_ndjson(resp.into_reader());
            }
        }

        let body = json!({
            "model": self.llm.model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": true,
        });
        let resp = self.agent.post(&format!("{base}/v1/chat/completions")).send_json(body).map_err(|e| self.http_error(e))?;
        let mut out = String::new();
        for line in BufReader::new(resp.into_reader()).lines() {
            let line = line.map_err(|e| if is_timeout(&e) { self.timeout_message() } else { e.to_string() })?;
            let Some(payload) = line.strip_prefix("data:") else { continue };
            let payload = payload.trim();
            if payload == "[DONE]" {
                break;
            }
            let data: Value = serde_json::from_str(payload).map_err(|e| e.to_string())?;
            if let Some(s) = data["choices"][0]["delta"]["content"].as_str() {
                out.push_str(s);
            }
        }
        Ok(out)
    }

    fn read_ndjson(&self, reader: impl Read) -> Result<String, String> {
        let mut out = String::new();
        for line in BufReader::new(reader).lines() {
            let line = line.map_err(|e| if is_timeout(&e) { self.timeout_message() } else { e.to_string() })?;
            if line.trim().is_empty() {
                continue;
            }
            let data: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if let Some(err) = data["error"].as_str() {
                return Err(format!("翻訳AIのエラー: {err}"));
            }
            if let Some(s) = data["message"]["content"].as_str() {
                out.push_str(s);
            }
            if data["done"].as_bool() == Some(true) && data["done_reason"].as_str() == Some("length") {
                log::warn!("AIの返事が長さの上限で打ち切られました");
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::Mutex;

    #[test]
    fn styles_and_names() {
        assert_eq!(detect_style("translategemma:4b"), "translategemma");
        assert_eq!(detect_style("hf.co/LiquidAI/LFM2-350M-ENJP-MT-GGUF"), "lfm2");
        assert_eq!(detect_style("gemma4:12b"), "chat");
        assert_eq!(normalize_model_name("gemma3"), "gemma3:latest");
        assert_eq!(normalize_model_name("hf.co/a/b:Q4"), "hf.co/a/b:Q4");
        assert_eq!(normalize_model_name("localhost:5000/x"), "localhost:5000/x:latest");
        assert_eq!(clean_output(" 「こんにちは」\n"), "こんにちは");
    }

    #[test]
    fn system_prompt_has_glossary() {
        let p = Profile {
            key: "k".into(),
            name: "n".into(),
            instructions: "短く".into(),
            glossary: vec![("Raid".into(), "レイド".into()), ("Active Matter".into(), String::new())],
        };
        let s = build_system_prompt(&p);
        assert!(s.contains("ゲーム固有の指示:\n短く"));
        assert!(s.contains("- Raid → レイド"));
        assert!(s.contains("- Active Matter → 英語のまま残す"));
    }

    /// 決まった返事を返すだけの HTTP サーバー。受け取ったリクエスト本文を記録する。
    fn serve(responses: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut len = 0;
                let mut line = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut buf = vec![0; len];
                reader.read_exact(&mut buf).unwrap();
                seen2.lock().unwrap().push(String::from_utf8(buf).unwrap());
                write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        (url, seen)
    }

    #[test]
    fn streams_ollama_and_retries_without_think() {
        let chunks = [r#"{"message":{"content":"「こんに"}}"#, r#"{"message":{"content":"ちは」"},"done":true}"#].join("\n");
        let (url, seen) = serve(vec![(400, r#"{"error":"model does not support think"}"#.into()), (200, chunks)]);
        let llm = LlmConfig { base_url: url, ..LlmConfig::default() };
        let t = Translator::new(&llm, &Profile::default(), Some(Arc::new(TranslationCache::memory())));
        assert_eq!(t.translate(&["Hello".into()]).unwrap(), vec!["こんにちは"]);
        let seen = seen.lock().unwrap();
        assert!(seen[0].contains("\"think\":false"));
        assert!(!seen[1].contains("think"));
        assert!(seen[1].contains("Please translate the following English text into Japanese:\\n\\n\\nHello"));
        // 2回目はキャッシュから（サーバーはもう返事しない）
        assert_eq!(t.translate(&["Hello".into()]).unwrap(), vec!["こんにちは"]);
    }

    #[test]
    fn pulls_missing_model() {
        let pull = [
            r#"{"status":"pulling manifest"}"#,
            r#"{"status":"pulling","digest":"a","total":100,"completed":50}"#,
            r#"{"status":"pulling","digest":"b","total":100,"completed":100}"#,
            r#"{"status":"success"}"#,
        ]
        .join("\n");
        let (url, _) = serve(vec![(200, r#"{"models":[{"name":"gemma3:latest"}]}"#.into()), (200, pull)]);
        let llm = LlmConfig { base_url: url, ..LlmConfig::default() };
        let t = Translator::new(&llm, &Profile::default(), None);
        let mut msgs = Vec::new();
        assert!(t.ensure_model(|m| msgs.push(m)).unwrap());
        assert!(msgs.last().unwrap().contains("100%"));
    }

    #[test]
    fn openai_sse() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"やあ\"}}]}\n\ndata: [DONE]\n";
        let (url, _) = serve(vec![(200, sse.into())]);
        let llm = LlmConfig { base_url: url, backend: "openai".into(), model: "gemma3".into(), ..LlmConfig::default() };
        let t = Translator::new(&llm, &Profile::default(), None);
        assert_eq!(t.translate(&["Hi".into()]).unwrap(), vec!["やあ"]);
    }
}
