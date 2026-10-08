# README のスクリーンショットを撮り直す

`docs/images/*.png` は、ダミーのセッションとリポジトリで撮っています。実在のユーザー名・パス・リポジトリは写りません。Windows 向けです（Chrome と Node.js 22 以上が必要。npm パッケージは使いません）。

```powershell
# 1. ダミーデータを C:\Temp\oyakata-demo に作る（git リポジトリ 3 つ、セッション 5 つ、稼働中の代わりの ping プロセス 3 つ）
python -I docs\screenshots\gen.py

# 2. そのデータで OYAKATA を別ポートに立てる（HOME も差し替えて、自分の ghq リポジトリが混ざらないようにする）
$env:HOME = 'C:\Temp\oyakata-demo\home'; $env:USERPROFILE = $env:HOME
oyakata serve --no-open --port 4877 --claude-dir C:\Temp\oyakata-demo\claude

# 3. 別のターミナルでヘッドレス Chrome から撮る（hero / team / workbench / new / dark）
node docs\screenshots\shoot.mjs http://127.0.0.1:4877 C:\Temp\oyakata-demo\ids.json C:\Temp\oyk-shot\out

# 4. 片付け
oyakata stop --port 4877
Get-Content C:\Temp\oyakata-demo\pids.txt | ForEach-Object { Stop-Process -Id $_ -Force }
Remove-Item -Recurse -Force C:\Temp\oyakata-demo, C:\Temp\oyk-shot
```

撮れた PNG を `docs/images/` にコピーします。`STEPS=hero,team` のように環境変数で一部だけ撮り直せます。
