# Builds a self-contained OYAKATA demo (fake Claude Code data + small git repos) under
# C:\Temp\oyakata-demo so screenshots contain no real sessions, users or repositories.
import json, os, subprocess, sys, uuid, shutil, time
from datetime import datetime, timedelta, timezone

ROOT = r"C:\Temp\oyakata-demo"
HOME = os.path.join(ROOT, "home")
CLAUDE = os.path.join(ROOT, "claude")
SRC = os.path.join(ROOT, "src", "github.com", "example")
if os.path.isdir(ROOT):
    shutil.rmtree(ROOT)
for d in (HOME, os.path.join(CLAUDE, "projects"), os.path.join(CLAUDE, "sessions"), SRC):
    os.makedirs(d, exist_ok=True)
with open(os.path.join(HOME, ".gitconfig"), "w", encoding="utf-8") as f:
    f.write("[user]\n\tname = Demo User\n\temail = demo@example.com\n[core]\n\tautocrlf = false\n")

GIT_ENV = {**os.environ, "HOME": HOME, "USERPROFILE": HOME, "GIT_CONFIG_GLOBAL": os.path.join(HOME, ".gitconfig")}


def git(cwd, *args):
    subprocess.run(["git", *args], cwd=cwd, env=GIT_ENV, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


# ------------------------------------------------------------------ repos
def repo(name, commits):
    d = os.path.join(SRC, name)
    os.makedirs(d)
    git(d, "init", "-q", "-b", "main")
    for msg, batch in commits:
        for p, text in batch:
            write(os.path.join(d, p), text)
        git(d, "add", "-A")
        git(d, "commit", "-q", "-m", msg)
    return d


RECIPE = repo("recipe-app", [
    ("Initial recipe search", [
        ("package.json", '{\n  "name": "recipe-app",\n  "version": "0.3.0",\n  "scripts": { "test": "vitest run", "dev": "vite" }\n}\n'),
        ("README.md", "# recipe-app\n\n材料と料理名からレシピを探す小さなアプリです。\n\n```bash\nnpm install\nnpm run dev\n```\n"),
        ("src/types.ts", "export interface Ingredient {\n  name: string;\n  amount?: string;\n}\n\nexport interface Recipe {\n  id: string;\n  title: string;\n  ingredients: Ingredient[];\n  minutes: number;\n}\n"),
        ("src/search.ts", "import type { Recipe } from './types';\n\nconst MAX = 200;\n\nexport function search(recipes: Recipe[], query: string): Recipe[] {\n  const q = query.trim().toLowerCase();\n  if (!q) return recipes.slice(0, MAX);\n  return recipes.filter((r) => r.title.toLowerCase().includes(q)).slice(0, MAX);\n}\n"),
        ("src/filters.ts", "import type { Recipe } from './types';\n\nexport function filterByTime(recipes: Recipe[], maxMinutes: number): Recipe[] {\n  return recipes.filter((r) => r.minutes <= maxMinutes);\n}\n"),
        ("tests/search.test.ts", "import { describe, it, expect } from 'vitest';\nimport { search } from '../src/search';\n\nconst recipes = [\n  { id: '1', title: '肉じゃが', ingredients: [{ name: 'じゃがいも' }], minutes: 40 },\n  { id: '2', title: 'ポテトサラダ', ingredients: [{ name: 'じゃがいも' }], minutes: 20 },\n];\n\ndescribe('search', () => {\n  it('matches by title', () => {\n    expect(search(recipes, 'ポテト')).toHaveLength(1);\n  });\n});\n"),
    ]),
    ("Add cooking-time filter", [
        ("src/App.tsx", "import { useState } from 'react';\nimport { search } from './search';\nimport { filterByTime } from './filters';\n\nexport function App({ recipes }) {\n  const [q, setQ] = useState('');\n  const [max, setMax] = useState(60);\n  const hits = filterByTime(search(recipes, q), max);\n  return (\n    <main>\n      <input value={q} onChange={(e) => setQ(e.target.value)} placeholder=\"料理名\" />\n      <input type=\"range\" min={5} max={120} value={max} onChange={(e) => setMax(Number(e.target.value))} />\n      <ul>{hits.map((r) => <li key={r.id}>{r.title}</li>)}</ul>\n    </main>\n  );\n}\n"),
    ]),
    ("Normalize ingredient names", [
        ("src/normalize.ts", "export function normalize(s: string): string {\n  return s.normalize('NFKC').trim().toLowerCase();\n}\n"),
    ]),
])
# Working-tree changes that the Git view and the diff tab will show.
write(os.path.join(RECIPE, "src/filters.ts"), "import type { Recipe } from './types';\nimport { normalize } from './normalize';\n\nexport function filterByTime(recipes: Recipe[], maxMinutes: number): Recipe[] {\n  return recipes.filter((r) => r.minutes <= maxMinutes);\n}\n\n/** 選んだ材料を 1 つでも含むレシピだけ残す。材料が空なら何もしない。 */\nexport function filterByIngredients(recipes: Recipe[], wanted: string[]): Recipe[] {\n  if (wanted.length === 0) return recipes;\n  const need = new Set(wanted.map(normalize));\n  return recipes.filter((r) => r.ingredients.some((i) => need.has(normalize(i.name))));\n}\n")
write(os.path.join(RECIPE, "src/ingredients.ts"), "import type { Recipe } from './types';\nimport { normalize } from './normalize';\n\n/** 全レシピに出てくる材料名を、出現回数の多い順に返す。 */\nexport function allIngredients(recipes: Recipe[]): string[] {\n  const count = new Map<string, number>();\n  for (const r of recipes) for (const i of r.ingredients) {\n    const k = normalize(i.name);\n    count.set(k, (count.get(k) ?? 0) + 1);\n  }\n  return [...count.entries()].sort((a, b) => b[1] - a[1]).map(([k]) => k);\n}\n")

DOCS = repo("docs-site", [
    ("Scaffold mkdocs site", [
        ("mkdocs.yml", "site_name: Recipe App Docs\nnav:\n  - Home: index.md\n  - Getting Started: getting-started.md\n"),
        ("docs/index.md", "# Recipe App Docs\n"),
        ("docs/getting-started.md", "# Getting Started\n\nInstall and run.\n"),
    ]),
])
INFRA = repo("infra", [
    ("Staging environment", [
        ("main.tf", 'terraform {\n  required_version = ">= 1.6"\n}\n\nresource "aws_s3_bucket" "assets" {\n  bucket = "recipe-app-assets-${var.env}"\n}\n'),
        ("variables.tf", 'variable "env" {\n  type    = string\n  default = "staging"\n}\n'),
    ]),
])

# ------------------------------------------------------------------ live pids
# Hidden processes stand in for running Claude Code sessions (pid liveness is all OYAKATA checks).
CREATE_NO_WINDOW = 0x08000000
pids = []
for _ in range(3):
    p = subprocess.Popen(["ping", "-t", "127.0.0.1"], creationflags=CREATE_NO_WINDOW, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    pids.append(p.pid)
write(os.path.join(ROOT, "pids.txt"), "\n".join(map(str, pids)) + "\n")

# ------------------------------------------------------------------ transcripts
NOW = datetime.now(timezone.utc)


def ts(minutes_ago):
    return (NOW - timedelta(minutes=minutes_ago)).strftime("%Y-%m-%dT%H:%M:%S.000Z")


def enc(path):
    return "".join("-" if c in ":\\/." else c for c in path)


def sid():
    return str(uuid.uuid4())


def session(cwd, lines, subagents=None):
    s = sid()
    pdir = os.path.join(CLAUDE, "projects", enc(cwd))
    os.makedirs(pdir, exist_ok=True)
    out = []
    for ln in lines:
        ln = dict(ln)
        ln.setdefault("sessionId", s)
        ln.setdefault("uuid", str(uuid.uuid4()))
        if ln["type"] in ("user", "assistant"):
            ln.setdefault("cwd", cwd)
            ln.setdefault("version", "2.1.293")
            ln.setdefault("gitBranch", "main")
        out.append(json.dumps(ln, ensure_ascii=False))
    write(os.path.join(pdir, s + ".jsonl"), "\n".join(out) + "\n")
    for agent_id, meta, alines in (subagents or []):
        adir = os.path.join(pdir, s, "subagents")
        write(os.path.join(adir, f"agent-{agent_id}.meta.json"), json.dumps(meta, ensure_ascii=False))
        write(os.path.join(adir, f"agent-{agent_id}.jsonl"), "\n".join(json.dumps({**l, "isSidechain": True, "sessionId": s, "agentId": agent_id}, ensure_ascii=False) for l in alines) + "\n")
    return s


def user(text, t, **kw):
    return {"type": "user", "timestamp": t, "message": {"role": "user", "content": text}, **kw}


def usage(ctx, out):
    return {"input_tokens": 4, "cache_creation_input_tokens": 2000, "cache_read_input_tokens": ctx, "output_tokens": out}


def assistant(content, t, stop="end_turn", ctx=120000, out=600, model="claude-opus-5-5", **kw):
    return {"type": "assistant", "timestamp": t, "effort": "high", "message": {"model": model, "role": "assistant", "stop_reason": stop, "content": content, "usage": usage(ctx, out)}, **kw}


def text(s):
    return {"type": "text", "text": s}


def tool(id_, name, inp):
    return {"type": "tool_use", "id": id_, "name": name, "input": inp}


def result(id_, t, content, err=False):
    return {"type": "user", "timestamp": t, "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id_, "content": content, "is_error": err}]}}


def title(s):
    return {"type": "ai-title", "aiTitle": s}


def mode(m):
    return {"type": "permission-mode", "permissionMode": m}


S1_ANSWER = """## 方針

既存の `search.ts` には手を入れず、検索結果の後ろにフィルタを 1 枚かませます。

| 案 | 変更範囲 | 既存 UI への影響 |
| --- | --- | --- |
| A. `search()` に引数を足す | 呼び出し元 6 箇所 | あり |
| B. 結果に後段フィルタをかける | `filters.ts` のみ | なし |
| C. インデックスを材料ごとに分ける | ビルド処理まで | あり |

**B で進めます。** 検索結果は最大 200 件なので後段で絞っても十分速く、UI 側は材料のチップを足すだけで済みます。

```mermaid
flowchart LR
  Q[検索語] --> S[search] --> R[候補 ≤200 件]
  I[選んだ材料] --> F
  R --> F[filterByIngredients] --> V[一覧]
```

```ts
export function filterByIngredients(recipes: Recipe[], wanted: string[]): Recipe[] {
  if (wanted.length === 0) return recipes;
  const need = new Set(wanted.map(normalize));
  return recipes.filter((r) => r.ingredients.some((i) => need.has(normalize(i.name))));
}
```

先に、既存のフィルタ UI とテストの構成をサブエージェントに調べてもらいます。"""

S1_SUB_A = [
    user("src/ 配下のフィルタ UI（時間フィルタ）がどこでどう使われているか、呼び出し箇所と state の持ち方をまとめて。", ts(9)),
    {"type": "agent-name", "agentName": "調査係"},
    assistant([tool("sa1", "Grep", {"pattern": "filterByTime", "path": "src"})], ts(9), stop="tool_use", ctx=30000, out=80, model="claude-sonnet-5-5"),
    result("sa1", ts(8), "src/App.tsx:3\nsrc/App.tsx:8\nsrc/filters.ts:3"),
    assistant([tool("sa2", "Read", {"file_path": os.path.join(RECIPE, "src", "App.tsx")})], ts(8), stop="tool_use", ctx=31000, out=60, model="claude-sonnet-5-5"),
    result("sa2", ts(8), "(App.tsx の内容)"),
    assistant([text("時間フィルタは `App.tsx` の `useState(60)` だけで持ち、`filterByTime(search(...), max)` の順に適用しています。材料フィルタも同じ場所に `useState<string[]>([])` を足し、`filterByIngredients` を最後に適用すれば既存の流れを崩しません。")], ts(7), ctx=33000, out=180, model="claude-sonnet-5-5"),
]
S1_SUB_B = [
    user("材料フィルタに必要なテストケースを洗い出して、tests/filters.test.ts の雛形を書いて。", ts(9)),
    {"type": "agent-name", "agentName": "試験係"},
    assistant([tool("sb1", "Read", {"file_path": os.path.join(RECIPE, "tests", "search.test.ts")})], ts(9), stop="tool_use", ctx=28000, out=50, model="claude-sonnet-5-5"),
    result("sb1", ts(8), "(search.test.ts の内容)"),
    assistant([tool("sb2", "Write", {"file_path": os.path.join(RECIPE, "tests", "filters.test.ts"), "content": "// ..."})], ts(6), stop="tool_use", ctx=29000, out=400, model="claude-sonnet-5-5"),
]

S1 = session(RECIPE, [
    mode("auto"),
    user("レシピ検索に「材料で絞り込む」機能を足して。既存の検索 UI は壊さないで。", ts(12)),
    title("レシピ検索に材料フィルタを追加"),
    assistant([text(S1_ANSWER),
               tool("tu_a1", "Agent", {"subagent_type": "Explore", "description": "既存のフィルタ UI を調査", "prompt": "src/ 配下のフィルタ UI がどこでどう使われているかをまとめて。"}),
               tool("tu_a2", "Agent", {"subagent_type": "general-purpose", "description": "テストケースを洗い出す", "prompt": "材料フィルタに必要なテストケースを洗い出して雛形を書いて。"})],
              ts(10), stop="tool_use", ctx=150000, out=900),
    result("tu_a1", ts(7), "時間フィルタは App.tsx の useState(60) だけで持ち、filterByTime(search(...), max) の順に適用しています。材料フィルタも同じ場所に足せば既存の流れを崩しません。"),
    assistant([text("調査の通り、`App.tsx` の流れはそのままにして `filters.ts` に関数を足します。"),
               tool("tu_e1", "Edit", {"file_path": os.path.join(RECIPE, "src", "filters.ts"), "old_string": "export function filterByTime", "new_string": "export function filterByIngredients(...)\n\nexport function filterByTime"}),
               tool("tu_w1", "Write", {"file_path": os.path.join(RECIPE, "src", "ingredients.ts"), "content": "// ..."})],
              ts(5), stop="tool_use", ctx=158000, out=700),
    result("tu_e1", ts(5), "The file has been updated."),
    result("tu_w1", ts(4), "File created successfully."),
    assistant([tool("tu_b1", "Bash", {"command": "npm test -- --run tests/filters.test.ts", "description": "Run the filter tests"})], ts(1), stop="tool_use", ctx=160000, out=60),
], subagents=[
    ("a1", {"agentType": "Explore", "description": "既存のフィルタ UI を調査", "toolUseId": "tu_a1", "requestShape": "foreground"}, S1_SUB_A),
    ("a2", {"agentType": "general-purpose", "description": "テストケースを洗い出す", "toolUseId": "tu_a2", "requestShape": "foreground"}, S1_SUB_B),
])

S2 = session(RECIPE, [
    mode("auto"),
    user("tests/search.test.ts が CI で時々落ちる。原因を調べて直して。", ts(60 * 26)),
    title("flaky なテストの修正"),
    assistant([text("落ちるのは `matches by title` ではなく、その前に走る `setup` の非同期待ちです。"), tool("t1", "Read", {"file_path": os.path.join(RECIPE, "tests", "search.test.ts")})], ts(60 * 26 - 1), stop="tool_use", ctx=40000, out=120),
    result("t1", ts(60 * 26 - 1), "(内容)"),
    assistant([text("## 原因\n\n`beforeAll` でインデックスを作る Promise を `await` していなかったため、速いマシンでは間に合い、CI では間に合わないことがありました。\n\n## 修正\n\n```ts\nbeforeAll(async () => {\n  index = await buildIndex(recipes);\n});\n```\n\n50 回連続で回して全て通ることを確認しました。")], ts(60 * 26 - 4), ctx=45000, out=300),
])

S5 = session(RECIPE, [
    mode("auto"),
    user("README.md を英語にして。日本語版は README.ja.md に残して。", ts(35)),
    title("README を英語化"),
    assistant([tool("r1", "Read", {"file_path": os.path.join(RECIPE, "README.md")})], ts(35), stop="tool_use", ctx=20000, out=40),
    result("r1", ts(34), "(内容)"),
    assistant([text("英語版を `README.md` に、元の日本語版を `README.ja.md` に置きました。冒頭に相互リンクを入れています。\n\n| ファイル | 内容 |\n| --- | --- |\n| `README.md` | English |\n| `README.ja.md` | 日本語（元の内容） |")], ts(30), ctx=24000, out=200),
])

S3 = session(DOCS, [
    mode("auto"),
    user("docs/getting-started.md を、Node を入れたことがない人向けに書き直して。", ts(60 * 5)),
    title("Getting Started を初心者向けに"),
    assistant([text("## 構成\n\n1. Node.js を入れる（nvm を推奨）\n2. リポジトリを取る\n3. `npm install` と `npm run dev`\n4. 最初のレシピを登録してみる\n\nつまずきやすい点を各手順の末尾に「うまくいかないときは」として足しました。"), tool("d1", "Write", {"file_path": os.path.join(DOCS, "docs", "getting-started.md"), "content": "# Getting Started\n..."})], ts(60 * 5 - 2), stop="tool_use", ctx=30000, out=500),
    result("d1", ts(60 * 5 - 2), "File created successfully."),
    assistant([text("書き直しました。`mkdocs serve` で確認できます。")], ts(60 * 5 - 3), ctx=31000, out=40),
])

S4 = session(INFRA, [
    mode("default"),
    user("staging の terraform plan を取って、差分を日本語で説明して。apply はしないで。", ts(3)),
    title("staging の plan 差分を確認"),
    assistant([text("先に `plan` を取ります。`apply` は実行しません。"), tool("p1", "Bash", {"command": "terraform plan -var env=staging -no-color", "description": "Show the staging plan"})], ts(2), stop="tool_use", ctx=12000, out=80),
])


def live(pid, s, cwd, status, name, **kw):
    now_ms = int(time.time() * 1000)
    d = {"pid": pid, "sessionId": s, "cwd": cwd, "status": status, "name": name, "kind": "interactive", "entrypoint": "cli",
         "startedAt": now_ms - 12 * 60000, "updatedAt": now_ms, "statusUpdatedAt": now_ms, "version": "2.1.293", **kw}
    write(os.path.join(CLAUDE, "sessions", f"{pid}.json"), json.dumps(d, ensure_ascii=False))


live(pids[0], S1, RECIPE, "busy", "レシピ検索に材料フィルタを追加")
live(pids[1], S4, INFRA, "waiting", "staging の plan 差分を確認", waitingFor="permission: Bash")
live(pids[2], S5, RECIPE, "idle", "README を英語化")

# ------------------------------------------------------------------ other agents
# The captured test fixtures of Codex CLI, Gemini CLI and Copilot CLI (tests/fixtures), placed
# where each agent keeps its sessions, with their working directory pointed at the demo repo.
# OpenCode keeps sessions in a database, so it is not part of the demo.
FIX = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))), "tests", "fixtures")
AGENTS = os.path.join(ROOT, "agents")


