import json
from pathlib import Path

import httpx

from readytrans.cache import TranslationCache
from readytrans.config import LLMConfig, Profile, load_config
from readytrans.hotkey import MOD_CONTROL, MOD_SHIFT, parse_hotkey
from readytrans.layout import TextBlock, group_lines
from readytrans.translator import Translator, build_system_prompt, parse_translations

ROOT = Path(__file__).resolve().parent.parent


def test_group_lines_merges_paragraph_and_keeps_columns_apart():
    lines = [
        TextBlock("Press E to", 10, 10, 100, 20),
        TextBlock("open the stash.", 10, 34, 120, 20),
        TextBlock("Inventory", 400, 10, 90, 20),
        TextBlock("Far below", 10, 200, 80, 20),
    ]
    blocks = group_lines(lines)
    texts = sorted(b.text for b in blocks)
    assert texts == ["Far below", "Inventory", "Press E to open the stash."]
    para = next(b for b in blocks if b.text.startswith("Press"))
    assert (para.x, para.y, para.w, para.h) == (10, 10, 120, 44)


def test_parse_translations():
    assert parse_translations('{"translations": ["a", "b"]}', 2) == ["a", "b"]
    assert parse_translations('```json\n["a"]\n```', 1) == ["a"]
    assert parse_translations('{"translations": ["a"]}', 2) is None
    assert parse_translations("no json", 1) is None


def test_glossary_in_prompt():
    prompt = build_system_prompt(Profile(glossary={"Raid": "レイド", "Active Matter": ""}))
    assert "Raid → レイド" in prompt
    assert "Active Matter → 英語のまま残す" in prompt


def _translator(handler, tmp_path, backend="ollama"):
    t = Translator(LLMConfig(backend=backend), Profile(), TranslationCache(tmp_path / "c.db"))
    t.client = httpx.Client(transport=httpx.MockTransport(handler))
    return t


def test_translate_uses_cache_and_ollama(tmp_path):
    calls = []

    def handler(request):
        body = json.loads(request.content)
        texts = json.loads(body["messages"][-1]["content"])
        calls.append(texts)
        out = [f"訳:{x}" for x in texts]
        return httpx.Response(200, json={"message": {"content": json.dumps({"translations": out})}})

    t = _translator(handler, tmp_path)
    assert t.translate(["Hello", "World"]) == ["訳:Hello", "訳:World"]
    assert t.translate(["World", "New"]) == ["訳:World", "訳:New"]
    assert calls == [["Hello", "World"], ["New"]]


def test_translate_falls_back_to_one_by_one(tmp_path):
    def handler(request):
        texts = json.loads(json.loads(request.content)["messages"][-1]["content"])
        out = ["まとめ"] if len(texts) > 1 else [f"訳:{texts[0]}"]
        return httpx.Response(200, json={"choices": [{"message": {"content": json.dumps(out)}}]})

    t = _translator(handler, tmp_path, backend="openai")
    assert t.translate(["A", "B"]) == ["訳:A", "訳:B"]


def test_parse_hotkey():
    assert parse_hotkey("ctrl+shift+t") == (MOD_CONTROL | MOD_SHIFT, ord("T"))
    assert parse_hotkey("Ctrl + F9")[1] == 0x78


def test_example_config_and_profiles_load():
    cfg = load_config(ROOT)
    assert cfg.llm.model.startswith("gemma")
    for path in (ROOT / "profiles").glob("*.toml"):
        load_config  # noqa
        from readytrans.config import load_profile

        assert load_profile(ROOT / "profiles", path.stem).name
