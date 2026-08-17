#!/bin/sh
set -eu

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

if [ -n "$(git status --porcelain)" ]; then
  echo "refusing to start M5 soaks from a dirty worktree" >&2
  exit 1
fi

binary="$repo_root/target/release/examples/task_runtime_soak"
if [ ! -x "$binary" ]; then
  echo "missing release soak binary: $binary" >&2
  echo "build it with: cargo build -p echo-agent-app-core --release --example task_runtime_soak --locked --offline" >&2
  exit 1
fi

start_soak() {
  hours="$1"
  output_dir="$repo_root/.eko/soak/m5-${hours}h"
  pid_file="$output_dir/process.pid"
  log_file="$output_dir/process.log"

  mkdir -p "$output_dir"
  if [ -f "$pid_file" ]; then
    existing_pid=$(tr -d '[:space:]' < "$pid_file")
    if [ -n "$existing_pid" ] && kill -0 "$existing_pid" 2>/dev/null; then
      echo "m5-${hours}h is already running as PID $existing_pid" >&2
      exit 1
    fi
  fi

  nohup "$binary" \
    --hours "$hours" \
    --output-dir "$output_dir" \
    > "$log_file" 2>&1 &
  soak_pid=$!
  printf '%s\n' "$soak_pid" > "$pid_file"
  echo "started m5-${hours}h PID $soak_pid"
}

start_soak 12
start_soak 24
start_soak 48
