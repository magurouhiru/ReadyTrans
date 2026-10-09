"""exe が正しくまとめられているかの自己診断（ReadyTrans.exe --self-test）。

文字を描いた画像を Windows OCR で読み、画面キャプチャと Qt が読み込めるかを確かめる。
翻訳AI（Ollama）は確かめない。結果は selftest.log に書き、失敗があれば終了コード 1 を返す。
"""

from __future__ import annotations

import logging
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

log = logging.getLogger("readytrans.selftest")

SAMPLE = "Press E to open the stash"


def _sample_image() -> np.ndarray:
    try:
        font = ImageFont.truetype("arial.ttf", 32)
    except OSError:
        font = ImageFont.load_default(size=32)
    img = Image.new("RGB", (640, 80), "white")
    ImageDraw.Draw(img).text((16, 20), SAMPLE, fill="black", font=font)
    rgb = np.asarray(img)
    bgra = np.dstack([rgb[..., 2], rgb[..., 1], rgb[..., 0], np.full(rgb.shape[:2], 255, np.uint8)])
    return np.ascontiguousarray(bgra)


def run(base_dir: Path) -> int:
    handler = logging.FileHandler(base_dir / "selftest.log", "w", encoding="utf-8")
    logging.getLogger().addHandler(handler)
    failures = 0

    def check(name: str, fn, required: bool = True) -> None:
        nonlocal failures
        try:
            log.info("[OK] %s: %s", name, fn())
        except Exception as e:
            log.exception("[%s] %s", "NG" if required else "警告", name)
            failures += required

    def ocr():
        from .ocr import WindowsOCR

        text = " ".join(b.text for b in WindowsOCR("en-US").recognize(_sample_image()))
        if "stash" not in text.lower():
            raise RuntimeError(f"読み取り結果が違います: {text!r}")
        return text

    def capture():
        from .capture import Region, ScreenCapture

        image = ScreenCapture().grab(Region(0, 0, 64, 64))
        return f"{image.shape}"

    def qt():
        from PySide6.QtWidgets import QApplication

        from .overlay import RegionSelector, TranslationOverlay  # noqa: F401

        QApplication.instance() or QApplication([])
        return "PySide6 を読み込めました"

    def config():
        from .config import load_config

        cfg = load_config(base_dir)
        return f"{cfg.llm.model} / {cfg.profile.name}"

    check("設定", config)
    check("Windows OCR", ocr)
    check("Qt", qt)
    check("画面キャプチャ", capture, required=False)  # 画面の無い環境では撮れないことがある
    log.info("自己診断: %s", "失敗あり" if failures else "すべて成功")
    handler.close()
    return 1 if failures else 0
