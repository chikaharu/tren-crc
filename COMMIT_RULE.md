# Commit Relay Rule (`develop` ⇄ `experiment`)

このリポジトリ (`chikaharu/tren-crc`) では、フォーク元 `chikaharu/tren` の
4 段ブランチ (`main → future → develop → experiment`) をそのまま受け継ぎ、
日々の作業コミットを **`develop` と `experiment` で厳密に交互 (alternating)
に author する** リレー運用を行う。

## なぜ交互にするか

- どちらか片方のブランチが「正本」になることを構造的に防止する
- ある時点でのソースが必ず両方のブランチで観察されるため、片方だけ進行
  して片方が腐るという状況が発生しない
- レビュー時に「どちらに最新があるか」を考えなくて済む

## 厳格ルール

1. **作業コミットは `develop` と `experiment` でのみ author する**。
   `main` / `future` 上で `git commit` してはならない (リリース節目の
   PR マージは例外、後述)。
2. **連続 2 コミットを同じブランチで author してはならない**。直前
   コミットが `develop` で author されていたら、次は必ず `experiment`
   で author し、その逆も同じ。
3. 全ての作業コミットメッセージの末尾に
   `Authored-on: <branch>` トレーラを付ける (値は `develop` か
   `experiment`)。
4. リレー転送は `git merge --ff-only` のみを用いる。merge commit や
   rebase は作業コミット間では使わない。

## 推奨手順 (1 サイクル)

```bash
# 1) develop で作業開始
git checkout develop
$EDITOR src/...
git add -A
git commit -m "subject

Body...

Authored-on: develop"

# 2) experiment へ ff-only で転送
git checkout experiment
git merge --ff-only develop

# 3) experiment 上で次のコミットを作る
$EDITOR src/...
git add -A
git commit -m "subject

Body...

Authored-on: experiment"

# 4) develop へ ff-only で転送
git checkout develop
git merge --ff-only experiment

# … 以下繰り返し
```

## リリース節目の昇格 (例外: 通常 PR マージ)

作業コミットの完成後、以下を **PR マージ (`--no-ff`)** で昇格する。
これらは「リレー」のカウントには含まれない。

```bash
git checkout future
git merge --no-ff develop -m "Promote vX.Y.Z from develop to future"

git checkout main
git merge --no-ff future -m "Release vX.Y.Z"
```

GitHub 上では `develop → future → main` の順に PR を作りマージしても
良い。

## アンチパターン

- ❌ `experiment` で 2 連続 author する (1, 2, 3 違反)
- ❌ `Authored-on:` トレーラを忘れる (3 違反、hook が reject)
- ❌ `main` や `future` で作業コミットを直接作る (1 違反)
- ❌ `develop ⇄ experiment` 間を `--no-ff` でマージする (4 違反、
  hook は素通りするが運用違反)
- ❌ `experiment → develop` 方向の rebase / cherry-pick で履歴を
  作り変える (履歴の交互パターンを破壊する)

## 強制機構

- リポジトリの `.githooks/` 配下に `pre-commit` と `commit-msg` を
  置き、`git config core.hooksPath .githooks` でリポジトリ単位に有効化
  している (clone 直後に各自の作業ツリーで有効化が必要)。
- `pre-commit` は HEAD ブランチが `develop`/`experiment` のとき、
  `HEAD^` のメッセージから `Authored-on:` トレーラを読み、現在の
  ブランチと一致したら reject する (連続 author 防止)。
- `commit-msg` は HEAD ブランチが `develop`/`experiment` のとき、
  作成しようとしているコミットメッセージに `Authored-on: <現在の
  ブランチ名>` が含まれているか確認し、無ければ reject する。
- GitHub 側でも `experiment` ブランチへの直接 push を branch
  protection で禁止することを推奨 (admin であっても push 不可、
  リレー規律をサーバ側でも担保)。

## 初期セットアップ (clone 直後 1 回)

```bash
git clone git@github.com:chikaharu/tren-crc.git
cd tren-crc
git config core.hooksPath .githooks
```

`core.hooksPath` はリポジトリ毎の設定なので clone する度に必要。
