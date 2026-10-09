"""Windows 標準の OCR（Windows.Media.Ocr）で画像から英文と位置を読み取る。"""

from __future__ import annotations

import asyncio

import numpy as np
from PIL import Image

from .layout import TextBlock


class WindowsOCR:
    def __init__(self, language: str = "en-US", upscale: float = 2.0):
        from winrt.windows.globalization import Language
        from winrt.windows.media.ocr import OcrEngine

        lang = Language(language)
        if not OcrEngine.is_language_supported(lang):
            raise RuntimeError(
                f"Windows の OCR が {language} に対応していません。"
                "「設定 > 時刻と言語 > 言語」で英語を追加し、OCR 機能を入れてください。"
            )
        self._engine = OcrEngine.try_create_from_language(lang)
        self._upscale = max(1.0, upscale)

    def recognize(self, image: np.ndarray) -> list[TextBlock]:
        """image は (高さ, 幅, 4) の BGRA。戻り値の座標は元画像のピクセル単位。"""
        return asyncio.run(self._recognize(image))

    async def _recognize(self, image: np.ndarray) -> list[TextBlock]:
        from winrt.windows.graphics.imaging import (
            BitmapAlphaMode,
            BitmapPixelFormat,
            SoftwareBitmap,
        )
        from winrt.windows.media.ocr import OcrEngine
        from winrt.windows.storage.streams import DataWriter

        scale = self._upscale
        h, w = image.shape[:2]
        limit = OcrEngine.max_image_dimension
        scale = min(scale, limit / max(h, w))
        if scale != 1.0:
            pil = Image.fromarray(image, "RGBA")  # BGRA のまま並びを保って拡大する
            pil = pil.resize((int(w * scale), int(h * scale)), Image.Resampling.LANCZOS)
            image = np.asarray(pil)
        h2, w2 = image.shape[:2]

        writer = DataWriter()
        writer.write_bytes(np.ascontiguousarray(image, dtype=np.uint8).tobytes())
        bitmap = SoftwareBitmap.create_copy_from_buffer(
            writer.detach_buffer(), BitmapPixelFormat.BGRA8, w2, h2, BitmapAlphaMode.PREMULTIPLIED
        )
        result = await self._engine.recognize_async(bitmap)

        lines = []
        for line in result.lines:
            rects = [word.bounding_rect for word in line.words]
            if not rects:
                continue
            x = min(r.x for r in rects)
            y = min(r.y for r in rects)
            right = max(r.x + r.width for r in rects)
            bottom = max(r.y + r.height for r in rects)
            lines.append(
                TextBlock(line.text, x / scale, y / scale, (right - x) / scale, (bottom - y) / scale)
            )
        return lines
