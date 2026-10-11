#!/usr/bin/env bash
# Exercise installed binaries on an existing X11/Wayland display, with isolated settings.
# Usage: xvfb-run -a dbus-run-session -- packaging/linux/smoke-test.sh [pdfcraft] [pdfcraft-cli]
# Requires jq. Set PDFCRAFT_SMOKE_OUTPUT to retain screenshots and diagnostics for inspection.
set -euo pipefail
umask 077

APP="$(command -v "${1:-pdfcraft}")"
CLI="$(command -v "${2:-pdfcraft-cli}")"
WORK="$(mktemp -d -t pdfcraft-smoke.XXXXXXXX)"
APP_PID=
cleanup() {
  local status=$?
  if [ -n "$APP_PID" ]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  if (( status != 0 )); then
    cat "$WORK/desktop.log" "$WORK/control-error.log" 2>/dev/null || true
  fi
  # Never retain the control token. Only synthetic test files and logs leave the temp directory.
  if [ -n "${PDFCRAFT_SMOKE_OUTPUT:-}" ]; then
    if mkdir -p "$PDFCRAFT_SMOKE_OUTPUT"; then
      for file in desktop.png page.png state.json desktop.log; do
        if [ -f "$WORK/$file" ]; then cp "$WORK/$file" "$PDFCRAFT_SMOKE_OUTPUT/" || status=1; fi
      done
    else
      status=1
    fi
  fi
  rm -rf "$WORK" || status=1
  exit "$status"
}
trap cleanup EXIT
export XDG_CONFIG_HOME="$WORK/config" XDG_DATA_HOME="$WORK/data" XDG_CACHE_HOME="$WORK/cache"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$WORK/runtime}"
export PDFCRAFT_SYSTEM_FONTS=0
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_RUNTIME_DIR"
"$APP" --version
"$CLI" --version
cat >"$WORK/create.json" <<'JSON'
[
  {"tool":"doc_create","args":{"from":"text","text":"PdfCraft Ubuntu smoke test"}},
  {"tool":"doc_save","args":{"doc":1,"path":"Ubuntu smoke.pdf"}},
  {"tool":"page_render","args":{"doc":1,"page":1,"dpi":72},"out":"page.png"}
]
JSON
"$CLI" run --script "$WORK/create.json" --root "$WORK"
test -s "$WORK/page.png"
"$CLI" text "$WORK/Ubuntu smoke.pdf" | grep -Fq 'PdfCraft Ubuntu smoke test'

# GNOME Files sends file: URIs, including escaped spaces, through the desktop entry's %U.
"$APP" --control "$WORK/control.json" "file://$WORK/Ubuntu%20smoke.pdf" >"$WORK/desktop.log" 2>&1 &
APP_PID=$!
deadline=$((SECONDS + 60))
while :; do
  kill -0 "$APP_PID"
  if "$CLI" ui --control "$WORK/control.json" state >"$WORK/state.json" 2>"$WORK/control-error.log" \
    && jq -e '(.documents | length == 1) and .active.raster_memory.page_count == 1 and .active.raster_memory.pending_requests == 0' "$WORK/state.json" >/dev/null; then
    break
  fi
  if (( SECONDS >= deadline )); then
    echo "PdfCraft did not open the smoke document within 60 seconds." >&2
    exit 1
  fi
  sleep 1
done
"$CLI" ui --control "$WORK/control.json" screenshot --out "$WORK/desktop.png"
test -s "$WORK/desktop.png"
"$CLI" ui --control "$WORK/control.json" state >"$WORK/state.json"
jq -e '.documents[0].pages == 1 and .documents[0].dirty == false and (.active.page_errors | length == 0)' "$WORK/state.json" >/dev/null
echo "ok: PDF create/save/reopen/text/render, desktop URI launch, control channel, and screenshot"
