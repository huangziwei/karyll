#!/bin/sh
# Pair a Bluetooth keyboard. Once per keyboard — the link key is kept.
# **Run from a terminal**, kterm or ssh: it lists what it finds and reads your
# choice. Put the keyboard into pairing mode first.
exec /mnt/us/extensions/karyll/bin/karyll --pair "$@"
