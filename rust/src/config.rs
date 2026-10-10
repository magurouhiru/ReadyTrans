//! 設定ファイル（config.toml）とゲームプロファイル（profiles/*.toml）。
//! Python 版と同じ書式なので、同じファイルをそのまま使える。

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// exe に埋め込む設定のひな形。初回起動時に exe の隣へ書き出す。
pub const CONFIG_EXAMPLE: &str = include_str!("../../config.example.toml");
pub const BUNDLED_PROFILES: &[(&str, &str)] = &[
    ("default.toml", include_str!("../../profiles/default.toml")),
    ("active_matter.toml", include_str!("../../profiles/active_matter.toml")),
];

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub backend: String,
    pub base_url: String,
    pub model: String,
    pub style: String,
    pub temperature: f64,
    pub timeout_seconds: f64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            backend: "ollama".into(),
            base_url: "http://localhost:11434".into(),
            model: "translategemma:4b".into(),
            style: "auto".into(),
            temperature: 0.2,
            timeout_seconds: 120.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OcrConfig {
    pub language: String,
    pub upscale: f64,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self { language: "en-US".into(), upscale: 2.0 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub select_and_translate: String,
    pub retranslate: String,
    pub toggle_auto: String,
    pub hide: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            select_and_translate: "ctrl+shift+t".into(),
            retranslate: "ctrl+shift+r".into(),
            toggle_auto: "ctrl+shift+a".into(),
            hide: "ctrl+shift+h".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    /// "inline": 元の文字の上に重ねる / "panel": 範囲の下にまとめて表示
    pub mode: String,
    pub font_family: String,
    pub font_size: f32,
    pub background_opacity: f32,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            mode: "inline".into(),
            font_family: "Yu Gothic UI".into(),
            font_size: 16.0,
            background_opacity: 0.85,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AutoConfig {
    pub interval_ms: u64,
}

impl Default for AutoConfig {
    fn default() -> Self {
        Self { interval_ms: 1000 }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub key: String,
    pub name: String,
    pub instructions: String,
    /// (英語, 日本語)。日本語が空なら原文のまま残す。ファイルに書いた順を保つ
    pub glossary: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub llm: LlmConfig,
    pub ocr: OcrConfig,
    pub hotkeys: HotkeyConfig,
    pub overlay: OverlayConfig,
    pub auto: AutoConfig,
    pub profile: Profile,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawConfig {
    profile: Option<String>,
    llm: LlmConfig,
    ocr: OcrConfig,
    hotkeys: HotkeyConfig,
    overlay: OverlayConfig,
    auto: AutoConfig,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawProfile {
    name: Option<String>,
    instructions: String,
    glossary: toml::Table,
}

pub fn parse_config(text: &str) -> Result<(Config, String), String> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| format!("config.toml を読めません: {e}"))?;
    let key = raw.profile.unwrap_or_else(|| "default".into());
    Ok((
        Config {
            llm: raw.llm,
            ocr: raw.ocr,
            hotkeys: raw.hotkeys,
            overlay: raw.overlay,
            auto: raw.auto,
            profile: Profile::default(),
        },
        key,
    ))
}

pub fn parse_profile(key: &str, text: &str) -> Result<Profile, String> {
    let raw: RawProfile = toml::from_str(text).map_err(|e| format!("プロファイル {key} を読めません: {e}"))?;
    let glossary = raw
        .glossary
        .into_iter()
        .map(|(en, ja)| (en, ja.as_str().unwrap_or_default().to_string()))
        .collect();
    Ok(Profile {
        key: key.into(),
        name: raw.name.unwrap_or_else(|| key.into()),
        instructions: raw.instructions.trim().to_string(),
        glossary,
    })
}

/// config.toml（無ければ config.example.toml、それも無ければ埋め込みのひな形）と
/// プロファイルを読む。
pub fn load_config(base_dir: &Path) -> Result<Config, String> {
    let text = [base_dir.join("config.toml"), base_dir.join("config.example.toml")]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| CONFIG_EXAMPLE.to_string());
    let (mut cfg, key) = parse_config(&text)?;
    let path = base_dir.join("profiles").join(format!("{key}.toml"));
    let profile_text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => BUNDLED_PROFILES
            .iter()
            .find(|(name, _)| *name == format!("{key}.toml"))
            .map(|(_, t)| t.to_string())
            .ok_or_else(|| format!("プロファイルが見つかりません: {}", path.display()))?,
    };
    cfg.profile = parse_profile(&key, &profile_text)?;
    Ok(cfg)
}

/// 初回起動時に、編集できるよう config.toml と profiles/ を base_dir に書き出す。
pub fn write_defaults(base_dir: &Path) -> std::io::Result<()> {
    let config = base_dir.join("config.toml");
    if !config.exists() {
        std::fs::write(&config, CONFIG_EXAMPLE)?;
    }
    let profiles: PathBuf = base_dir.join("profiles");
    std::fs::create_dir_all(&profiles)?;
    for (name, text) in BUNDLED_PROFILES {
        let path = profiles.join(name);
        if !path.exists() {
            std::fs::write(path, text)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let (cfg, key) = parse_config(CONFIG_EXAMPLE).unwrap();
        assert_eq!(key, "default");
        assert_eq!(cfg.llm.model, "translategemma:4b");
        assert_eq!(cfg.hotkeys.select_and_translate, "ctrl+shift+t");
        assert_eq!(cfg.overlay.mode, "inline");
    }

    #[test]
    fn missing_sections_use_defaults() {
        let (cfg, key) = parse_config("profile = \"x\"\n[llm]\nmodel = \"gemma4:12b\"\n").unwrap();
        assert_eq!(key, "x");
        assert_eq!(cfg.llm.model, "gemma4:12b");
        assert_eq!(cfg.llm.timeout_seconds, 120.0);
        assert_eq!(cfg.auto.interval_ms, 1000);
    }

    #[test]
    fn profile_keeps_glossary_order() {
        let (_, text) = BUNDLED_PROFILES[1];
        let p = parse_profile("active_matter", text).unwrap();
        assert_eq!(p.name, "Active Matter");
        assert_eq!(p.glossary[0], ("Active Matter".to_string(), String::new()));
        assert_eq!(p.glossary[1], ("Raid".to_string(), "レイド".to_string()));
    }

    #[test]
    fn bundled_default_profile_parses() {
        let p = parse_profile("default", BUNDLED_PROFILES[0].1).unwrap();
        assert_eq!(p.name, "汎用");
        assert!(p.glossary.is_empty());
    }
}
