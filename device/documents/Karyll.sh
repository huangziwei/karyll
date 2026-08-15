#!/bin/sh
# Name: Karyll
# Author: Ziwei Huang
# DontUseFBInk

# The home-screen tile, and karyll's primary launcher. The hotfix indexes a
# documents/*.sh scriptlet as a library tile, so one tap from the library opens
# the editor. KUAL > karyll is the fallback front door and runs this same
# wrapper, so the daemon lifecycle and the launch log stay single-sourced in the
# extension.
#
# No `# Icon:` line yet, so the tile draws with a default cover. Adding one
# means generating a ~55 KB base64 PNG into a header line here; the body is
# hand-edited and would be left alone by that.

# Don't stack a second editor on a double tap. Two instances would fight over an
# exclusive keyboard grab and both try to start and stop the Bluetooth daemon.
#
# This is the cheap check that catches the common case; the launcher holds a
# proper lock across the whole session, including the seconds it spends waiting
# for a keyboard to connect, which is the window this test cannot see.
if pidof karyll >/dev/null 2>&1; then
    exit 0
fi

nohup sh -c 'sleep 1; exec /mnt/us/extensions/karyll/bin/karyll.sh' >/dev/null 2>&1 &
