# PyInstaller 用の入り口（パッケージの相対 import を使うため、__main__.py は直接使えない）
from readytrans.app import main

main()
