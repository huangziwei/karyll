#!/bin/sh
# Regenerate the home-screen tile's cover art (documents/Karyll.sh).
#
# The jailbreak hotfix indexes a `documents/*.sh` scriptlet as a library tile
# and draws whatever PNG its `# Icon:` header carries as a base64 data URI. That
# header is one line tens of kilobytes long, so it is generated rather than
# hand-edited.
#
# Pipeline: assets/cover.svg -> rsvg-convert -> pngquant -> assets/cover.png ->
# base64 -> the `# Icon:` line. Both tools are optional; without them the script
# degrades in the order below and says which step it skipped.
#
# This rewrites ONLY the `# Icon:` line. The shebang, the other headers and the
# script body are left untouched, so the scriptlet stays a normal file you can
# edit by hand — run this after changing the cover, not after changing the body.
#
# Usage: device/make-tile.sh
set -eu

cd "$(dirname "$0")"

SVG="assets/cover.svg"
PNG="assets/cover.png"
TILE="documents/Karyll.sh"

# Kindle library-tile cover, matching the committed PNG. Other dimensions still
# render, just blurry or letterboxed in the home grid.
WIDTH=1440
HEIGHT=2200

[ -f "$TILE" ] || { echo "error: $TILE not found (run me from the repo)" >&2; exit 1; }
grep -q '^# Icon: ' "$TILE" || {
    echo "error: $TILE has no '# Icon:' line to replace" >&2
    exit 1
}

if [ -f "$SVG" ] && command -v rsvg-convert >/dev/null 2>&1; then
    echo "==> Rendering $SVG -> $PNG (${WIDTH}x${HEIGHT})"
    rsvg-convert -w "$WIDTH" -h "$HEIGHT" -o "$PNG" "$SVG"
    # Quantize to an 8-bit palette. The tile ships inline as base64 inside a
    # file pushed over USB, so the PNG's size is the scriptlet's size. The cover
    # is three flat values plus the greys along their edges, which is a handful
    # of palette entries — the truecolour render is pure waste here.
    if command -v pngquant >/dev/null 2>&1; then
        pngquant --force --skip-if-larger --output "$PNG" -- "$PNG"
    else
        echo "    note: pngquant not installed — cover stays truecolour and the"
        echo "          tile grows by several times"
        echo "          (install with: brew install pngquant)"
    fi
else
    echo "==> Skipping the SVG render; reusing the committed $PNG"
    command -v rsvg-convert >/dev/null 2>&1 ||
        echo "    (rsvg-convert not installed: brew install librsvg)"
fi

[ -f "$PNG" ] || { echo "error: no $PNG to embed" >&2; exit 1; }

# `base64` differs across platforms: BSD/macOS emits one line, GNU wraps at 76
# columns unless given -w0. A wrapped icon would break the single-line header,
# so strip newlines either way.
ICON="$(base64 < "$PNG" | tr -d '\n')"

TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT
# awk, not sed: the replacement line is tens of kilobytes and sed's line buffer
# chokes on it.
awk -v icon="$ICON" '
    /^# Icon: / { print "# Icon: data:image/png;base64," icon; next }
    { print }
' "$TILE" > "$TMP"
cat "$TMP" > "$TILE"

echo "==> Embedded $(wc -c < "$PNG" | tr -d ' ')-byte cover; $TILE is now $(wc -c < "$TILE" | tr -d ' ') bytes"
echo "    Copy documents/Karyll.sh to /mnt/us/documents/ over MTP or USB."
