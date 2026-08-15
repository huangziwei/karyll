#!/bin/sh
# The editor launcher, run by the home-screen tile (documents/Karyll.sh).
#
# Thin on purpose. karyll manages the Bluetooth stack itself over the daemon's
# local API — starting it, scanning, pairing, stopping it again — so there is no
# daemon logic here to drift out of step with the app. This script only handles
# what a shell is actually better at: not launching twice, and capturing output
# that would otherwise go nowhere.

EXT=/mnt/us/extensions/karyll
VAR="$EXT/var"
LOG="$VAR/karyll.log"
DOCS=/mnt/us/karyll

mkdir -p "$VAR" 2>/dev/null

log() { echo "[$(date)] $*" >> "$LOG"; }

# Hold the session against a second tap. Two editors would fight over an
# exclusive keyboard grab and over the daemon's lifetime.
LOCK="$VAR/karyll.pid"
if [ -f "$LOCK" ]; then
    OLD=$(cat "$LOCK" 2>/dev/null)
    if [ -n "$OLD" ] && [ -d "/proc/$OLD" ]; then
        log "already running (pid $OLD), ignoring launch"
        exit 0
    fi
    log "clearing stale lock (pid ${OLD:-unknown})"
fi
echo $$ > "$LOCK"

# Let the device sleep again on the way out. karyll holds powerd's
# `preventScreenSaver` for the session, because it grabs the keyboard and a
# grabbed key cannot reset the idle timer. The binary is built `panic = "abort"`
# and so skips its own cleanup on an abort, which would leave the Kindle unable
# to sleep after the editor is gone. This trap fires however the binary died.
trap 'rm -f "$LOCK"; lipc-set-prop com.lab126.powerd preventScreenSaver 0 2>/dev/null' EXIT INT TERM

# The most recently touched document, or the welcome one. Documents live
# outside the extension so replacing it on update cannot take them with it.
#
# **Only when there is nothing at all**, which is a fresh install and nothing
# else: a writer who deletes the welcome document has said what they think of
# it, and it must not come back the next time they empty the directory. It used
# to be an empty `draft.md`, which opened onto a blank page with nothing saying
# what any of the controls did.
DOC="$1"
if [ -z "$DOC" ]; then
    DOC=$(ls -t "$DOCS"/*.md 2>/dev/null | head -1)
    if [ -z "$DOC" ]; then
        DOC="$DOCS/Welcome.md"
        cp "$EXT/share/Welcome.md" "$DOC" 2>/dev/null || : > "$DOC"
    fi
fi

log "launch $(uname -m), document $DOC"
"$EXT/bin/karyll" "$DOC" >> "$LOG" 2>&1
log "exit=$?"
