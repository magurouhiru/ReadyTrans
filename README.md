# ReadyTrans

ゲーム画面の英語を読み取り、自分の PC で動く AI（Gemma など）で日本語に訳して、画面に重ねて表示する Windows アプリです。

- ゲームのファイルやメモリには一切触れません。OBS などと同じ OS 標準の画面キャプチャで撮り、別の透明ウィンドウに訳を表示します
- インターネット不要。翻訳は Ollama などで動くオープンウェイトモデルが行います
- ゲームごとの用語集（プロファイル）で、固有名詞の訳し方を揃えられます

> まだ試作段階です。

## ダウンロードして使う（かんたん）

1. [Releases](https://github.com/magurouhiru/ReadyTrans/releases) から `ReadyTrans.exe` をダウンロードして、好きなフォルダに置く
2. Ollama を起動しておく（下の「必要なもの」を参照）
3. `ReadyTrans.exe` を起動する

初回起動時に、exe と同じフォルダに `config.toml`（設定）と `profiles/`（ゲームごとの用語集）が作られます。翻訳モデルが無ければ自動でダウンロードします。ログは同じフォルダの `readytrans.log` に出ます。

## 必要なもの

- Windows 10（2004 以降）または Windows 11
- [Ollama](https://ollama.com/)（Windows 版をインストールするか、下の Docker で動かします）
- Windows の英語 OCR（「設定 > 時刻と言語 > 言語と地域」で英語を追加すると入ります）

## セットアップ

### Ollama を Docker で動かす場合

Docker Desktop（NVIDIA の GPU を使う設定）が入っていれば、次のコマンドで Ollama を起動できます。

```powershell
docker run -d --gpus=all -v ollama:/root/.ollama -p 11434:11434 --name ollama ollama/ollama
```

ダウンロードしたモデルは `ollama` ボリュームに保存されるので、コンテナを作り直しても残ります。2回目以降は `docker start ollama` で起動できます。`config.toml` の `base_url` は既定の `http://localhost:11434` のままで大丈夫です。

### ReadyTrans

[Releases](https://github.com/magurouhiru/ReadyTrans/releases) の `ReadyTrans.exe` を置いて起動するだけです。動かないときは、PowerShell で `ReadyTrans.exe --self-test` を実行すると、OCR・設定・翻訳AIへの接続・画面キャプチャを確認して結果を `selftest.log` に書きます。

起動するとタスクトレイに青いアイコンが出ます。設定のモデルが Ollama に無ければ自動でダウンロードします（translategemma:4b は約3GB、進み具合はログと画面右下の通知に出ます）。

## 使い方

| ホットキー | 動作 |
|---|---|
| `Ctrl+Shift+T` | 範囲をドラッグで選んで訳す（Esc か右クリックで中止） |
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

`config.toml` の `[llm]` の `model` で切り替えます。

| モデル | 特徴 |
|---|---|
| `translategemma:4b`（既定） | Google の翻訳専用 Gemma。速くて軽い。用語集・ゲームごとの指示は効かない |
| `translategemma:12b` | 同じく翻訳専用で、より正確。重い |
| `gemma4` / `gemma3` など汎用モデル | 用語集やゲームごとの指示がよく効く。重い |
| LFM2-350M-ENJP-MT | とても軽い英日翻訳専用モデル。固有名詞に弱い |

翻訳は段落ごとに行い、訳せたものから順に表示します。

- LM Studio や llama.cpp の server: `backend = "openai"`、`base_url = "http://localhost:1234"`

## アンチチートについて

このアプリはゲームのプロセスに注入せず、メモリも読まず、OS 標準の画面キャプチャと普通のウィンドウだけを使います（配信ソフトや画面共有と同じ種類の操作）。一般的にはアンチチートの対象になりにくい方式ですが、最終的には各ゲームの規約に従ってください。

## 開発

Rust で書いています（[rustup](https://rustup.rs/) で入れられます）。

```powershell
cargo test                  # 翻訳・設定・段落まとめの単体テスト（Windows 以外でも動く）
cargo build --release       # target/release/readytrans.exe
target/release/readytrans.exe --self-test   # 結果は exe の隣の selftest.log
```

設定・キャッシュ（`cache.jsonl`）・ログは exe と同じフォルダに置かれます。push すると GitHub Actions が exe を作って自己診断し、`v0.2.0` のようなタグを push するか、Actions の「Build」を手動実行して `release_tag` を入れると Releases に載せます。

| ファイル | 役割 |
|---|---|
| `src/app.rs` | 全体の流れ・オーバーレイ（egui）・範囲選択・トレイ・ホットキー |
| `src/capture.rs` | 画面キャプチャ（Windows Graphics Capture、だめなら GDI） |
| `src/ocr.rs` | Windows 標準 OCR |
| `src/layout.rs` | OCR の行を段落にまとめる |
| `src/translator.rs` | Ollama / OpenAI 互換 API での翻訳 |
| `src/cache.rs` | 訳のキャッシュ |
| `src/config.rs` | 設定ファイルとプロファイル（`config.example.toml` と `profiles/` を exe に埋め込む） |
| `src/selftest.rs` | `--self-test` |
| `src/logger.rs` | ログ（`readytrans.log`） |
