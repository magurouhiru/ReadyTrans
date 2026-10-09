"""透明・最前面・クリック透過のウィンドウに訳文を描く。ゲームには一切触れない。"""

from __future__ import annotations

from dataclasses import dataclass

from PySide6.QtCore import QPoint, QRect, QRectF, Qt, Signal
from PySide6.QtGui import QColor, QFont, QFontMetrics, QGuiApplication, QMouseEvent, QPainter, QPen
from PySide6.QtWidgets import QWidget

from .capture import Region
from .config import OverlayConfig


@dataclass
class Item:
    rect: QRectF  # 論理ピクセル（画面座標）
    text: str


def _overlay_flags() -> Qt.WindowType:
    return (
        Qt.WindowType.FramelessWindowHint
        | Qt.WindowType.WindowStaysOnTopHint
        | Qt.WindowType.Tool
        | Qt.WindowType.WindowDoesNotAcceptFocus
    )


class TranslationOverlay(QWidget):
    def __init__(self, cfg: OverlayConfig):
        super().__init__(None, _overlay_flags() | Qt.WindowType.WindowTransparentForInput)
        self.cfg = cfg
        self.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground)
        self.setAttribute(Qt.WidgetAttribute.WA_ShowWithoutActivating)
        screen = QGuiApplication.primaryScreen()
        self.setGeometry(screen.geometry())
        self.dpr = screen.devicePixelRatio()
        self._items: list[Item] = []
        self._region: QRectF | None = None
        self._status: str | None = None
        self._error = False

    # --- 外から呼ぶ ---

    def to_logical(self, region: Region, x: float, y: float, w: float, h: float) -> QRectF:
        d = self.dpr
        return QRectF((region.x + x) / d, (region.y + y) / d, w / d, h / d)

    def show_status(self, region: Region, text: str, error: bool = False) -> None:
        self._region = self.to_logical(region, 0, 0, region.w, region.h)
        self._status = text
        self._error = error
        self._refresh()

    def show_translations(self, region: Region, items: list[Item]) -> None:
        self._region = self.to_logical(region, 0, 0, region.w, region.h)
        self._items = items
        self._status = None
        self._error = False
        self._refresh()

    def clear(self) -> None:
        self._items = []
        self._status = None
        self.hide()

    # --- 描画 ---

    def _refresh(self) -> None:
        if not self.isVisible():
            self.show()
        self.update()

    def _font(self, size: int) -> QFont:
        font = QFont(self.cfg.font_family)
        font.setPixelSize(size)
        return font

    def _box(self, p: QPainter, rect: QRectF, text: str, color: QColor) -> None:
        bg = QColor(0, 0, 0, int(255 * self.cfg.background_opacity))
        # 枠に収まるまで文字を小さくし、それでも無理なら下に伸ばす
        size = self.cfg.font_size
        flags = Qt.TextFlag.TextWordWrap
        while True:
            fm = QFontMetrics(self._font(size))
            needed = fm.boundingRect(QRect(0, 0, int(rect.width()) - 8, 10000), flags, text)
            if needed.height() + 6 <= rect.height() or size <= 11:
                break
            size -= 1
        box = QRectF(rect.x(), rect.y(), max(rect.width(), 40), max(rect.height(), needed.height() + 6))
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(bg)
        p.drawRoundedRect(box, 4, 4)
        p.setPen(color)
        p.setFont(self._font(size))
        p.drawText(box.adjusted(4, 3, -4, -3), int(flags | Qt.AlignmentFlag.AlignLeft), text)

    def paintEvent(self, _event) -> None:
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        offset = self.geometry().topLeft()
        p.translate(-offset)
        white = QColor(255, 255, 255)

        if self._region is not None and self._status:
            color = QColor(255, 120, 120) if self._error else QColor(255, 220, 120)
            r = self._region
            self._box(p, QRectF(r.x(), max(0, r.y() - 28), max(r.width(), 260), 26), self._status, color)

        if self.cfg.mode == "panel" and self._items and self._region is not None:
            r = self._region
            text = "\n".join(i.text for i in self._items)
            below = r.bottom() + 6
            self._box(p, QRectF(r.x(), below, r.width(), 40), text, white)
        else:
            for item in self._items:
                self._box(p, item.rect, item.text, white)
        p.end()


class RegionSelector(QWidget):
    """画面を暗くして、ドラッグした範囲を物理ピクセルの Region で返す。Esc で中止。"""

    selected = Signal(object)  # Region | None

    def __init__(self):
        super().__init__(None, _overlay_flags())
        self.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground)
        screen = QGuiApplication.primaryScreen()
        self.setGeometry(screen.geometry())
        self.dpr = screen.devicePixelRatio()
        self.setCursor(Qt.CursorShape.CrossCursor)
        self._start: QPoint | None = None
        self._end: QPoint | None = None

    def begin(self) -> None:
        self._start = self._end = None
        self.showFullScreen()
        self.activateWindow()
        self.raise_()
        self.setFocus()

    def _rect(self) -> QRect | None:
        if self._start is None or self._end is None:
            return None
        return QRect(self._start, self._end).normalized()

    def paintEvent(self, _event) -> None:
        p = QPainter(self)
        p.fillRect(self.rect(), QColor(0, 0, 0, 90))
        rect = self._rect()
        if rect:
            p.setCompositionMode(QPainter.CompositionMode.CompositionMode_Clear)
            p.fillRect(rect, Qt.GlobalColor.transparent)
            p.setCompositionMode(QPainter.CompositionMode.CompositionMode_SourceOver)
            p.setPen(QPen(QColor(80, 200, 255), 2))
            p.drawRect(rect)
        p.setPen(QColor(255, 255, 255))
        p.drawText(20, 30, "訳したい範囲をドラッグしてください（Esc で中止）")
        p.end()

    def mousePressEvent(self, e: QMouseEvent) -> None:
        self._start = self._end = e.position().toPoint()
        self.update()

    def mouseMoveEvent(self, e: QMouseEvent) -> None:
        if self._start is not None:
            self._end = e.position().toPoint()
            self.update()

    def mouseReleaseEvent(self, e: QMouseEvent) -> None:
        self._end = e.position().toPoint()
        rect = self._rect()
        self.hide()
        if rect is None or rect.width() < 8 or rect.height() < 8:
            self.selected.emit(None)
            return
        d = self.dpr
        self.selected.emit(
            Region(int(rect.x() * d), int(rect.y() * d), int(rect.width() * d), int(rect.height() * d))
        )

    def keyPressEvent(self, e) -> None:
        if e.key() == Qt.Key.Key_Escape:
            self.hide()
            self.selected.emit(None)
