#!/bin/sh
# Pair a Bluetooth keyboard. Once per keyboard — the link key is kept.
#
# **Run from a terminal**, kterm or ssh: it lists what it finds and reads your
# choice, and the home-screen tile has no terminal attached.
#
# Put the keyboard into pairing mode first. On a Logitech K380s that is holding
# a channel key until it blinks quickly.
#
# The work is all in the binary — this exists so there is something short to
# type on a device where nothing else is.
exec /mnt/us/extensions/karyll/bin/karyll --pair "$@"
