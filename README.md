# Multi Point Clicker

最大10か所のクリック位置を記憶し、指定間隔で順番に自動クリックする Windows 向けGUIツールです。
A Windows GUI tool that remembers up to 10 click positions and clicks them in order at a set interval.

日本語 / English の切り替えに対応 (初回は OS の言語を自動判定)。ライト/ダークテーマ対応。

## 使い方 / Usage

```bash
pip install -r requirements.txt
python clicker.py
```

1. **記憶開始 / Start recording** — 画面上の好きな場所を順番に左クリック (最大10か所)。番号付きの丸が画面に表示されます。
2. **記憶終了 / Stop recording** — 位置を確定・保存。
3. **自動クリック開始 / Start clicking** — 記憶した位置を指定間隔で順番にクリックし続けます。
4. **自動クリック終了 / Stop clicking** — 停止 (位置は保持されます)。

すべての操作は画面上のボタンでも、ショートカットキーでも行えます。

## ショートカットキー / Hotkeys

ファンクションキーやテンキーが無いキーボードでも使えるよう、初期値は Ctrl + Alt + 英字です。

| 操作 | 初期値 |
|------|--------|
| 記憶開始 | Ctrl + Alt + R |
| 記憶終了 | Ctrl + Alt + D |
| 自動クリック開始 | Ctrl + Alt + S |
| 自動クリック終了 | Ctrl + Alt + X |

「設定」タブでボタンを押し、割り当てたいキーの組み合わせを押すと変更できます (Esc でキャンセル、↺ で初期値に戻す)。

## 機能 / Features

- クリック間隔 (0.05秒〜、プリセットあり) / 繰り返し周回数 (0 = 無限) / 開始前の待機時間 / 左右ボタン選択
- クリック数・周回数・経過時間のリアルタイム表示
- 位置の個別削除・全削除、画面上への位置プレビュー
- 設定と位置は自動保存 (`%APPDATA%\MultiPointClicker\settings.json`)。位置は新たに記憶し直したときだけ上書きされます
- 記憶モード中、このツール自身のウィンドウ上のクリックは記録されません

## exe 化 (任意)

```bash
pip install pyinstaller
pyinstaller --onefile --noconsole --collect-all customtkinter clicker.py
```

## 注意

オンラインゲーム等では自動操作が規約で禁止されている場合があります。自己責任で使用してください。
