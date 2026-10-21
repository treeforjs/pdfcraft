#!/usr/bin/env bash
# Generate hidden aliases so saved desktop associations keep launching the current app.
set -euo pipefail
PREFIX=${1:?usage: desktop-compat.sh staged-prefix}
APP_ID=io.github.treeforjs.pdfcraft
# Historical desktop identity, used exclusively for compatibility with existing installations.
LEGACY_APP_ID=ai.storyteller.pdfcraft
DESKTOP="$PREFIX/share/applications/$APP_ID.desktop"
if [ ! -f "$DESKTOP" ]; then
  echo "missing current desktop entry: $DESKTOP" >&2
  exit 1
fi
sed '/^NoDisplay=/d' "$DESKTOP" >"$PREFIX/share/applications/$LEGACY_APP_ID.desktop"
printf '\nNoDisplay=true\n' >>"$PREFIX/share/applications/$LEGACY_APP_ID.desktop"
for icon in "$PREFIX"/share/icons/hicolor/*/apps/"$APP_ID".*; do
  if [ -f "$icon" ]; then
    cp "$icon" "${icon%/*}/$LEGACY_APP_ID.${icon##*.}"
  fi
done
