"""同じ英文を毎回訳さないための SQLite キャッシュ。"""

from __future__ import annotations

import sqlite3
import threading
from pathlib import Path


class TranslationCache:
    def __init__(self, path: Path | str):
        self._lock = threading.Lock()
        self._db = sqlite3.connect(str(path), check_same_thread=False)
        self._db.execute(
            "CREATE TABLE IF NOT EXISTS translations ("
            " key TEXT NOT NULL, source TEXT NOT NULL, target TEXT NOT NULL,"
            " PRIMARY KEY (key, source))"
        )
        self._db.commit()

    def get(self, key: str, source: str) -> str | None:
        with self._lock:
            row = self._db.execute(
                "SELECT target FROM translations WHERE key = ? AND source = ?", (key, source)
            ).fetchone()
        return row[0] if row else None

    def put(self, key: str, source: str, target: str) -> None:
        with self._lock:
            self._db.execute(
                "INSERT OR REPLACE INTO translations (key, source, target) VALUES (?, ?, ?)",
                (key, source, target),
            )
            self._db.commit()
