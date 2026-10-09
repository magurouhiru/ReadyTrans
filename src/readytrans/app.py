"""アプリ本体: ホットキー → 範囲選択 → キャプチャ → OCR → 翻訳 → オーバーレイ表示。"""

from __future__ import annotations

import ctypes
import logging
import sys
from concurrent.futures import Future, ThreadPoolExecutor
from pathlib import Path

from PySide6.QtCore import QObject, QTimer, Signal
from PySide6.QtGui import QAction, QIcon, QPixmap, QColor
from PySide6.QtWidgets import QApplication, QMenu, QSystemTrayIcon

from .cache import TranslationCache
from .capture import Region, ScreenCapture
from .config import Config, load_config
from .hotkey import HotkeyListener
from .layout import group_lines
from .ocr import WindowsOCR
from .overlay import Item, RegionSelector, TranslationOverlay
from .translator import Translator

log = logging.getLogger("readytrans")

WDA_EXCLUDEFROMCAPTURE = 0x11


def exclude_from_capture(widget) -> None:
    """自分のウィンドウが画面キャプチャに写り込まないようにする（Windows 10 2004 以降）。"""
    try:
        ctypes.windll.user32.SetWindowDisplayAffinity(int(widget.winId()), WDA_EXCLUDEFROMCAPTURE)
    except Exception as e:
        log.warning("キャプチャ除外を設定できませんでした: %s", e)


class Pipeline:
    """重い処理（キャプチャ・OCR・翻訳）。別スレッドで動かす。"""

    def __init__(self, cfg: Config, base_dir: Path):
        self.capture = ScreenCapture()
        self.ocr = WindowsOCR(cfg.ocr.language, cfg.ocr.upscale)
        cache = TranslationCache(base_dir / "cache.sqlite3")
        self.translator = Translator(cfg.llm, cfg.profile, cache)

    def read(self, region: Region):
        image = self.capture.grab(region)
        return group_lines(self.ocr.recognize(image))

    def translate(self, blocks):
        return self.translator.translate([b.text for b in blocks])


class Bridge(QObject):
    """ワーカースレッドから UI スレッドへ結果を渡す。"""

    hotkey = Signal(str)
    done = Signal(object, object, object)  # region, blocks, translations
    status = Signal(object, str)
    failed = Signal(object, str)


