"""Windows の RegisterHotKey でグローバルホットキーを受け取る。

キーボードフックを使わない OS 標準の仕組みなので、ゲームの入力に割り込まない。
"""

from __future__ import annotations

import ctypes
import threading
from ctypes import wintypes
from typing import Callable

MOD_ALT = 0x0001
MOD_CONTROL = 0x0002
MOD_SHIFT = 0x0004
MOD_WIN = 0x0008
MOD_NOREPEAT = 0x4000
WM_HOTKEY = 0x0312
WM_QUIT = 0x0012

_MODIFIERS = {"ctrl": MOD_CONTROL, "control": MOD_CONTROL, "shift": MOD_SHIFT, "alt": MOD_ALT, "win": MOD_WIN}
_NAMED_KEYS = {
    "space": 0x20, "enter": 0x0D, "tab": 0x09, "esc": 0x1B, "escape": 0x1B,
    "insert": 0x2D, "delete": 0x2E, "home": 0x24, "end": 0x23, "pageup": 0x21, "pagedown": 0x22,
    **{f"f{i}": 0x6F + i for i in range(1, 25)},
}


def parse_hotkey(text: str) -> tuple[int, int]:
    """"ctrl+shift+t" → (修飾キー, 仮想キーコード)"""
    mods = 0
    vk = None
    for part in text.lower().replace(" ", "").split("+"):
        if part in _MODIFIERS:
            mods |= _MODIFIERS[part]
        elif part in _NAMED_KEYS:
            vk = _NAMED_KEYS[part]
        elif len(part) == 1 and part.isalnum():
            vk = ord(part.upper())
        else:
            raise ValueError(f"ホットキーのキー名がわかりません: {part!r}（{text}）")
    if vk is None:
        raise ValueError(f"ホットキーに通常のキーがありません: {text}")
    return mods, vk


class HotkeyListener:
    """別スレッドでメッセージループを回し、ホットキーが押されたら callback(name) を呼ぶ。"""

    def __init__(self, bindings: dict[str, str], callback: Callable[[str], None]):
        self._bindings = bindings
        self._callback = callback
        self._thread: threading.Thread | None = None
        self._thread_id: int | None = None
        self.failed: list[str] = []
        self._ready = threading.Event()

    def start(self) -> None:
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()
        self._ready.wait(2)

    def stop(self) -> None:
        if self._thread_id is not None:
            ctypes.windll.user32.PostThreadMessageW(self._thread_id, WM_QUIT, 0, 0)

    def _run(self) -> None:
        user32 = ctypes.windll.user32
        self._thread_id = ctypes.windll.kernel32.GetCurrentThreadId()
        ids: dict[int, str] = {}
        for i, (name, text) in enumerate(self._bindings.items(), start=1):
            mods, vk = parse_hotkey(text)
            if user32.RegisterHotKey(None, i, mods | MOD_NOREPEAT, vk):
                ids[i] = name
            else:
                self.failed.append(text)  # 他のアプリが同じキーを使っている
        self._ready.set()

        msg = wintypes.MSG()
        while user32.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
            if msg.message == WM_HOTKEY and msg.wParam in ids:
                self._callback(ids[msg.wParam])

        for i in ids:
            user32.UnregisterHotKey(None, i)
