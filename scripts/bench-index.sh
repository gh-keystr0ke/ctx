#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
    echo "Usage: $0 <ctx-binary> [revision]" >&2
    exit 2
fi

ctx="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
revision=${2:-dc99f75}
source_repo=$(git rev-parse --show-toplevel)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
git clone -q --shared "$source_repo" "$scratch/repo"
cd "$scratch/repo"
git checkout -q --detach "$revision"
git remote set-url origin "$(git -C "$source_repo" remote get-url origin)"
mkdir -p .ctx
cp "$source_repo/.ctx/config.toml" .ctx/config.toml

python3 - "$ctx" "$revision" <<'PY'
import pathlib
import statistics
import subprocess
import sys
import time

ctx, revision = sys.argv[1:]
samples = {label: [] for label in ("full index", "incremental index", "review HEAD~20")}

def git(*args):
    subprocess.run(["git", *args], check=True, stdout=subprocess.DEVNULL)

def measure(label, *args):
    start = time.perf_counter()
    subprocess.run([ctx, *args], check=True, stdout=subprocess.DEVNULL)
    samples[label].append(time.perf_counter() - start)

for _ in range(5):
    for path in pathlib.Path(".ctx").glob("ctx.db*"):
        path.unlink()
    git("checkout", "-q", "--detach", revision + "^")
    subprocess.run([ctx, "init"], check=True, stdout=subprocess.DEVNULL)
    measure("full index", "index")
    git("checkout", "-q", "--detach", revision)
    measure("incremental index", "index")
    measure("review HEAD~20", "review", "--base", "HEAD~20")

for label, times in samples.items():
    print(f"{label}: {statistics.median(times):.3f} s")
PY
