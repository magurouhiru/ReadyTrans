"""画面の一部を撮る。Windows Graphics Capture（OBS と同じ仕組み）を優先し、だめなら mss を使う。

座標はすべてメインモニター左上を原点とした物理ピクセル。戻り値は (高さ, 幅, 4) の BGRA。
"""

from __future__ import annotations

import logging
import threading
from dataclasses import dataclass

import numpy as np

log = logging.getLogger(__name__)


@dataclass(frozen=True)
class Region:
    x: int
    y: int
    w: int
    h: int


def _grab_wgc(region: Region, timeout: float = 2.0) -> np.ndarray:
    from windows_capture import WindowsCapture

    done = threading.Event()
    result: dict[str, np.ndarray] = {}

    try:
        capture = WindowsCapture(cursor_capture=False, draw_border=False, monitor_index=None)
    except Exception:
        # Windows 10 では枠線を消す設定ができないので、既定のまま撮る
        capture = WindowsCapture(cursor_capture=False, monitor_index=None)

    @capture.event
    def on_frame_arrived(frame, capture_control):
        buf = frame.frame_buffer
        crop = buf[region.y : region.y + region.h, region.x : region.x + region.w, :]
        result["image"] = np.array(crop, copy=True)  # 元のバッファは止めると消えるので複製する
        capture_control.stop()
        done.set()

    @capture.event
    def on_closed():
        done.set()

    control = capture.start_free_threaded()
    if not done.wait(timeout):
        control.stop()
        raise TimeoutError("画面キャプチャのフレームが届きませんでした")
    if "image" not in result:
        raise RuntimeError("画面キャプチャが途中で終了しました")
    return result["image"]


def _grab_mss(region: Region) -> np.ndarray:
    import mss

    with mss.mss() as sct:
        primary = sct.monitors[1]
        shot = sct.grab(
            {
                "left": primary["left"] + region.x,
                "top": primary["top"] + region.y,
                "width": region.w,
                "height": region.h,
            }
        )
        return np.array(shot)


class ScreenCapture:
    def __init__(self) -> None:
        self._use_wgc = True

    def grab(self, region: Region) -> np.ndarray:
        if self._use_wgc:
            try:
                return _grab_wgc(region)
            except Exception as e:  # ライブラリが無い、古い Windows など
                log.warning("Windows Graphics Capture が使えないので mss に切り替えます: %s", e)
                self._use_wgc = False
        return _grab_mss(region)
