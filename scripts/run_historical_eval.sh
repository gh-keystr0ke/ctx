#!/usr/bin/env bash
# Replays real historical commits of this repository through the release
# `ctx` binary to produce a historical-PR-style evaluation corpus: index at
# each case's parent commit (using .context exactly as it was committed at
# that point in history), then run `ctx review --base <parent>` against the
# working tree checked out to the case's commit. Output is raw JSON per case;
# grading against hand-written ground truth happens separately.
set -euo pipefail

REPO_SRC="$(git -C "$(dirname "${BASH_SOURCE[0]}")/.." rev-parse --show-toplevel)"
CORPUS_FILE="$REPO_SRC/scripts/historical-corpus.tsv"
BIN="$REPO_SRC/target/release/ctx"
OUT_DIR="$REPO_SRC/docs/historical-eval-results"

if [[ ! -x "$BIN" ]]; then
  echo "release binary not found at $BIN; run 'cargo build --locked --workspace --release' first" >&2
  exit 1
fi

CLONE_DIR="$(mktemp -d)"
trap 'rm -rf "$CLONE_DIR"' EXIT

git clone --quiet "$REPO_SRC" "$CLONE_DIR"
mkdir -p "$OUT_DIR"

while IFS=$'\t' read -r id parent commit category message; do
  [[ "$id" == \#* || -z "$id" ]] && continue
  echo "=== $id ($category): $message ==="

  git -C "$CLONE_DIR" checkout --force --quiet "$parent"
  rm -f "$CLONE_DIR/.ctx/ctx.db" "$CLONE_DIR/.ctx/ctx.db-wal" "$CLONE_DIR/.ctx/ctx.db-shm"

  (cd "$CLONE_DIR" && "$BIN" --json init) > "$OUT_DIR/${id}.init.json"
  if ! (cd "$CLONE_DIR" && "$BIN" --json index) > "$OUT_DIR/${id}.index-parent.json" 2>"$OUT_DIR/${id}.index-parent.stderr"; then
    echo "  index at parent FAILED, see ${id}.index-parent.stderr"
    continue
  fi
  (cd "$CLONE_DIR" && "$BIN" --json status) > "$OUT_DIR/${id}.status-parent.json" || true

  git -C "$CLONE_DIR" checkout --force --quiet "$commit"
  if (cd "$CLONE_DIR" && "$BIN" --json review --base "$parent") > "$OUT_DIR/${id}.review.json" 2>"$OUT_DIR/${id}.review.stderr"; then
    findings=$(python3 -c "import json;d=json.load(open('$OUT_DIR/${id}.review.json'));print(len(d.get('findings',[])))" 2>/dev/null || echo "?")
    schema=$(python3 -c "import json;d=json.load(open('$OUT_DIR/${id}.review.json'));print(len(d.get('schema_findings',[])))" 2>/dev/null || echo "0")
    echo "  review: $findings findings, $schema schema_findings"
  else
    echo "  review FAILED, see ${id}.review.stderr"
  fi
done < "$CORPUS_FILE"

echo "Raw results written to $OUT_DIR"
