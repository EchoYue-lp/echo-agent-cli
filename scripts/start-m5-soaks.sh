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

launchctl_bin=$(command -v launchctl || true)
if [ -z "$launchctl_bin" ]; then
  echo "launchctl is required so soak processes survive the launching terminal" >&2
  exit 1
fi
launch_domain="gui/$(id -u)"

start_soak() {
  hours="$1"
  output_dir="$repo_root/.eko/soak/m5-${hours}h"
  pid_file="$output_dir/process.pid"
  log_file="$output_dir/process.log"
  error_log="$output_dir/process.err.log"
  label="com.eko.m5-soak-${hours}h"
  service_target="$launch_domain/$label"

  mkdir -p "$output_dir"
  if "$launchctl_bin" print "$service_target" >/dev/null 2>&1; then
    echo "m5-${hours}h launch service is already loaded as $label" >&2
    exit 1
  fi
  if [ -f "$pid_file" ]; then
    existing_pid=$(tr -d '[:space:]' < "$pid_file")
    if [ -n "$existing_pid" ] && kill -0 "$existing_pid" 2>/dev/null; then
      echo "m5-${hours}h is already running as PID $existing_pid" >&2
      exit 1
    fi
  fi

  # Positional parameters expand in launchd's child shell, not in this launcher.
  # shellcheck disable=SC2016
  "$launchctl_bin" submit \
    -l "$label" \
    -o "$log_file" \
    -e "$error_log" \
    -- /bin/sh -c 'cd "$1" && exec "$2" --hours "$3" --output-dir "$4"' \
    eko-m5-soak "$repo_root" "$binary" "$hours" "$output_dir"

  attempts=0
  soak_pid=""
  while [ "$attempts" -lt 50 ] && [ -z "$soak_pid" ]; do
    service_state=$("$launchctl_bin" print "$service_target" 2>/dev/null || true)
    soak_pid=$(printf '%s\n' "$service_state" | awk '$1 == "pid" && $2 == "=" { print $3; exit }')
    if [ -z "$soak_pid" ]; then
      sleep 0.1
    fi
    attempts=$((attempts + 1))
  done
  if [ -z "$soak_pid" ]; then
    echo "m5-${hours}h launch service did not report a PID; inspect $error_log" >&2
    exit 1
  fi
  printf '%s\n' "$soak_pid" > "$pid_file"
  printf '%s\n' "$label" > "$output_dir/process.label"
  echo "started m5-${hours}h PID $soak_pid as $label"
}

start_soak 12
start_soak 24
start_soak 48
