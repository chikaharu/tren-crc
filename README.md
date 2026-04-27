# tren-crc

> **Wire-incompatible with `chikaharu/tren` v0.4.x and earlier.**
> The diagonal 32 bits of `Frame32` were repurposed from a per-row XOR
> parity to a single CRC-32C value. The wire layout (128 bytes,
> 32 × big-endian u32 slots, diagonal-as-checksum geometry) is byte-for-
> byte identical, but the *meaning* of the diagonal differs, so a
> `tren-crc` binary cannot exchange frames with a `tren` v0.4.x binary
> and vice versa.

This repository is a fork of `chikaharu/tren` at v0.4.0 (commit
`326bba2`), maintained as an experimental track for the diagonal-
CRC32C frame protocol. Upstream `chikaharu/tren` is left untouched;
integration of this variant back into upstream is a separate decision.

## Changelog

See **[CHANGELOG.md](./CHANGELOG.md)** for a history of notable changes in this fork.

---

## scheduler/ — tren-crc job scheduler (in-repo single source of truth)

このディレクトリはジョブスケジューラ `tren-crc` の **唯一の正本** です。
バイナリ (`tren-wrapper` / `qsub` / `qstat` / `qwait` / `qrun` / `qmap` /
`qbind` / `qclone` / `qowner` / `qworkdir` / `qlog` / `qwait-mark` / `qdel` /
`bench_idtree`) はすべて、このディレクトリ配下の Cargo crate からビルド
される。crate 名のみ `tren` → `tren-crc` に変更されており、ライブラリ名
(`tren`) と各バイナリ名はフォーク元と同一なので、import パス・PATH 互換
は維持される。

