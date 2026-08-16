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

# Both, and both every time. The documents directory is outside the extension
# so that an update cannot take a draft with it, which also means nothing else
# creates it: on a Kindle karyll has never run on, it is not there, and then the
# welcome document cannot be written and the editor opens a path whose directory
# does not exist.
mkdir -p "$VAR" "$DOCS" 2>/dev/null

log() { echo "[$(date)] $*" >> "$LOG"; }

# **A tap on the tile replaces whatever is running.** Two editors at once would
# fight over an exclusive keyboard grab and over the daemon's lifetime, so only
# one may live — but refusing the launch is the worse half of that: the
# framework can take the screen back from karyll while it goes on running
# behind the home screen, and pairing a keyboard is exactly when it does,
# because the Bluetooth daemon kills `bsa_server` out from under it. A tile that
# does nothing then is a writer with no way back to their page.
#
# What is lost by killing is the last couple of seconds of typing that autosave
# has not reached, which by the time someone has walked to the library and
# tapped a tile is nothing. A second tap by mistake costs a restart, which is
# the cheaper mistake of the two.
LOCK="$VAR/karyll.pid"
if [ -f "$LOCK" ]; then
    OLD=$(cat "$LOCK" 2>/dev/null)
    if [ -n "$OLD" ] && [ -d "/proc/$OLD" ]; then
        log "already running (pid $OLD), replacing it"
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

# **The lock names the editor, not this shell.** A signal to the shell is not a
# signal to the editor: a foreground child never receives one, and the shell's
# own trap cannot run until that child has finished, so the editor outlives
# both the `kill` and the `kill -9` that follows it. It is then orphaned and
# still holding the exclusive keyboard grab, the touchscreen and its Bluetooth
# daemon, and the launch that meant to replace it comes up beside it instead:
# two editors painting one screen, splitting every tap, each restoring the
# framework's screen over the other on the way out. That is the tile that will
# not give a writer their page back.
#
# So the editor is started in the background and the lock names it, which makes
# `kill` reach the process that can act on it and `/proc/$PID` mean what the
# waiting launcher above reads it to mean. `$$` holds the claim only for the
# moment before the editor exists, so a tap landing in that gap still finds
# something alive and waits for it.
PID=$$
echo "$PID" > "$LOCK"

# Let the device sleep again on the way out. karyll holds powerd's
# `preventScreenSaver` for the session, because it grabs the keyboard and a
# grabbed key cannot reset the idle timer. The binary is built `panic = "abort"`
# and so skips its own cleanup on an abort, which would leave the Kindle unable
# to sleep after the editor is gone. This trap fires however the binary died.
#
# **The lock goes only if it is still ours.** A launch that replaced this one
# has already written its own pid there, and removing it then would leave the
# next tap unable to see the editor that is running.
trap 'if [ "$(cat "$LOCK" 2>/dev/null)" = "$PID" ]; then rm -f "$LOCK"; fi; lipc-set-prop com.lab126.powerd preventScreenSaver 0 2>/dev/null' EXIT

# **Passed on, not swallowed.** The framework signals the launcher when it takes
# the screen back, and the editor is the one that can save and let go of the
# window. Nothing here may exit on its own account: the lock names the editor,
# so the launcher has to outlive it.
trap '[ "$PID" != "$$" ] && kill "$PID" 2>/dev/null' INT TERM

# The most recently touched document, or the welcome one. Documents live
# outside the extension so replacing it on update cannot take them with it.
#
# **Only when there is nothing at all**, which is a fresh install and nothing
# else: a writer who deletes the welcome document has said what they think of
# it, and it must not come back the next time they empty the directory. It used
# to be an empty `draft.md`, which opened onto a blank page with nothing saying
# what any of the controls did.
#
# **Found by glob and `-nt`, never by `ls`.** BusyBox `ls` prints a `?` for
# every byte it thinks is unprintable, and with no Unicode support compiled in
# that is every byte above 0x7F — so a document named in Chinese or Japanese
# came back as a row of question marks, a path that does not exist, and the
# editor opened a blank page called `???.md`. The Files panel never had the bug
# because it reads the directory itself. A glob hands the bytes over untouched.
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

# **Which binary this Kindle can start, asked rather than assumed.** Two are
# shipped, one for each ARM float ABI, and a Kindle has the loader for one of
# them and not the other. Starting the wrong one fails as `not found` — the
# shell reporting the missing interpreter, which reads exactly like the binary
# itself being absent, so it is worth naming here rather than leaving to be
# rediscovered.
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

# A trapped signal returns from `wait` while the editor is still on its way
# out. The next tap watches this lock for the editor to be gone, so wait again
# for what it actually exited with rather than reporting the interruption.
while [ -d "/proc/$PID" ]; do
    sleep 1
    wait "$PID"
    STATUS=$?
done
log "exit=$STATUS"
