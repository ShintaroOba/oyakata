# README のスクリーンショットを撮り直す / Regenerating the README screenshots

`docs/images/*.png` は、ダミーのセッションとリポジトリで撮っています。実在のユーザー名・パス・リポジトリは写りません。Windows 向けです（Chrome と Node.js 22 以上が必要。npm パッケージは使いません）。

The images are taken against dummy sessions and repositories, so no real user names, paths or repositories appear. Windows only (needs Chrome and Node.js 22+, no npm packages).

```powershell
# 1. ダミーデータを C:\Temp\oyakata-demo に作る（git リポジトリ 3 つ、Claude Code のセッション 5 つ、
#    tests/fixtures の Codex / Gemini / Copilot セッション、稼働中の代わりの ping プロセス 3 つ）
python -I docs\screenshots\gen.py

# 2. そのデータで OYAKATA を別ポートに立てる。agents-env.ps1 が各エージェントのデータ置き場を
#    ダミーに向け、HOME も差し替えて自分の ghq リポジトリが混ざらないようにする
. C:\Temp\oyakata-demo\agents-env.ps1
oyakata serve --no-open --port 4877 --claude-dir C:\Temp\oyakata-demo\claude

# 3. 別のターミナルでヘッドレス Chrome から撮る（hero / team / workbench / new / dark）
node docs\screenshots\shoot.mjs http://127.0.0.1:4877 C:\Temp\oyakata-demo\ids.json C:\Temp\oyk-shot\out

# 4. 片付け
oyakata stop --port 4877
Get-Content C:\Temp\oyakata-demo\pids.txt | ForEach-Object { Stop-Process -Id $_ -Force }
Remove-Item -Recurse -Force C:\Temp\oyakata-demo, C:\Temp\oyk-shot
```

撮れた PNG を `docs/images/` にコピーします。`STEPS=hero,team` のように環境変数で一部だけ撮り直せます。英語 UI で撮るには、手順 2 の前に `C:\Temp\oyakata-demo\oyakata-home\config.json` に `{"lang":"en"}` を置きます。

Copy the PNGs into `docs/images/`. `STEPS=hero,team` re-shoots a subset. For the English UI, put `{"lang":"en"}` in `C:\Temp\oyakata-demo\oyakata-home\config.json` before step 2.
