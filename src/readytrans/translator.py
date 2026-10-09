"""ローカル LLM（Ollama または OpenAI 互換 API）で英語を日本語に訳す。"""

from __future__ import annotations

import json
import logging
import re

import httpx

from .cache import TranslationCache
from .config import LLMConfig, Profile

log = logging.getLogger(__name__)

SYSTEM_PROMPT = """\
あなたはゲームの翻訳者です。ゲーム画面から OCR で読み取った英語を自然な日本語に訳します。
- 入力は JSON の文字列配列です。同じ順番・同じ個数の日本語の文字列配列を {"translations": [...]} の形で返してください。
- OCR の読み間違いらしい文字は文脈から推測して直してから訳してください。
- 数字、記号、キー表記（[E]、LMB など）はそのまま残してください。
- 訳文以外の説明は書かないでください。"""


SINGLE_PROMPT = """\
あなたはゲームの翻訳者です。ゲーム画面から OCR で読み取った英語を自然な日本語に訳します。
- 訳文だけを1行で返してください。説明や引用符は付けないでください。
- OCR の読み間違いらしい文字は文脈から推測して直してから訳してください。
- 数字、記号、キー表記（[E]、LMB など）はそのまま残してください。"""


def build_system_prompt(profile: Profile, base: str = SYSTEM_PROMPT) -> str:
    parts = [base]
    if profile.instructions:
        parts.append("\nゲーム固有の指示:\n" + profile.instructions)
    if profile.glossary:
        lines = []
        for en, ja in profile.glossary.items():
            lines.append(f"- {en} → {ja}" if ja else f"- {en} → 英語のまま残す")
        parts.append("\n用語集（必ず従う）:\n" + "\n".join(lines))
    return "\n".join(parts)


def parse_translations(content: str, expected: int) -> list[str] | None:
    """モデルの返答から訳文の配列を取り出す。個数が合わなければ None。"""
    match = re.search(r"\{.*\}|\[.*\]", content, re.DOTALL)
    if not match:
        return None
    try:
        data = json.loads(match.group(0))
    except json.JSONDecodeError:
        return None
    if isinstance(data, dict):
        data = data.get("translations")
    if not isinstance(data, list) or len(data) != expected:
        return None
    return [str(x) for x in data]


class Translator:
    def __init__(self, llm: LLMConfig, profile: Profile, cache: TranslationCache | None = None):
        self.llm = llm
        self.profile = profile
        self.cache = cache
        self.system_prompt = build_system_prompt(profile)
        self.single_prompt = build_system_prompt(profile, SINGLE_PROMPT)
        self._think_off = True
        # 接続はすぐ失敗させ、返事（生成）だけ長めに待つ
        self.client = httpx.Client(timeout=httpx.Timeout(llm.timeout_seconds, connect=5.0))

    def warmup(self) -> None:
        """モデルを先にメモリへ読み込んでおく（最初の翻訳が遅くならないように）。"""
        if self.llm.backend != "ollama":
            return
        base = self.llm.base_url.rstrip("/")
        self.client.post(f"{base}/api/generate", json={"model": self.llm.model, "keep_alive": "30m"})

    def translate(self, texts: list[str]) -> list[str]:
        results: list[str | None] = [None] * len(texts)
        todo: list[int] = []
        for i, text in enumerate(texts):
            cached = self.cache.get(self._cache_key(), text) if self.cache else None
            if cached:
                results[i] = cached
            else:
                todo.append(i)
        if len(todo) < len(texts):
            log.info("キャッシュから %d 件", len(texts) - len(todo))

        if todo:
            batch = [texts[i] for i in todo]
            translated = self._translate_batch(batch)
            if translated is None:
                # まとめて訳すと個数がずれたり空になったりすることがあるので、1件ずつ訳し直す
                translated = [self._translate_one(t) for t in batch]
            for i, ja in zip(todo, translated):
                results[i] = ja
                if self.cache and ja:
                    self.cache.put(self._cache_key(), texts[i], ja)

        return [r or "" for r in results]

    def _cache_key(self) -> str:
        return f"{self.llm.model}|{self.profile.key}"

    def _translate_batch(self, texts: list[str]) -> list[str] | None:
        user = json.dumps(texts, ensure_ascii=False)
        content = self._chat(self.system_prompt, user, json_schema=True)
        log.info("AIの返事: %r", content[:500])
        parsed = parse_translations(content, len(texts))
        if parsed is None:
            log.warning("AIの返事の形式が違うので、1件ずつ訳し直します")
            return None
        if any(src.strip() and not ja.strip() for src, ja in zip(texts, parsed)):
            log.warning("空の訳が返ってきたので、1件ずつ訳し直します")
            return None
        return parsed

    def _translate_one(self, text: str) -> str:
        content = self._chat(self.single_prompt, text, json_schema=False)
        log.info("AIの返事（1件）: %r", content[:500])
        return content.strip().strip('"「」').strip()

    def _chat(self, system: str, user: str, json_schema: bool) -> str:
        try:
            return self._chat_raw(system, user, json_schema)
        except httpx.TimeoutException as e:
            raise TimeoutError(
                f"翻訳AIが {self.llm.timeout_seconds:.0f} 秒以内に返事をしませんでした。"
                "ゲームと AI で VRAM が足りていない可能性があります（`ollama ps` で確認できます）。"
            ) from e

    def _chat_raw(self, system: str, user: str, json_schema: bool) -> str:
        # 返事が終わらなくなる（空白を出し続ける等）のを防ぐため、出力の長さに上限をつける
        max_tokens = 128 + len(user) * 4
        messages = [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ]
        base = self.llm.base_url.rstrip("/")
        if self.llm.backend == "ollama":
            body = {
                "model": self.llm.model,
                "messages": messages,
                "stream": False,
                "options": {"temperature": self.llm.temperature, "num_predict": max_tokens},
                "keep_alive": "30m",
            }
            if json_schema:
                body["format"] = {
                    "type": "object",
                    "properties": {"translations": {"type": "array", "items": {"type": "string"}}},
                    "required": ["translations"],
                }
            if self._think_off:
                # Gemma 4 など考えるモードを持つモデルは、考える分だけ遅くなり、
                # 出力の上限を使い切って訳が空になることがあるので切る
                body["think"] = False
            resp = self.client.post(f"{base}/api/chat", json=body)
            if resp.status_code == 400 and "think" in resp.text and self._think_off:
                log.info("このモデルは think の指定に対応していないので外します")
                self._think_off = False
                body.pop("think")
                resp = self.client.post(f"{base}/api/chat", json=body)
            resp.raise_for_status()
            data = resp.json()
            if data.get("done_reason") == "length":
                log.warning("AIの返事が長さの上限で打ち切られました")
            if data.get("message", {}).get("thinking"):
                log.info("AIが考えるモードで返事をしました（%d 文字）", len(data["message"]["thinking"]))
            return data["message"]["content"]

        resp = self.client.post(
            f"{base}/v1/chat/completions",
            json={
                "model": self.llm.model,
                "messages": messages,
                "temperature": self.llm.temperature,
                "max_tokens": max_tokens,
            },
        )
        resp.raise_for_status()
        return resp.json()["choices"][0]["message"]["content"]
