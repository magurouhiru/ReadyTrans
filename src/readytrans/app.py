"""アプリ本体: ホットキー → 範囲選択 → キャプチャ → OCR → 翻訳 → オーバーレイ表示。"""

from __future__ import annotations

import ctypes
import logging
import sys
import time
from concurrent.futures import Future, ThreadPoolExecutor
from pathlib import Path

from PySide6.QtCore import QObject, QTimer, Signal, Slot
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
        start = time.perf_counter()
        image = self.capture.grab(region)
        log.info("キャプチャ: %s → 画像 %dx%d", region, image.shape[1], image.shape[0])
        blocks = group_lines(self.ocr.recognize(image))
        log.info("OCR 完了（%.2f 秒）: %d 件", time.perf_counter() - start, len(blocks))
        for b in blocks:
            log.info("  OCR: %r @ (%.0f, %.0f, %.0f x %.0f)", b.text, b.x, b.y, b.w, b.h)
        return blocks

    def translate_iter(self, blocks):
        start = time.perf_counter()
        log.info("翻訳開始: %d 件", len(blocks))
        yield from self.translator.translate_iter([b.text for b in blocks])
        log.info("翻訳完了（%.2f 秒）", time.perf_counter() - start)


class Bridge(QObject):
    """ワーカースレッドから UI スレッドへ結果を渡す。"""

    hotkey = Signal(str)
    done = Signal(object, object, object)  # region, blocks, translations
    status = Signal(object, str)
    notify = Signal(str)
    failed = Signal(object, str)


class App(QObject):
    # QObject にして、ワーカースレッドからのシグナルを確実に UI スレッドで受け取る

    def __init__(self, base_dir: Path):
        self.qt = QApplication.instance() or QApplication(sys.argv)
        super().__init__()
        self.base_dir = base_dir
        self.cfg = load_config(base_dir)
        self.qt.setQuitOnLastWindowClosed(False)

        self.bridge = Bridge()
        self.bridge.hotkey.connect(self.on_hotkey)
        self.bridge.done.connect(self.on_done)
        self.bridge.failed.connect(self.on_failed)
        self.bridge.status.connect(self.overlay_status)
        self.bridge.notify.connect(self.on_notify)

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
        self.preparing: str | None = "翻訳モデルを準備中…"
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
        if self.preparing:
            # モデルのダウンロード・読み込みが終わるまでは進み具合だけ見せる
            if force:
                self.overlay.show_status(region, self.preparing)
            return
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
        # 訳せた段落から順に表示する（まだのものは「…」）
        translations: list[str | None] = [None] * len(blocks)
        self.bridge.done.emit(region, blocks, list(translations))
        for i, ja in self.pipeline.translate_iter(blocks):
            translations[i] = ja
            self.bridge.done.emit(region, blocks, list(translations))
        return None

    def _warmup(self) -> None:
        translator = self.pipeline.translator
        try:
            if translator.ensure_model(self._set_preparing):
                self.bridge.notify.emit(f"翻訳モデル {self.cfg.llm.model} のダウンロードが終わりました")
        except Exception as e:
            log.warning("翻訳モデルの確認・ダウンロードに失敗しました: %s", e)
            self.bridge.notify.emit(f"翻訳モデルを用意できませんでした: {e}")
        self.preparing = f"翻訳モデル {self.cfg.llm.model} を読み込み中…"
        try:
            translator.warmup()
            log.info("翻訳モデル %s を読み込みました", self.cfg.llm.model)
        except Exception as e:
            log.warning("翻訳モデルの事前読み込みに失敗しました: %s", e)
        self.preparing = None

    def _set_preparing(self, message: str) -> None:
        if self.preparing is not None and "ダウンロード中" not in self.preparing:
            self.bridge.notify.emit(message)  # ダウンロード開始を一度だけ通知する
        self.preparing = message

    def on_notify(self, message: str) -> None:
        self.tray.showMessage("ReadyTrans", message)

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
            Item(
                self.overlay.to_logical(region, b.x, b.y, b.w, b.h),
                "…" if ja is None else (ja or "（訳せませんでした）"),
            )
            for b, ja in zip(blocks, translations)
        ]
        if all(ja is not None for ja in translations):
            log.info("表示: %d 件（画面倍率 %.2f）", len(items), self.overlay.dpr)
            for item in items:
                r = item.rect
                log.info("  表示位置 (%.0f, %.0f, %.0f x %.0f): %r", r.x(), r.y(), r.width(), r.height(), item.text)
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
