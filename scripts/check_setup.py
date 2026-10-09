"""セットアップ確認: Ollama と Gemma が動くか、Windows OCR が使えるかを調べる。

使い方: uv run python scripts/check_setup.py
"""

import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from readytrans.config import load_config  # noqa: E402
from readytrans.translator import Translator  # noqa: E402

cfg = load_config(Path.cwd())
print(f"プロファイル: {cfg.profile.name} / モデル: {cfg.llm.model} ({cfg.llm.backend} {cfg.llm.base_url})")

ok = True
try:
    t = Translator(cfg.llm, cfg.profile)
    samples = ["Press [E] to open the stash.", "Extraction point is closing in 30 seconds!"]
    start = time.perf_counter()
    for en, ja in zip(samples, t.translate(samples)):
        print(f"  {en}\n  → {ja}")
    print(f"[OK] 翻訳できました（{time.perf_counter() - start:.1f} 秒）")
except Exception as e:
    ok = False
    print(f"[NG] 翻訳AIにつながりません: {e}")
    print(f"     Ollama を起動し、`ollama pull {cfg.llm.model}` を済ませてください。")

if sys.platform == "win32":
    try:
        from readytrans.ocr import WindowsOCR

        WindowsOCR(cfg.ocr.language)
        print("[OK] Windows OCR が使えます")
    except Exception as e:
        ok = False
        print(f"[NG] Windows OCR: {e}")

sys.exit(0 if ok else 1)
