"""ローカル LLM（Ollama または OpenAI 互換 API）で英語を日本語に訳す。

段落ごとに1回ずつ問い合わせ、返事は少しずつ受け取る（ストリーミング）。
長い文でも、AI が書き続けている限り時間切れにならない。
"""

from __future__ import annotations

import json
import logging
import time
from collections.abc import Iterator

import httpx

from .cache import TranslationCache
from .config import LLMConfig, Profile

log = logging.getLogger(__name__)

SYSTEM_PROMPT = """\
あなたはゲームの翻訳者です。ゲーム画面から OCR で読み取った英語を自然な日本語に訳します。
- 訳文だけを返してください。説明や引用符は付けないでください。
- OCR の読み間違いらしい文字は文脈から推測して直してから訳してください。
- 数字、記号、キー表記（[E]、LMB など）はそのまま残してください。"""


def build_system_prompt(profile: Profile) -> str:
    parts = [SYSTEM_PROMPT]
    if profile.instructions:
        parts.append("\nゲーム固有の指示:\n" + profile.instructions)
    if profile.glossary:
        lines = []
        for en, ja in profile.glossary.items():
            lines.append(f"- {en} → {ja}" if ja else f"- {en} → 英語のまま残す")
        parts.append("\n用語集（必ず従う）:\n" + "\n".join(lines))
    return "\n".join(parts)


TRANSLATEGEMMA_PROMPT = """\
You are a professional English (en) to Japanese (ja) translator. Your goal is to accurately convey \
the meaning and nuances of the original English text while adhering to Japanese grammar, vocabulary, \
and cultural sensitivities.
Produce only the Japanese translation, without any additional explanations or commentary. \
Please translate the following English text into Japanese:


"""


def detect_style(model: str) -> str:
    """モデル名から問い合わせ方を決める。翻訳専用モデルは決まった書式でないとうまく訳せない。"""
    name = model.lower()
    if "translategemma" in name:
        return "translategemma"
    if "lfm2" in name and "enjp" in name:
        return "lfm2"
    return "chat"


def clean_output(text: str) -> str:
    return text.strip().strip('"「」').strip()


class Translator:
    def __init__(self, llm: LLMConfig, profile: Profile, cache: TranslationCache | None = None):
        self.llm = llm
        self.profile = profile
        self.cache = cache
        self.style = llm.style if llm.style != "auto" else detect_style(llm.model)
        self.system_prompt = build_system_prompt(profile)
        self._think_off = True
        log.info("翻訳モデル %s（問い合わせ方: %s）", llm.model, self.style)
        # timeout は「次のデータが届くまで」の待ち時間。ストリーミングなので書き続けている限り切れない
        self.client = httpx.Client(timeout=httpx.Timeout(llm.timeout_seconds, connect=5.0))

    def warmup(self) -> None:
        """モデルを先にメモリへ読み込んでおく（最初の翻訳が遅くならないように）。"""
        if self.llm.backend != "ollama":
            return
        base = self.llm.base_url.rstrip("/")
        self.client.post(f"{base}/api/generate", json={"model": self.llm.model, "keep_alive": "30m"})

    def translate(self, texts: list[str]) -> list[str]:
        results = [""] * len(texts)
        for i, ja in self.translate_iter(texts):
            results[i] = ja
        return results

    def translate_iter(self, texts: list[str]) -> Iterator[tuple[int, str]]:
        """訳せたものから順に (番号, 訳文) を返す。キャッシュにあるものは先にまとめて返す。"""
        todo = []
        for i, text in enumerate(texts):
            cached = self.cache.get(self._cache_key(), text) if self.cache else None
            if cached:
                yield i, cached
            else:
                todo.append(i)
        if len(todo) < len(texts):
            log.info("キャッシュから %d 件", len(texts) - len(todo))

        for i in todo:
            start = time.perf_counter()
            ja = clean_output(self._chat(texts[i]))
            log.info("  訳 %d/%d（%.2f 秒）: %r → %r", i + 1, len(texts), time.perf_counter() - start, texts[i], ja)
            if ja and self.cache:
                self.cache.put(self._cache_key(), texts[i], ja)
            yield i, ja

    def _cache_key(self) -> str:
        return f"{self.llm.model}|{self.profile.key}"

    def _chat(self, text: str) -> str:
        try:
            return "".join(self._stream(text))
        except httpx.TimeoutException as e:
            raise TimeoutError(
                f"翻訳AIから {self.llm.timeout_seconds:.0f} 秒以上返事がありませんでした。"
                "ゲームと AI で VRAM が足りていない可能性があります（`ollama ps` で確認できます）。"
            ) from e

    def _stream(self, text: str) -> Iterator[str]:
        # 返事が終わらなくなる（同じ文を繰り返す等）のを防ぐため、出力の長さに上限をつける
        max_tokens = 128 + len(text) * 4
        temperature = self.llm.temperature
        if self.style == "translategemma":
            # 用語集やゲームごとの指示は使えない（決まった1通のメッセージで訳すモデル）
            messages = [{"role": "user", "content": TRANSLATEGEMMA_PROMPT + text}]
        elif self.style == "lfm2":
            messages = [
                {"role": "system", "content": "Translate to Japanese."},
                {"role": "user", "content": text},
            ]
            temperature = 0.5
        else:
            messages = [
                {"role": "system", "content": self.system_prompt},
                {"role": "user", "content": text},
            ]
        base = self.llm.base_url.rstrip("/")

        if self.llm.backend == "ollama":
            body = {
                "model": self.llm.model,
                "messages": messages,
                "stream": True,
                "options": {"temperature": temperature, "num_predict": max_tokens},
                "keep_alive": "30m",
            }
            if self._think_off:
                # Gemma 4 など考えるモードを持つモデルは、考える分だけ遅くなるので切る
                body["think"] = False
            with self.client.stream("POST", f"{base}/api/chat", json=body) as resp:
                if resp.status_code == 400 and self._think_off:
                    resp.read()
                    if "think" in resp.text:
                        log.info("このモデルは think の指定に対応していないので外します")
                        self._think_off = False
                        yield from self._stream(text)
                        return
                resp.raise_for_status()
                for line in resp.iter_lines():
                    if not line:
                        continue
                    data = json.loads(line)
                    if "error" in data:
                        raise RuntimeError(f"翻訳AIのエラー: {data['error']}")
                    yield data.get("message", {}).get("content", "")
                    if data.get("done") and data.get("done_reason") == "length":
                        log.warning("AIの返事が長さの上限で打ち切られました")
            return

        body = {
            "model": self.llm.model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": True,
        }
        with self.client.stream("POST", f"{base}/v1/chat/completions", json=body) as resp:
            resp.raise_for_status()
            for line in resp.iter_lines():
                if not line.startswith("data:"):
                    continue
                payload = line[5:].strip()
                if payload == "[DONE]":
                    break
                choices = json.loads(payload).get("choices") or [{}]
                yield choices[0].get("delta", {}).get("content") or ""
