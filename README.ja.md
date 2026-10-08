<div align="center">

<img src="web/icon.svg" width="96" height="96" alt="OYAKATA">

# OYAKATA（親方）

**コーディングエージェントのセッションを、ブラウザから見渡して指図する。**

Claude Code・Codex CLI・Gemini CLI・Copilot CLI・OpenCode が複数のリポジトリで走らせているセッションを 1 つの画面に集め、読みやすく表示し、その場で指示を送れる司令塔です。表示は日本語と英語から選べます。

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg)](#ターミナルから使う)
[![Claude Code plugin](https://img.shields.io/badge/Claude%20Code-plugin-8A2BE2.svg)](#skill-で使い始める)

[English](README.md) | **日本語**

</div>

![OYAKATA の画面。左にセッション一覧（Claude Code・Codex・Gemini・Copilot のセッションが並ぶ）、右に回答が表・図・コード付きで表示されている](docs/images/hero.png)

## どんなもの？

コーディングエージェントを 2 つ 3 つ並行して動かし始めると、ターミナルだけでは手に負えなくなってきます。

- どのターミナルで何が進んでいるのか分からない
- 表や図を含む長い回答が、ターミナルでは読みづらい
- ツール呼び出しのログで会話が埋まってしまう

OYAKATA は、これをブラウザの画面 1 枚で解決します。各エージェントが手元に残す記録（Claude Code なら `~/.claude`、Codex CLI なら `~/.codex` など）を読んで表示するので、エージェントの起動方法や使い方を変える必要はありません。データはすべて手元の PC の中で完結し、外には出ません。

| | できること |
| --- | --- |
| **見渡す** | 全リポジトリ・全エージェントのセッションを一覧。動いているものは上に固定され、作業中 / 待機中 / 判断待ち が 1 秒ごとに更新される |
| **読む** | Markdown・表・コード・図を HTML として描画。長い回答には目次と折りたたみが付く |
| **指示する** | ブラウザの入力欄から指示を送る。Claude Code なら権限の確認や質問への回答もその場でできる |
| **仕上げる** | エージェントが変えたファイルを隣のエディタで開き、差分を確かめ、commit / push まで済ませる |

> 名前の由来: 親方（あなた）が、棟梁（メインのエージェント）と職人（サブエージェント）の仕事ぶりを見て指図する、という見立てです。

## 対応エージェント

| エージェント | セッションの読み取り | ブラウザからの起動・指示 |
| --- | --- | --- |
| **Claude Code** | `~/.claude/projects` | `claude -p`。作業中でも指示を割り込ませられ、権限の確認・質問・計画の承認もブラウザで答える |
| **Codex CLI** (OpenAI) | `~/.codex/sessions` | `codex exec` / `codex exec resume`。指示 1 つが 1 ターン |
| **Gemini CLI** (Google) | `~/.gemini/tmp/*/chats` | `gemini --output-format stream-json`。指示 1 つが 1 ターン |
| **Copilot CLI** (GitHub) | `~/.copilot/session-state` | `copilot --output-format json`。指示 1 つが 1 ターン |
| **OpenCode** | `opencode db` 経由でデータベースから | `opencode run`。指示 1 つが 1 ターン |

入っているエージェントは自動で見つけます（データの置き場所か、コマンドが見つかれば対象）。表示するかどうかは設定で切り替えられます。Claude Code 以外は非対話で 1 ターンずつ動かすので、権限の確認カードは出ません。代わりに、ステータス行の権限モードを各 CLI のサンドボックス / 承認の設定に写します。

| 権限モード | Codex CLI | Gemini CLI | Copilot CLI | OpenCode |
| --- | --- | --- | --- | --- |
| auto（既定） | `sandbox_mode=workspace-write` | `--approval-mode yolo` | `--allow-all-tools` | `--auto` |
| acceptEdits | `workspace-write` | `auto_edit` | `--allow-all-tools` | `--auto` |
| default | `workspace-write` | `default` | （ツールは許可しない） | （権限は許可しない） |
| plan | `read-only` | `plan` | （ツールは許可しない） | `--agent plan` |

ターミナルなど別のプロセスで動いている最中のセッションは「別のプロセスで実行中」として読むだけになり、終わると送れるようになります。

## Skill で使い始める

Claude Code のプラグインとして `/oyakata` スキルを入れるだけで使えます。Rust のビルドは要りません。

**1. プラグインを入れる**（Claude Code の中で）

```
/plugin marketplace add ShintaroOba/oyakata
/plugin install oyakata@oyakata
```

ターミナルからなら `claude plugin marketplace add ShintaroOba/oyakata` → `claude plugin install oyakata@oyakata` です。

**2. `/oyakata` と打つ**

```
/oyakata
```

初めて呼んだときは本体（`oyakata` コマンド）がまだ無いので、Claude がスキル同梱のスクリプトで [GitHub Releases](https://github.com/ShintaroOba/oyakata/releases) からビルド済みバイナリを入れます（Windows / macOS / Linux）。そのあと常駐プロセスを立ち上げ、ブラウザが今のセッションを開いた状態で起動します。初回はブラウザで表示言語（日本語 / English）を選びます。

以降、そのセッションの Claude は「ブラウザで読まれている」ことを前提に、比較は表で、図は描画できる形（Mermaid）で書くようになります。`/oyakata` と打たなくても、「ブラウザで見たい」「他のリポジトリの Claude は何してる？」のような言葉でもスキルは呼び出されます。

**Codex CLI・Gemini CLI・Copilot CLI・OpenCode から使うには**、本体を入れたあとに一度 `oyakata install` を実行します。各エージェントがスキルを探す場所（`~/.codex/skills`、共通の `~/.agents/skills`）に同じ SKILL.md が置かれるので、「oyakata を開いて」と頼めば同じように開きます。

## 画面の見かた

### セッション一覧と会話

![セッション一覧と会話（ダークテーマ）](docs/images/hero-dark.png)

- **左のサイドバー**には、動いているセッションが上に、その下にリポジトリごとの履歴が並びます。点の色が状態です（橙: 作業中、緑: 待機中、紫: 判断待ち）。Claude Code 以外のセッションには「Codex CLI」のようにエージェント名が付きます。サイドバーの下にも「作業中 1 · 判断待ち 1」のように件数が出ます。
- **セッションをクリック**すると会話が開きます。ツール呼び出しなどの作業ログは隠れていて、今何をしているかは入力欄の上の 1 行（`Bash: Run the filter tests … esc で中断`）で分かります。設定で作業ログを表示することもできます。
- **入力欄の下**はターミナルと同じステータス行です。左が権限モード（Shift+Tab で切替）、右がモデル・努力レベル・コンテキスト使用率。Claude Code 以外では、変更は次のターンから効きます。

### 新しいセッションを始める

![新しいセッションのダイアログ](docs/images/new-session.png)

ヘッダーの「＋」でエージェントとフォルダを選ぶだけです。空のチャットが開き、最初の指示を送った時点でエージェントが起動します。権限モードは auto から始まります。

Claude Code なら作業中でも指示を送れます。送った指示は次のツール呼び出しの区切りで Claude に渡り、それまでは入力欄の上に「次の区切りで渡します」と並びます。権限の確認、`AskUserQuestion` への回答、計画の承認もチャットの中のカードで答えられます。承認には朱色の「承認」、差し戻しには藍色の「差戻」の判子が押されます。他のエージェントでは、作業中に送った指示は今のターンが終わってから次のターンとして渡ります。

### 体制図（サブエージェントの様子）

![体制図](docs/images/team.png)

チャット右上の「👥」を押すと、メインのエージェント（棟梁）とサブエージェント（職人）の構成が見えます。誰がどの仕事を担当し、今どのツールを実行していて、終わったときに何を報告したか。並行で振られた仕事は「陣」にまとまり、ライブで更新されます。

### ファイル・差分・Git

![エディタと差分と Git ビュー](docs/images/workbench.png)

会話の隣に、エージェントが編集したファイルをエディタで開き（Ctrl+S で保存）、差分を見て、サイドバーの Git ビューから commit / push / pull まで行えます。ペインは VS Code のようにタブをドラッグして分割でき、配置はリポジトリごとに記憶されます。

ほかにもこんなことができます。

- `Ctrl+P` でファイル名のあいまい検索、`Ctrl+Shift+F` でリポジトリの全文検索とすべての会話の横断検索
- エディタで `F12` / `Ctrl+クリック` で定義へ、`Shift+F12` で参照一覧（言語サーバー不要）
- 会話の中の `src/main.rs:42` のようなファイル参照をクリックすると、その行がエディタで開く
- 作業が終わると拍子木が「カン、カン」と鳴る。待機や判断待ちになったらデスクトップ通知（任意）
- テーマはライト / ダーク / セピア / Solarized / Nord / Dracula / 高コントラスト。アクセント色、文字サイズ、本文の幅も変えられる
- 要らないセッションは一覧から削除。記録はゴミ箱（`~/.oyakata/trash`）に移り、30 日で消える（OpenCode はデータベースなので、その場で消える）

### 言語

初回にブラウザで日本語か英語かを選びます。あとから設定（⚙）の「言語 / Language」で変えられます。選んだ言語は `~/.oyakata/config.json` に記録され、`oyakata new` や `oyakata attach` などターミナル側の表示も同じ言語になります。環境変数 `OYAKATA_LANG=ja|en` で一時的に上書きできます。

## ターミナルから使う

スキルを使わなくても、普通のコマンドとして動きます。本体は単一のバイナリで、実行時の依存はありません。

### 本体を入れる

ビルド済みバイナリ（Windows x64 / arm64、macOS Intel / Apple Silicon、Linux x64 / arm64）を [GitHub Releases](https://github.com/ShintaroOba/oyakata/releases) に置いています。インストールスクリプトが環境に合うものを取り、SHA-256 を照合して PATH の通る場所に置きます。

```bash
# macOS / Linux: ~/.local/bin に置く
curl -fsSL https://raw.githubusercontent.com/ShintaroOba/oyakata/main/scripts/install.sh | sh
```

```powershell
# Windows: %LOCALAPPDATA%\Programs\oyakata に置き、ユーザーの PATH に加える
irm https://raw.githubusercontent.com/ShintaroOba/oyakata/main/scripts/install.ps1 | iex
```

置き場所は `OYAKATA_INSTALL_DIR`、版は `OYAKATA_VERSION=v0.1.0` で指定できます。Rust（1.80 以上）がある環境なら `cargo install --git https://github.com/ShintaroOba/oyakata` でも入ります。

そのほかに必要なもの:

| | |
| --- | --- |
| 使いたいエージェントの CLI | `claude`、`codex`、`gemini`、`copilot`、`opencode` のうち、ブラウザから起動したいもの。閲覧だけなら不要 |
| git | ファイルツリー・差分・Git 操作に使います |

### 起動と停止

```bash
oyakata                     # 常駐を起動（既に動いていれば再利用）してブラウザを開く
oyakata --focus <session>   # 指定したセッションを開いた状態で起動
oyakata status              # 常駐が動いているか確認
oyakata stop                # 常駐を停止（OYAKATA が起動したセッションも終了。会話はディスクに残る）
oyakata install             # /oyakata スキルを各エージェントのスキル置き場にコピー
```

常駐を長く使うなら、エージェントの中（`/oyakata`）からではなくターミナルで `oyakata` を実行して立ててください。エージェントの中から立てた常駐は、そのセッションの終了に巻き込まれて止まることがあります。止まっても会話は残るので、ブラウザでそのセッションに送信すれば OYAKATA が引き継いで再開します。

<details>
<summary>そのほかのコマンドとオプション</summary>

| コマンド | 動作 |
| --- | --- |
| `oyakata --no-open` | ブラウザを開かずに常駐だけ起動 |
| `oyakata serve` | フォアグラウンドで実行（ログを見たいとき） |
| `oyakata new "最初の指示"` | このフォルダで OYAKATA が持つセッションを始め、この端末を接続する。`--agent codex` のようにエージェントを選べる。`--cwd <dir>`、`--model`、`--mode`、`--effort`、`--no-attach` も指定できる |
| `oyakata attach <session-id>` | OYAKATA が持つセッションにこの端末を接続する（`/quit` で端末だけ離脱、`/stop` でセッション終了） |
| `oyakata attach --resume <session-id>` | 終了済みのセッションを OYAKATA で再開する |
| `oyakata sessions` | OYAKATA が今持っているセッションの一覧 |

| オプション | 既定値 | 意味 |
| --- | --- | --- |
| `--port <n>` | `4848` | 待ち受けポート |
| `--bind <addr>` | `127.0.0.1` | バインドするアドレス |
| `--claude-dir <path>` | `$CLAUDE_CONFIG_DIR` または `~/.claude` | Claude Code の設定ディレクトリ |
| `--claude <path>` | `PATH` から探す | ブラウザから起動するセッションに使う `claude` の実行ファイル |
| `--repo-root <dir>` | | 直下のフォルダをリポジトリとして一覧に加える（複数指定可） |

他のエージェントの場所は自動検出のほか、環境変数（`CODEX_HOME`、`COPILOT_HOME`、`OYAKATA_CODEX` のように `OYAKATA_<AGENT>` でコマンドのパス）か、`~/.oyakata/config.json` の `agents` で指定できます。常駐のログは `~/.oyakata/oyakata.log` に出ます。

OYAKATA が持つセッションは、ブラウザからもターミナルからも同じ会話に入力できます。権限の確認や質問にもどちらからでも答えられます。

```bash
oyakata new "パーサーをリファクタして"            # Claude Code で始めて、この端末を接続
oyakata new --agent codex "テストを直して"     # Codex CLI で始める
oyakata attach <session-id>                     # 別の端末からも同じ会話に入る
```

</details>

## よくある質問

**ブラウザから指示を送れるのは、どのセッション？**

| セッション | ブラウザから |
| --- | --- |
| OYAKATA が起動したもの（「＋」や `oyakata new`） | すべて操作できる。Claude Code は作業中でも送れて、権限の確認や質問にもチャットで答えられる。他のエージェントは今のターンが終わってから次のターンとして渡る |
| 終了済み | 送信すると OYAKATA がそのエージェントで再開し、以後は上の行と同じ |
| ターミナルで稼働中の Claude Code（Windows） | 「ターミナルへ送信」で、そのターミナルに打ち込んで Enter を押す。権限の確認はターミナル側で答える |
| 別のプロセスで稼働中の他エージェント | 読むだけ。終わると送れる |
| IDE 拡張・SDK で稼働中 | 送れない（打ち込む先のターミナルが無いため）。終了後に引き継げる |

**データはどこに行く？**

どこにも行きません。サーバーは `127.0.0.1` だけで待ち受け、閲覧は各エージェントが書き出すファイル（Claude Code なら `~/.claude/projects/*/*.jsonl` と `~/.claude/sessions/*.json`）を読むだけです。OpenCode だけは `opencode db` コマンドでデータベースを読みます。描画に使うライブラリ（marked・DOMPurify・highlight.js・Mermaid・CodeMirror）はバイナリに同梱していて、オフラインで動きます。

**エージェントの設定を変える必要は？**

ありません。任意で、Claude Code のすべてのセッションに図を Mermaid で描かせたいときは `~/.claude/CLAUDE.md` に次の 1 行を足してください。

```
図は ```mermaid フェンスで書く（OYAKATA がブラウザで描画する）。ASCIIアートで図を描かない。
```

<details>
<summary>キーボードショートカット</summary>

VS Code に合わせています。設定の「ショートカット一覧」でも見られます。

| キー | 動作 |
| --- | --- |
| `Ctrl+P` | ファイルを開く（あいまい検索。`main.rs:42` で行も指定。`>` でコマンド、`:` で行、`@` でセッション） |
| `Ctrl+Shift+P` / `F1` | コマンドパレット |
| `Ctrl+Shift+F` | 全文検索（このリポジトリのファイル / すべての会話） |
| `Ctrl+F` / `F3` / `Shift+F3` | エディタ内を検索 / 次 / 前 |
| `Ctrl+G` | 行へ移動 |
| `F12` / `Ctrl+クリック` | 定義へ移動（候補が複数なら一覧から選ぶ） |
| `Shift+F12` | 参照を検索（検索ビューに一覧） |
| `Alt+←` / `Alt+→` | ジャンプ前の場所へ戻る / 進む |
| `Ctrl+Shift+E` / `Ctrl+Shift+G` | ツリー / Git を表示 |
| `Ctrl+B` | サイドバーの表示 / 非表示 |
| `` Ctrl+` `` | チャットの入力欄へ |
| `Ctrl+\` | アクティブなタブを右に分割 |
| `Ctrl+Alt+N` | 新しいセッション |
| `Ctrl+,` | 設定 |
| `Ctrl+S` | エディタで保存 |
| `Enter` / `Shift+Enter` | 入力欄で送信 / 改行 |
| `Shift+Tab` | 権限モードを切替（auto → default → acceptEdits → plan） |
| `Esc` | エージェントが作業中なら中断。それ以外はメニュー・ダイアログを閉じる |
| `/` / `t` | セッション検索 / ライト・ダーク切替 |
| 中クリック | タブを閉じる |

`Ctrl+W`・`Ctrl+Tab`・`Ctrl+N` などはブラウザが先に使うため割り当てていません。

</details>

<details>
<summary>仕組みと安全性</summary>

- **索引。** Claude Code の `~/.claude/projects/<cwd>/<session>.jsonl` を起動時に 1 回読んでメタデータ（タイトル、cwd、往復数、トークン、権限モード、編集ファイルなど）を作り、以後はファイルの増分だけを読み足します。本文をメモリに持つのは表示中のセッションだけです。
- **他のエージェント。** Codex CLI（rollout JSONL）と Copilot CLI（events.jsonl）は追記分を、Gemini CLI（後から書き換わる JSONL）はファイル全体を、OpenCode は `opencode db` の結果を、それぞれ Claude Code と同じ形の行に変換してから同じ索引に入れます。変換ルールは `src/agents/` にあり、`tests/fixtures/` の実物のセッションで検査しています。
- **稼働中の判定。** Claude Code は `~/.claude/sessions/<pid>.json` のうち pid が生きているものを「稼働中」とし、`status: waiting` を「判断待ち」とします。他のエージェントは稼働中の記録を残さないので、セッションのファイルが 20 秒以内に更新されていれば「別のプロセスで実行中」とみなします。
- **コンテキスト使用率。** 直近の応答のトークン数（input + cache + output）をモデルのコンテキスト長で割った値です。OYAKATA が起動した Claude Code セッションは Claude Code の報告値、それ以外はモデル名から推定します。
- **体制図。** `<session>/subagents/agent-*.jsonl` と `*.meta.json` を読み、呼び出し元の Agent ツール呼び出しと突き合わせます（Claude Code）。
- **ブラウザから起動するセッション。** Claude Code は `claude -p --input-format stream-json --output-format stream-json --permission-prompt-tool stdio` の子プロセスで、指示は stdin に書き、権限の確認は stdout から受け取ります。他のエージェントは指示ごとに `codex exec` / `gemini` / `copilot` / `opencode run` を起動し、指示は stdin で渡し、JSON のイベント列で状態を追い、次の指示は同じセッションの再開として起動します。会話は各エージェント自身が書き出すので、表示は他のセッションと同じ経路です。
- **ターミナルへの打ち込み（Windows）**は、そのコンソールの入力バッファへキー入力を書き込みます（`AttachConsole` + `WriteConsoleInputW`）。打ち込む前にプロセスの起動時刻を照合し、pid が別のプロセスに再利用されていたら打ちません。
- **Git 操作**は `git` コマンドをそのまま呼びます。commit は選択したファイル、push は上流が無ければ `-u origin HEAD`、pull は `--ff-only`。ブランチ切替は意図的に入れていません。
- **定義へのジャンプと検索**は `git grep` です。言語サーバーは使いません。
- **ファイル保存**は読み込み時の更新時刻を添えて送り、ディスク上で変わっていれば止めて「上書き / 読み込み直す」を選ばせます。
- **ネットワーク。** `/api` へのクロスサイト要求は `Sec-Fetch-Site` で拒否し、書き込み系はカスタムヘッダ必須にしています。

</details>

## 開発

```bash
cargo test
cargo run -- serve --no-open
```

UI の文言は日本語で書き、英語は辞書で当てます。文言を足したら `python docs/i18n/transform.py` で `t()` に包んでキーを集め、`docs/i18n/en.json` に英語を書き、`python docs/i18n/build.py` で `web/i18n-en.js` を生成します（未訳のキーは報告されます）。Rust 側の文言は `i18n::tr(ja, en)` です。

リリースは、`Cargo.toml` と `.claude-plugin/plugin.json` の `version` を同じ番号に上げてから `vX.Y.Z` のタグを push します。`.github/workflows/release.yml` が 6 つのターゲットをビルドして GitHub Release に添付し、インストールスクリプトはそこから取ります。

```bash
git tag v0.1.0 && git push origin v0.1.0
```

```
src/
  main.rs        CLI（起動・常駐・停止・スキル導入）
  server.rs      axum ルート、SSE、ファイル監視ループ、OpenCode の取り込みループ、クロスサイト防御
  index.rs       セッション索引と増分更新、リポジトリ一覧、設定とゴミ箱
  agents/        エージェントごとの読み取り・変換・起動コマンド（codex / gemini / copilot / opencode）と共通の行の組み立て（canon）
  transcript.rs  JSONL → 表示アイテム、編集ファイル・アーティファクトの抽出
  runner.rs      OYAKATA が起動するセッション（Claude Code は stream-json、他は 1 ターンずつの子プロセス）
  i18n.rs        ターミナル側の表示言語
  console.rs     ターミナルで稼働中のセッションへの打鍵（Windows コンソール入力）
  client.rs      ターミナル側フロント（new / attach / sessions）
  gitops.rs      git status / tree / diff / log / grep / commit / push / pull / clone
  live.rs        稼働中セッション（pid 生存確認）
  paths.rs       PATH の補完、実行ファイルと ~/.oyakata の場所
  repo.rs        cwd → リポジトリ名
web/
  index.html / style.css / icon.svg
  i18n.js / i18n-en.js  表示言語の切替と英語辞書（docs/i18n から生成）
  app.js         共通: API・テーマ・Markdown・トランスクリプト描画・イベントバス・設定・言語選択
  workbench.js   ペインの分割ツリー、タブ、ドラッグ＆ドロップ、リポジトリごとの配置
  chat.js        チャットペイン（会話・入力欄とステータス行・許可/質問/計画カード・進行表示）
  team.js        体制図（棟梁とサブエージェントの構成・状態）
  code.js        定義 / 参照へのジャンプ、戻る / 進む、会話中のファイル参照
  palette.js     Ctrl+P / コマンドパレット / 候補の選択
  search.js      全文検索ビュー（ファイル / 会話）
  fx.js          判子と拍子木
  editors.js     CodeMirror エディタ、差分・コミット・URL・サブエージェントのビュー
  sidebar.js     セッション一覧 / ツリー / Git（commit・push・pull）、削除、リポジトリ追加
  vendor/        同梱ライブラリ（marked, DOMPurify, highlight.js, Mermaid, CodeMirror 5）
skills/oyakata/  /oyakata スキル（Claude Code のプラグイン、および oyakata install で各エージェントに配る SKILL.md）
scripts/         install.sh / install.ps1（GitHub Releases のビルド済みバイナリを入れる）
tests/fixtures/  各エージェントの実物のセッション記録（個人情報を除いたもの）
docs/i18n/       UI 文言のキー一覧と英語辞書、生成スクリプト
docs/images/     README のスクリーンショット（ダミーのセッションとリポジトリで撮影）
.claude-plugin/  プラグインとマーケットプレイスのマニフェスト
.github/workflows/release.yml  バージョンタグで 6 ターゲットをビルドして Release を公開
```

## ライセンス

MIT。同梱ライブラリのライセンスは `web/vendor/LICENSE.*` を参照してください。