class App:
    def __init__(self, base_dir: Path):
        self.base_dir = base_dir
        self.cfg = load_config(base_dir)
        self.qt = QApplication.instance() or QApplication(sys.argv)
        self.qt.setQuitOnLastWindowClosed(False)

        self.bridge = Bridge()
        self.bridge.hotkey.connect(self.on_hotkey)
        self.bridge.done.connect(self.on_done)
        self.bridge.failed.connect(self.on_failed)
        self.bridge.status.connect(self.overlay_status)

        self.overlay = TranslationOverlay(self.cfg.overlay)
        self.selector = RegionSelector()
        self.selector.selected.connect(self.on_region_selected)
        for w in (self.overlay, self.selector):
            w.winId()  # ネイティブウィンドウを先に作る
            exclude_from_capture(w)

        self.pipeline = Pipeline(self.cfg, base_dir)
        self.executor = ThreadPoolExecutor(max_workers=1)
        self.executor.submit(self._warmup)
        self.busy = False
        self.region: Region | None = None
        self.last_text: str | None = None

        self.auto_timer = QTimer()
        self.auto_timer.setInterval(self.cfg.auto.interval_ms)
        self.auto_timer.timeout.connect(self.on_auto_tick)

        h = self.cfg.hotkeys
        self.hotkeys = HotkeyListener(
            {
                "select": h.select_and_translate,
                "retranslate": h.retranslate,
                "auto": h.toggle_auto,
                "hide": h.hide,
            },
            self.bridge.hotkey.emit,
        )
        self._build_tray()

    # --- トレイアイコン ---

    def _build_tray(self) -> None:
        pix = QPixmap(32, 32)
        pix.fill(QColor(40, 140, 220))
        self.tray = QSystemTrayIcon(QIcon(pix))
        self.tray.setToolTip(f"ReadyTrans（{self.cfg.profile.name} / {self.cfg.llm.model}）")
        menu = QMenu()
        self.auto_action = QAction("自動モード", menu, checkable=True)
        self.auto_action.toggled.connect(self.set_auto)
        menu.addAction("範囲を選んで訳す", self.start_select)
        menu.addAction(self.auto_action)
        menu.addAction("表示を消す", self.overlay.clear)
        menu.addSeparator()
        menu.addAction("終了", self.quit)
        self.tray.setContextMenu(menu)
        self.tray.show()

    # --- 操作 ---

    def on_hotkey(self, name: str) -> None:
        if name == "select":
            self.start_select()
        elif name == "retranslate" and self.region:
            self.run(self.region, force=True)
        elif name == "auto":
            self.auto_action.setChecked(not self.auto_action.isChecked())
        elif name == "hide":
            self.overlay.clear()

    def start_select(self) -> None:
        self.overlay.clear()
        self.selector.begin()

    def on_region_selected(self, region: Region | None) -> None:
        if region is None:
            return
        self.region = region
        self.last_text = None
        self.run(region, force=True)

    def set_auto(self, on: bool) -> None:
        if on and self.region is None:
            self.tray.showMessage("ReadyTrans", "先に範囲を選んでください（" + self.cfg.hotkeys.select_and_translate + "）")
            self.auto_action.setChecked(False)
            return
        if on:
            self.auto_timer.start()
        else:
            self.auto_timer.stop()

    def on_auto_tick(self) -> None:
        if self.region and not self.busy:
            self.run(self.region, force=False)

    # --- 処理 ---

    def run(self, region: Region, force: bool) -> None:
        if self.busy:
            return
        self.busy = True
        if force:
            self.overlay.show_status(region, "読み取り中…")
        future = self.executor.submit(self._work, region, force)
        future.add_done_callback(self._finish)

    def _work(self, region: Region, force: bool):
        blocks = self.pipeline.read(region)
        text = "\n".join(b.text for b in blocks)
        if not force and text == self.last_text:
            return None
        self.last_text = text
        if not blocks:
            return region, [], []
        if force:
            self.bridge.status.emit(region, "翻訳中…")
        return region, blocks, self.pipeline.translate(blocks)

    def _warmup(self) -> None:
        try:
            self.pipeline.translator.warmup()
            log.info("翻訳モデル %s を読み込みました", self.cfg.llm.model)
        except Exception as e:
            log.warning("翻訳モデルの事前読み込みに失敗しました: %s", e)

    def overlay_status(self, region: Region, text: str) -> None:
        self.overlay.show_status(region, text)

    def _finish(self, future: Future) -> None:
        self.busy = False
        try:
            result = future.result()
        except TimeoutError as e:
            log.error("%s", e)
            self.bridge.failed.emit(self.region, str(e))
            return
        except Exception as e:
            log.exception("処理に失敗しました")
            self.bridge.failed.emit(self.region, str(e))
            return
        if result is not None:
            self.bridge.done.emit(*result)

    def on_done(self, region: Region, blocks, translations) -> None:
        if not blocks:
            self.overlay.show_status(region, "文字が見つかりませんでした", error=True)
            QTimer.singleShot(2000, self.overlay.clear)
            return
        items = [
            Item(self.overlay.to_logical(region, b.x, b.y, b.w, b.h), ja)
            for b, ja in zip(blocks, translations)
        ]
        self.overlay.show_translations(region, items)

    def on_failed(self, region: Region | None, message: str) -> None:
        if region:
            self.overlay.show_status(region, f"エラー: {message}", error=True)
        self.tray.showMessage("ReadyTrans", f"エラー: {message}", QSystemTrayIcon.MessageIcon.Warning)

    def quit(self) -> None:
        self.hotkeys.stop()
        self.executor.shutdown(wait=False, cancel_futures=True)
        self.qt.quit()

    def exec(self) -> int:
        self.hotkeys.start()
        if self.hotkeys.failed:
            self.tray.showMessage(
                "ReadyTrans",
                "次のホットキーは他のアプリが使っているため登録できませんでした: " + ", ".join(self.hotkeys.failed),
                QSystemTrayIcon.MessageIcon.Warning,
            )
        else:
            self.tray.showMessage(
                "ReadyTrans",
                f"起動しました。{self.cfg.hotkeys.select_and_translate} で範囲を選んで訳します。",
            )
        return self.qt.exec()


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    if sys.platform != "win32":
        sys.exit("ReadyTrans は Windows 専用です。")
    base_dir = Path(sys.executable).parent if getattr(sys, "frozen", False) else Path.cwd()
    sys.exit(App(base_dir).exec())


if __name__ == "__main__":
    main()