def retarget(text):
    return text.replace("C:\\\\Temp\\\\agents\\\\work", RECIPE.replace("\\", "\\\\")).replace("C:\\Temp\\agents\\work", RECIPE).replace("C:\\\\Temp\\\\agents", RECIPE.replace("\\", "\\\\")).replace("C:\\Temp\\agents", RECIPE)


def copy_tree(src, dst):
    for base, _, files in os.walk(src):
        for f in files:
            rel = os.path.relpath(os.path.join(base, f), src)
            with open(os.path.join(base, f), encoding="utf-8") as fh:
                write(os.path.join(dst, rel), retarget(fh.read()))


codex_home = os.path.join(AGENTS, "codex")
copy_tree(os.path.join(FIX, "codex"), os.path.join(codex_home, "sessions", "2026", "10", "08"))
gemini_home = os.path.join(AGENTS, "gemini", ".gemini")
copy_tree(os.path.join(FIX, "gemini"), os.path.join(gemini_home, "tmp", "recipe-app", "chats"))
write(os.path.join(gemini_home, "projects.json"), json.dumps({"projects": {RECIPE.lower(): "recipe-app"}}))
copilot_home = os.path.join(AGENTS, "copilot")
copy_tree(os.path.join(FIX, "copilot"), os.path.join(copilot_home, "session-state"))
write(os.path.join(ROOT, "agents-env.ps1"), "\n".join([
    f"$env:CODEX_HOME = '{codex_home}'",
    f"$env:HOME = '{os.path.dirname(gemini_home)}'",
    f"$env:USERPROFILE = '{os.path.dirname(gemini_home)}'",
    f"$env:COPILOT_HOME = '{copilot_home}'",
    f"$env:OYAKATA_HOME = '{os.path.join(ROOT, 'oyakata-home')}'",
]) + "\n")

write(os.path.join(ROOT, "ids.json"), json.dumps({"S1": S1, "S2": S2, "S3": S3, "S4": S4, "S5": S5, "recipe": RECIPE, "docs": DOCS, "infra": INFRA}, ensure_ascii=False, indent=1))
print("demo built at", ROOT, "pids", pids)
