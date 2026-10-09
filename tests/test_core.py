import json
from pathlib import Path

import httpx

from readytrans.cache import TranslationCache
from readytrans.config import LLMConfig, Profile, load_config
from readytrans.hotkey import MOD_CONTROL, MOD_SHIFT, parse_hotkey
from readytrans.layout import TextBlock, group_lines
from readytrans.translator import Translator, build_system_prompt, detect_style

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


def test_glossary_in_prompt():
    prompt = build_system_prompt(Profile(glossary={"Raid": "レイド", "Active Matter": ""}))
    assert "Raid → レイド" in prompt
    assert "Active Matter → 英語のまま残す" in prompt


def _translator(handler, tmp_path, backend="ollama", model="gemma4:12b"):
    t = Translator(LLMConfig(backend=backend, model=model), Profile(), TranslationCache(tmp_path / "c.db"))
    t.client = httpx.Client(transport=httpx.MockTransport(handler))
    return t


def _ollama_stream(text):
    lines = [{"message": {"content": ch}, "done": False} for ch in text]
    lines.append({"message": {"content": ""}, "done": True, "done_reason": "stop"})
    return httpx.Response(200, content="\n".join(json.dumps(l, ensure_ascii=False) for l in lines))


def test_translate_streams_each_paragraph_and_caches(tmp_path):
    asked = []

    def handler(request):
        body = json.loads(request.content)
        assert body["stream"] is True and body["think"] is False
        user = body["messages"][-1]["content"]
        asked.append(user)
        return _ollama_stream(f"「訳:{user}」")

    t = _translator(handler, tmp_path)
    assert t.translate(["Hello", "World"]) == ["訳:Hello", "訳:World"]
    assert list(t.translate_iter(["World", "New"])) == [(0, "訳:World"), (1, "訳:New")]
    assert asked == ["Hello", "World", "New"]


def test_empty_translation_is_not_cached(tmp_path):
    replies = iter(["", "訳"])

    def handler(request):
        return _ollama_stream(next(replies))

    t = _translator(handler, tmp_path)
    assert t.translate(["A"]) == [""]
    assert t.translate(["A"]) == ["訳"]


def test_openai_streaming(tmp_path):
    def handler(request):
        chunks = [{"choices": [{"delta": {"content": c}}]} for c in ["訳", ":A"]]
        sse = "".join(f"data: {json.dumps(c, ensure_ascii=False)}\n\n" for c in chunks) + "data: [DONE]\n\n"
        return httpx.Response(200, content=sse)

    t = _translator(handler, tmp_path, backend="openai")
    assert t.translate(["A"]) == ["訳:A"]


def test_think_is_dropped_if_unsupported(tmp_path):
    bodies = []

    def handler(request):
        body = json.loads(request.content)
        bodies.append(body)
        if "think" in body:
            return httpx.Response(400, json={"error": '"x" does not support thinking'})
        return _ollama_stream("訳")

    t = _translator(handler, tmp_path)
    assert t.translate(["A", "B"]) == ["訳", "訳"]
    assert "think" in bodies[0] and all("think" not in b for b in bodies[1:])


def test_translategemma_uses_its_own_prompt(tmp_path):
    seen = []

    def handler(request):
        seen.append(json.loads(request.content)["messages"])
        return _ollama_stream("こんにちは")

    t = _translator(handler, tmp_path, model="translategemma:4b")
    assert t.translate(["Hello"]) == ["こんにちは"]
    (msg,) = seen[0]
    assert msg["role"] == "user" and msg["content"].endswith("into Japanese:\n\n\nHello")
    assert detect_style("hf.co/LiquidAI/LFM2-350M-ENJP-MT-GGUF") == "lfm2"


def test_parse_hotkey():
    assert parse_hotkey("ctrl+shift+t") == (MOD_CONTROL | MOD_SHIFT, ord("T"))
    assert parse_hotkey("Ctrl + F9")[1] == 0x78


def test_example_config_and_profiles_load():
    cfg = load_config(ROOT)
    assert cfg.llm.model.startswith("translategemma")
    for path in (ROOT / "profiles").glob("*.toml"):
        load_config  # noqa
        from readytrans.config import load_profile

        assert load_profile(ROOT / "profiles", path.stem).name

