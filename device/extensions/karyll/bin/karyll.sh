#!/bin/sh
# The editor launcher, run by the home-screen tile (documents/Karyll.sh). Thin
# on purpose: karyll manages the Bluetooth stack itself, so this only handles
# not launching twice and capturing output that would otherwise go nowhere.

EXT=/mnt/us/extensions/karyll
VAR="$EXT/var"
LOG="$VAR/karyll.log"
DOCS=/mnt/us/karyll

# Both, and both every time. The documents directory is outside the extension so
# that an update cannot take a draft with it, which also means nothing else
# creates it.
mkdir -p "$VAR" "$DOCS" 2>/dev/null

log() { echo "[$(date)] $*" >> "$LOG"; }

# **A tap on the tile replaces whatever is running.** Two editors would fight
# over the keyboard grab and the daemon, and refusing the launch is worse: the
# framework can take the screen while karyll runs on behind the home screen.
LOCK="$VAR/karyll.pid"
if [ -f "$LOCK" ]; then
    OLD=$(cat "$LOCK" 2>/dev/null)
    if [ -n "$OLD" ] && [ -d "/proc/$OLD" ]; then
        log "already running (pid $OLD), replacing it"
        # **The lock is claimed before the kill**, or the launcher being
        # replaced asks for its origin screen and puts it over the editor that
        # replaced it. `$$` stands in until the new editor exists.
        echo $$ > "$LOCK"
        kill "$OLD" 2>/dev/null
        # The editor catches the signal and leaves through the same door as
        # `[ Exit ]` — the document written, the daemon stopped, the screen let
        # go of — so wait for it to be gone rather than racing its shutdown.
        i=0
        while [ -d "/proc/$OLD" ] && [ "$i" -lt 5 ]; do
            sleep 1
            i=$((i + 1))
        done
        [ -d "/proc/$OLD" ] && kill -9 "$OLD" 2>/dev/null
    else
        log "clearing stale lock (pid ${OLD:-unknown})"
    fi
fi

# **The lock names the editor, not this shell.** A foreground child never
# receives the shell's signals, so the editor is started in the background and
# the lock names it, which makes `kill` and `/proc/$PID` reach the right process.
PID=$$
echo "$PID" > "$LOCK"

# The screen the tap came from, asked for once the editor is gone: the app
# manager keeps no history for a `documents/` scriptlet, so the tile carries it
# in. `startView` takes `<view_name>:<layer>:<app_uri>`, layer 0 being the top.
land() {
    log "origin ${KARYLL_ORIGIN_VIEW:-none}"
    case "${KARYLL_ORIGIN_VIEW:-}" in
        KPP_*|LEGACY_*) ;;
        *) return 0 ;;
    esac
    lipc-set-prop com.lab126.appmgrd startView \
        "$KARYLL_ORIGIN_VIEW:0:app://com.lab126.KPPMainApp?view=$KARYLL_ORIGIN_VIEW" \
        2>/dev/null
}

# Let the device sleep again on the way out: karyll holds `preventScreenSaver`
# for the session and is built `panic = "abort"`, so it can die without its own
# cleanup. **The lock and the landing go only if the lock is still ours.**
trap 'if [ "$(cat "$LOCK" 2>/dev/null)" = "$PID" ]; then rm -f "$LOCK"; land; fi; lipc-set-prop com.lab126.powerd preventScreenSaver 0 2>/dev/null' EXIT

# **Passed on, not swallowed.** The framework signals the launcher, and the
# editor is the one that can save and let go of the window; nothing here may
# exit on its own account.
trap '[ "$PID" != "$$" ] && kill "$PID" 2>/dev/null' INT TERM

# The most recently touched document, or the welcome one on a fresh install.
# **Found by glob and `-nt`, never by `ls`**: BusyBox `ls` has no Unicode support
# and prints a `?` for every byte above 0x7F, so a CJK name comes back unusable.
DOC="$1"
if [ -z "$DOC" ]; then
    for f in "$DOCS"/*.md; do
        # The literal pattern, when the directory holds nothing to match it.
        [ -e "$f" ] || continue
        if [ -z "$DOC" ] || [ "$f" -nt "$DOC" ]; then
            DOC="$f"
        fi
    done
    if [ -z "$DOC" ]; then
        DOC="$DOCS/Welcome.md"
        cp "$EXT/share/Welcome.md" "$DOC" 2>/dev/null || : > "$DOC"
    fi
fi

# **Which binary this Kindle can start, asked rather than assumed.** One is
# shipped per ARM float ABI, and the wrong one fails as `not found` — the shell
# reporting the missing interpreter, which reads like an absent binary.
BIN="$EXT/bin/karyll"
if [ ! -e /lib/ld-linux-armhf.so.3 ]; then
    BIN="$EXT/bin/karyll-softfloat"
    log "no hard-float loader here, using $BIN"
fi

log "launch $(uname -m), document $DOC"
"$BIN" "$DOC" >> "$LOG" 2>&1 &
PID=$!
echo "$PID" > "$LOCK"
wait "$PID"
STATUS=$?

# A trapped signal returns from `wait` while the editor is still on its way out,
# and the next tap watches this lock for it to be gone.
while [ -d "/proc/$PID" ]; do
    sleep 1
    wait "$PID"
    STATUS=$?
done
log "exit=$STATUS"
