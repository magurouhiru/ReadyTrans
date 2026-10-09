"""OCR の行をまとまり（段落）にまとめる。行単位より段落単位の方が自然に訳せる。"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class TextBlock:
    text: str
    x: float
    y: float
    w: float
    h: float

    @property
    def right(self) -> float:
        return self.x + self.w

    @property
    def bottom(self) -> float:
        return self.y + self.h


def _same_paragraph(prev: TextBlock, line: TextBlock, line_h: float) -> bool:
    gap = line.y - prev.bottom
    if gap < -line_h * 0.5 or gap > line_h * 0.8:
        return False
    # 横方向に重なっていること（別カラムの行は混ぜない）
    overlap = min(prev.right, line.right) - max(prev.x, line.x)
    return overlap > 0 and abs(line.x - prev.x) < line_h * 3


def group_lines(lines: list[TextBlock]) -> list[TextBlock]:
    lines = sorted((l for l in lines if l.text.strip()), key=lambda l: (l.y, l.x))
    paragraphs: list[list[TextBlock]] = []
    for line in lines:
        for para in paragraphs:
            last = para[-1]
            if _same_paragraph(last, line, max(last.h, line.h)):
                para.append(line)
                break
        else:
            paragraphs.append([line])

    blocks = []
    for para in paragraphs:
        x = min(l.x for l in para)
        y = min(l.y for l in para)
        right = max(l.right for l in para)
        bottom = max(l.bottom for l in para)
        text = " ".join(l.text.strip() for l in para)
        blocks.append(TextBlock(text, x, y, right - x, bottom - y))
    return blocks
