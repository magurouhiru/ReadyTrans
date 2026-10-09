# ReadyTrans.exe（1ファイル）を dist/ に作る。リポジトリのルートで実行する。
#   powershell -ExecutionPolicy Bypass -File packaging/build.ps1
$ErrorActionPreference = "Stop"
uv sync --group build
uv run pyinstaller `
    --noconfirm --clean --onefile --windowed `
    --name ReadyTrans `
    --paths src `
    --add-data "config.example.toml;." `
    --add-data "profiles;profiles" `
    --collect-all winrt `
    --collect-all windows_capture `
    packaging/entry.py
