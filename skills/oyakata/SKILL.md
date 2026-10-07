---
name: oyakata
description: "OYAKATA（親方）— リポジトリ横断で Claude Code のセッションを一覧・閲覧・追従し、ブラウザから指示も送れる司令塔画面を開く。「oyakata を開いて」「ブラウザで見たい」「セッション一覧」「他のリポジトリの Claude は何してる」「ターミナルだと読みづらい」「図で見たい」などで使う。開いた後の応答は図を Mermaid で書く。"
allowed-tools: Bash(oyakata:*)
argument-hint: "[session-id]"
---

# OYAKATA

ブラウザ上の司令塔。`~/.claude/projects` にある全セッションをリポジトリ別に並べ、稼働中のセッションの出力を 1 秒おきに追従して、Markdown・Mermaid・コードハイライト付きで表示する。リポジトリのファイルツリーと Git の差分、Claude が編集したファイル、作ったアーティファクトも同じ画面の右パネルで開ける。

## Step 1: 起動する

`oyakata` は常駐プロセスを自分で立ち上げ、ブラウザを開いてすぐ戻る（ブロックしない）。既に動いていればブラウザを開くだけなので、何度呼んでもよい。

```bash
oyakata                      # 引数なし: 今のセッションがブラウザで最初に開く
oyakata --focus <session-id> # 引数あり: そのセッションを開く
```

`$ARGUMENTS` が空なら 1 行目、セッション ID が渡されていれば 2 行目を実行する。`run_in_background` は不要。

標準出力の `OYAKATA is open at http://127.0.0.1:4848/...` の URL を、そのままユーザーに伝える。

`oyakata: command not found` の場合は、インストール方法を案内して止まる。別のコマンドを推測で試さない。

```bash
cargo install --git https://github.com/ShintaroOba/oyakata
oyakata install   # /oyakata スキルを ~/.claude/skills に入れる
```

## Step 2: 以降の応答は「ブラウザで読まれる」前提で書く

OYAKATA が開いている間、このセッションの応答は HTML として描画される。次に従う。

- 図は必ず ```` ```mermaid ```` フェンスで書く（flowchart / sequenceDiagram / classDiagram / stateDiagram-v2 / erDiagram / gantt / mindmap）。ASCII アートや罫線文字で図を描かない。
- 比較や一覧は Markdown の表にする。
- 長い説明は `##` 見出しで区切る。
- コードブロックには言語名を付ける（```` ```rust ````、```` ```bash ````、```` ```json ```` など）。
- ターミナル向けの幅合わせ（全角スペース、罫線での枠）はしない。
- ファイルを編集したら、応答の最後に編集したファイルのパスを列挙する（OYAKATA の「変更ファイル」から開ける）。

## ユーザーに聞かれたときの説明

- ブラウザの入力欄から指示を送れるのは、OYAKATA が起動したセッション（画面の「＋」でフォルダを選んで開く空のチャット、ターミナルの `oyakata new "指示"`、または終了済みセッションへの送信 / `oyakata attach --resume <id>`。権限モードは auto で始まる）と、ターミナルで `claude` を直接起動したセッション（Windows のみ）。後者は「ターミナルへ送信」でそのターミナルに打ち込んで Enter を押す仕組みなので、権限の確認や質問にはターミナル側で答える。IDE 拡張や SDK から動いているセッションには送れない。
- OYAKATA が持つセッションは、ターミナルからも `oyakata attach <id>` で同じ会話に入力できる（ブラウザと併用可）。`/quit` で端末だけ離脱、`/stop` でセッション終了。
- 常駐を長く使うなら、Claude の中ではなくターミナルで `oyakata` を実行して立てるよう案内する。Claude の中から立てた常駐は、その Claude セッションの終了に巻き込まれることがある。
- OYAKATA が起動したセッションでは、権限の確認と AskUserQuestion の回答もブラウザで行う。
- `oyakata status` で常駐の確認、`oyakata stop` で停止。停止すると OYAKATA が起動したセッションも終わるが、会話は残るので `claude --resume <id>` で続けられる。
- 画面はファイル変更を 1 秒ごとに検知して自動更新される。ユーザーにリロードを頼む必要はない。
