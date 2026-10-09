"""設定ファイル（config.toml）とゲームプロファイル（profiles/*.toml）の読み込み。"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass, field
from pathlib import Path


@dataclass
class LLMConfig:
    backend: str = "ollama"
    base_url: str = "http://localhost:11434"
    model: str = "gemma3:12b"
    temperature: float = 0.2
    timeout_seconds: float = 60.0


@dataclass
class OCRConfig:
    language: str = "en-US"
    upscale: float = 2.0


@dataclass
class HotkeyConfig:
    select_and_translate: str = "ctrl+shift+t"
    retranslate: str = "ctrl+shift+r"
    toggle_auto: str = "ctrl+shift+a"
    hide: str = "ctrl+shift+h"


@dataclass
class OverlayConfig:
    mode: str = "inline"
    font_family: str = "Yu Gothic UI"
    font_size: int = 16
    background_opacity: float = 0.85


@dataclass
class AutoConfig:
    interval_ms: int = 1000


@dataclass
class Profile:
    key: str = "default"
    name: str = "汎用"
    instructions: str = ""
    glossary: dict[str, str] = field(default_factory=dict)


@dataclass
class Config:
    profile: Profile = field(default_factory=Profile)
    llm: LLMConfig = field(default_factory=LLMConfig)
    ocr: OCRConfig = field(default_factory=OCRConfig)
    hotkeys: HotkeyConfig = field(default_factory=HotkeyConfig)
    overlay: OverlayConfig = field(default_factory=OverlayConfig)
    auto: AutoConfig = field(default_factory=AutoConfig)


def _fill(cls, data: dict):
    known = {k: v for k, v in data.items() if k in cls.__dataclass_fields__}
    return cls(**known)


def load_profile(profiles_dir: Path, key: str) -> Profile:
    path = profiles_dir / f"{key}.toml"
    if not path.exists():
        return Profile(key=key)
    with path.open("rb") as f:
        data = tomllib.load(f)
    return Profile(
        key=key,
        name=data.get("name", key),
        instructions=data.get("instructions", "").strip(),
        glossary={str(k): str(v) for k, v in data.get("glossary", {}).items()},
    )


def load_config(base_dir: Path) -> Config:
    """base_dir の config.toml を読む。無ければ config.example.toml、それも無ければ既定値。"""
    data: dict = {}
    for name in ("config.toml", "config.example.toml"):
        path = base_dir / name
        if path.exists():
            with path.open("rb") as f:
                data = tomllib.load(f)
            break

    return Config(
        profile=load_profile(base_dir / "profiles", data.get("profile", "default")),
        llm=_fill(LLMConfig, data.get("llm", {})),
        ocr=_fill(OCRConfig, data.get("ocr", {})),
        hotkeys=_fill(HotkeyConfig, data.get("hotkeys", {})),
        overlay=_fill(OverlayConfig, data.get("overlay", {})),
        auto=_fill(AutoConfig, data.get("auto", {})),
    )
