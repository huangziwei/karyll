#!/bin/sh
# Regenerate the home-screen tile's cover art: assets/cover.svg -> rsvg-convert
# -> pngquant -> base64 -> the `# Icon:` header of documents/Karyll.sh.
# **Rewrites ONLY that line.** Both tools are optional.
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
    # Quantize to an 8-bit palette: the PNG ships inline as base64, so its size
    # is the scriptlet's, and the cover is three flat values plus edge greys.
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
