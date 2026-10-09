# ReadyTrans

ゲーム画面の英語を読み取り、自分の PC で動く AI（Gemma など）で日本語に訳して、画面に重ねて表示する Windows アプリです。

- ゲームのファイルやメモリには一切触れません。OBS などと同じ OS 標準の画面キャプチャで撮り、別の透明ウィンドウに訳を表示します
- インターネット不要。翻訳は Ollama などで動くオープンウェイトモデルが行います
- ゲームごとの用語集（プロファイル）で、固有名詞の訳し方を揃えられます

> まだ試作段階です。

## 必要なもの

- Windows 10（2004 以降）または Windows 11
- [Ollama](https://ollama.com/)
- [uv](https://docs.astral.sh/uv/)（Python の実行環境をまとめて用意してくれるツール）
- Windows の英語 OCR（「設定 > 時刻と言語 > 言語と地域」で英語を追加すると入ります）

## セットアップ

```powershell
# 1. 翻訳モデルを取得（VRAM 目安: 4b → 6GB, 12b → 12GB, 27b → 20GB 以上）
ollama pull gemma3:12b

# 2. このリポジトリを取得して依存関係を入れる
git clone https://github.com/magurouhiru/ReadyTrans.git
cd ReadyTrans
uv sync

# 3. 設定ファイルを作る（モデル名やプロファイルを編集）
copy config.example.toml config.toml

# 4. 翻訳と OCR が動くか確認
uv run python scripts/check_setup.py

# 5. 起動
uv run readytrans
```

起動するとタスクトレイに青いアイコンが出ます。

## 使い方

| ホットキー | 動作 |
|---|---|
| `Ctrl+Shift+T` | 範囲をドラッグで選んで訳す |
| `Ctrl+Shift+R` | 前回の範囲をもう一度訳す |
| `Ctrl+Shift+A` | 自動モード（範囲を監視し、文字が変わったら訳す）の切り替え |
| `Ctrl+Shift+H` | 訳の表示を消す |

ホットキーは `config.toml` で変えられます。

### ゲーム側の設定

ゲームは **ボーダーレスウィンドウ**（ウィンドウフルスクリーン）で起動してください。排他フルスクリーンだと訳が上に表示されません。

## ゲームごとの設定（プロファイル）

`profiles/` に TOML ファイルを置き、`config.toml` の `profile` でファイル名を指定します。

```toml
name = "Active Matter"
instructions = "アイテム名は英語のまま残してください。"

[glossary]
"Raid" = "レイド"
"Active Matter" = ""   # 空文字なら原文のまま
```

`profiles/active_matter.toml` の用語集は仮のものです。遊びながら直していってください。

## 翻訳AIを変える

`config.toml` の `[llm]` で切り替えます。

- Ollama の別モデル: `model = "gemma3:4b"` など
- LM Studio や llama.cpp の server: `backend = "openai"`、`base_url = "http://localhost:1234"`

## アンチチートについて

このアプリはゲームのプロセスに注入せず、メモリも読まず、OS 標準の画面キャプチャと普通のウィンドウだけを使います（配信ソフトや画面共有と同じ種類の操作）。一般的にはアンチチートの対象になりにくい方式ですが、最終的には各ゲームの規約に従ってください。

## 開発

```powershell
uv run pytest
```

| ファイル | 役割 |
|---|---|
| `src/readytrans/app.py` | 全体の流れ・トレイアイコン |
| `src/readytrans/capture.py` | 画面キャプチャ（Windows Graphics Capture、だめなら mss） |
| `src/readytrans/ocr.py` | Windows 標準 OCR |
| `src/readytrans/layout.py` | OCR の行を段落にまとめる |
| `src/readytrans/translator.py` | Ollama / OpenAI 互換 API での翻訳 |
| `src/readytrans/cache.py` | 訳のキャッシュ（SQLite） |
| `src/readytrans/overlay.py` | 範囲選択と訳の表示 |
| `src/readytrans/hotkey.py` | グローバルホットキー |
