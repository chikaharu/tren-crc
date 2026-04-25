#!/usr/bin/env bash
# scheduler/env.sh — このリポジトリ内の tren crate を有効化する
#
# 方針 (Task #21):
#   tren ソースは `artifacts/bitrag/scheduler/` を single source of truth と
#   する。`~/UTILITY/tren/` のような外部パスへのシムは廃止した。
#   こうすることで task agent の隔離環境 (リポジトリのコピーしか見えない)
#   でも `cargo build --release` → `qsub` 等が即座に使えるようになる。
#
# Usage:
#   source artifacts/bitrag/scheduler/env.sh
#
# 効果:
#   - $TREN          : このリポジトリ内 `artifacts/bitrag/scheduler/` の絶対パス
#   - $TREN_HOME     : $TREN と同値 (互換目的)
#   - $PATH          : $TREN/target/release を先頭に追加
#                       → qsub / qstat / qwait / qrun / qmap / qbind / qclone /
#                         qowner / qworkdir / qlog / qwait-mark / qdel /
#                         tren-wrapper / bench_idtree がフルパス無しで使える
#
# 初回 source 時に target/release/qsub が無ければ
# `cargo build --release` を自動で走らせる。明示的に再ビルドしたい場合は
# 直接 `cargo build --release --manifest-path "$TREN/Cargo.toml"` でよい。
#
# tren は PWD-local: 中央デーモンは無く、最初の qsub が cwd 配下に
# .tren-<uuid>/ を自動生成する (詳細は scheduler/README.md)。

# このスクリプト自身の絶対ディレクトリを解決する
_TREN_SELF="${BASH_SOURCE[0]:-$0}"
if [ -z "$_TREN_SELF" ]; then
    echo "[scheduler/env.sh] ERROR: BASH_SOURCE が解決できません" >&2
    return 1 2>/dev/null || exit 1
fi
_TREN_DIR="$(cd "$(dirname "$_TREN_SELF")" && pwd)"
unset _TREN_SELF

if [ ! -f "$_TREN_DIR/Cargo.toml" ]; then
    echo "[scheduler/env.sh] ERROR: $_TREN_DIR/Cargo.toml が見つかりません" >&2
    echo "[scheduler/env.sh]   このディレクトリは tren crate ではありません。" >&2
    unset _TREN_DIR
    return 1 2>/dev/null || exit 1
fi

export TREN="$_TREN_DIR"
export TREN_HOME="$_TREN_DIR"

# target/release/qsub が無ければ自動ビルド (初回のみ)
if [ ! -x "$_TREN_DIR/target/release/qsub" ]; then
    echo "[scheduler/env.sh] target/release/qsub が無いので cargo build --release を実行します..." >&2
    if ! ( cd "$_TREN_DIR" && cargo build --release >&2 ); then
        echo "[scheduler/env.sh] ERROR: cargo build --release に失敗しました" >&2
        unset _TREN_DIR
        return 1 2>/dev/null || exit 1
    fi
fi

case ":$PATH:" in
    *":$_TREN_DIR/target/release:"*) : ;;
    *) export PATH="$_TREN_DIR/target/release:$PATH" ;;
esac

unset _TREN_DIR