> 補足: 過去 (Task #14, commit `7fc412c5e`) にこのソースは
> `~/UTILITY/tren/` へ退避され、ここはシムだけになっていた。Task #21
> で agent の隔離環境からも触れるようにするため、その方針を撤回し、
> ソースをリポジトリ内に戻した。

---

## 使い方

```bash
# 毎セッション (PATH を通す + 必要なら自動ビルド)
source artifacts/bitrag/scheduler/env.sh

# 明示的に再ビルドしたい場合
cargo build --release --manifest-path artifacts/bitrag/scheduler/Cargo.toml

# ジョブ投入の例
qsub echo hello       # (cwd に .tren-<uuid>/ が無ければ自動生成)
qstat
qwait <jobid>
```

`env.sh` を source すると、

- `$TREN`        … このディレクトリの絶対パス
- `$TREN_HOME`   … `$TREN` と同値 (互換目的)
- `$PATH`        … `$TREN/target/release` を先頭に追加

がセットアップされる。`target/release/qsub` が無ければ初回 source 時に
`cargo build --release` を自動で走らせる。

## tren は PWD-local

中央デーモンは無い。最初の `qsub` が cwd 配下に `.tren-<uuid>/` を自動
生成し、`tren-wrapper` がそこで起動する。詳細は `src/lib.rs` 冒頭のコメ
ントおよび `tests/smoke.rs` を参照。

## edit → build → test → user の手元へ反映されるパス

このリポジトリが single source of truth なので、流れは単純:

1. `artifacts/bitrag/scheduler/src/...` を編集する (agent でも user でも)
2. `cargo build --release --manifest-path artifacts/bitrag/scheduler/Cargo.toml`
3. `cargo test --release --manifest-path artifacts/bitrag/scheduler/Cargo.toml`
4. commit & push → user 側 (および他 agent 隔離環境) は次回 pull / merge 時に
   そのまま新しいソースを取得する
5. user 側でも `source artifacts/bitrag/scheduler/env.sh` がそのまま動く。
   外部の `~/UTILITY/tren/` を別途 rebuild する必要は **無い**。

旧い `~/UTILITY/tren/` ツリーを使い続けると drift の元になる。残ってい
る場合は `unset TREN_HOME` した上でこの env.sh を source するのが安全。

## 残骸ファイルについて

このディレクトリには Task #14 以前の残骸がいくつか同居している
(`._qsched*-...` ディレクトリ群、`scheduler.sock`、`scheduler.serial`、
`logs/`、`target/` 配下の旧ビルド等)。これらは Task #21 のスコープ外で、
別タスクで掃除予定。`Cargo.toml` / `src/` / `tests/` だけが現行のソース
本体である。

---

## Architectural notes

### 対角 CRC-32C の規約

`Frame32` は 128 バイト固定 = 32 × u32 = 32 行 × 32 列のビット行列で、
**列番号 i (= ビット位置 i) と行番号 i (= スロット番号 i) が一致する
対角 32 ビット**を CRC のホストとして使う。具体的計算は Ethernet FCS
流の固定手順:

1. 対角 32 ビットを 0 クリアした 128 バイトを big-endian で書き出す
   (`slot_i.to_be_bytes()` を i=0..31 で連結)
2. その 128 バイトを **CRC-32C (Castagnoli, polynomial 0x1EDC6F41,
   reflect-input/output, init = xor-out = 0xFFFFFFFF)** にかける
3. 得られた 32 bit 値の bit `i` を、スロット `i` の bit `i` に書き戻す
   (LSB-first スキャタ; bit 0 → slot 0 bit 0, …, bit 31 → slot 31
   bit 31)

検出能力:

| 誤りパターン            | XOR 対角パリティ (旧)               | CRC-32C 対角 (新)         |
|-------------------------|-------------------------------------|---------------------------|
| 任意 1 ビット誤り       | 検出 (行内に限る)                   | **常に検出**              |
| 任意 2 ビット誤り       | 同行内は素通り                      | **常に検出**              |
| 任意 3 ビット誤り       | 行をまたぐと素通り                  | **常に検出 (1024 bit 内)** |
| 任意 奇数ビット誤り     | 行内のみ                            | **常に検出**              |
| 長さ ≤ 32 のバースト    | 行をまたぐと素通り                  | **常に検出**              |
| 対角ビット自身の誤り    | パリティの再計算と区別不能 → 素通り | **常に検出**              |

### CRC-32C を選んだ理由

- IEEE 802.3 (gzip / Ethernet 用、polynomial 0xEDB88320) と比較して
  **ハードウェア加速命令が x86 (`CRC32`) / ARMv8 (`CRC32CX`) で広く
  利用可能**。128 バイト 1 フレームで実測 ~数十 ns @ 3GHz、UDP
  send/recv オーバーヘッドに対して無視できる。
- 短いペイロード (≤ 約 5KB) における誤り検出能力が IEEE 802.3 より
  わずかに優れる。
- 新規プロトコルなので Ethernet 互換性は不要 → CRC-32C で問題なし。

### Op code 上位ビットによる将来のバージョンネゴシエーション

`Frame32` のスロット 0 (header) は op_code 8 bit + msg_seq 23 bit で
構成される。upstream Task #1 のメモにもある通り、**op_code の上位
ビットを将来のプロトコルバージョン識別子に予約**しておけば、対角 CRC
版と従来版の混在環境でも受信側が「自分には読めないフレーム」を即座
に区別できる。本タスクではフォーマット上の予約だけを意識し、ネゴ
シエーション処理そのものの実装は別タスク扱い。

### 互換性

- 旧 `tren` (v0.4.x 以前) のバイナリと `tren-crc` のバイナリは
  通信不能。各ホストで一方のみを稼働させること。
- ライブラリ名は `tren` のまま (rust 側の `use tren::Frame32` などの
  import パスはフォーク元と同一)。crate 名のみ `tren-crc` に変更
  されている。
- バイナリ名 (`tren-wrapper` / `qsub` 等) も同名のまま維持。`PATH`
  互換が必要な既存スクリプトはそのまま動く。

## ブランチ構成 / コミットリレー

このリポジトリは **`main → future → develop → experiment`** の 4 段
ブランチ構造を採り、日々の作業コミットは `develop` と `experiment`
で **厳密に交互 (alternating) に author** する規律で運用する。詳細
と pre-commit hook の挙動は **[`COMMIT_RULE.md`](./COMMIT_RULE.md)**
を参照。clone 直後に必ず `git config core.hooksPath .githooks` を
有効化すること。
