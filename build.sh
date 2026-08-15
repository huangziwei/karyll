#!/bin/sh
# Cross-compile karyll and assemble the directory that gets copied to the
# device. Run from anywhere:
#
#   ./build.sh
#
# Output: deploy/out — its extensions/ and documents/ go into /mnt/us/.

set -e

cd "$(dirname "$0")"
ROOT=$(pwd)
TARGET=armv7-unknown-linux-gnueabihf
DEVICE="$ROOT/device"
OUT="$ROOT/deploy/out"
EXT="$OUT/extensions/karyll"
SYSROOT="$ROOT/deploy/sysroot"

if ! rustup target list --installed | grep -qx "$TARGET"; then
    echo "error: rustup target '$TARGET' is not installed" >&2
    echo "       fix: rustup target add $TARGET" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# The armhf glibc to link against.
#
# karyll is dynamically linked, because CJK input `dlopen`s Amazon's predictor
# plugin off the device and a static binary has no dynamic loader. Producing a
# dynamic ELF means the linker — which runs here, not on the Kindle — has to
# read an armhf `libc.so.6` to resolve symbols and record the versions it used.
# The device's copy is unreachable at link time, so a copy has to exist on this
# machine. Debian's armhf glibc is the same glibc, public, immutable at these
# URLs, and checksummed below.
#
# **The pin must be no newer than the oldest device's glibc**, because a binary
# records the symbol versions it linked against and a device that has never
# heard of one refuses to start. The Kindles this targets run 2.20 and 2.35.
# glibc keeps its old versions forever, so linking against the oldest produces
# one binary that runs on all of them; linking against the newest produces one
# that runs on exactly one.
#
# Jessie's 2.19 is that pin. Below 2.34 `dlopen` and `pthread_*` still live in
# `libdl.so.2` and `libpthread.so.0`, so both appear in NEEDED — allowed by the
# check further down, because every device ships them and 2.34 and later keep
# them as stubs for exactly this.
#
# archive.debian.org, not deb.debian.org: a pool URL stops resolving the moment
# Debian ships a point release. Jessie is end of life and frozen, so its pool
# will not move again.
ARCHIVE=http://archive.debian.org/debian/pool/main
GLIBC=2.19-18+deb8u10
LIBGCC=4.9.2-10+deb8u1
# The stamp is the whole pin, so bumping any part of it rebuilds the sysroot
# instead of leaving a stale one in place.
#
# **Where it lives is part of the pin.** `usr-lib/libc.so` is a linker script
# naming absolute paths into this directory, so a checkout that moves carries
# its sysroot with it and leaves the script naming a directory that is gone: the
# link then fails on a missing `libc.so.6` that is present at the new path.
# Rebuilding is a relink — the .debs stay in `cache/`, which this never removes.
SYSROOT_STAMP="jessie $GLIBC $LIBGCC $SYSROOT"

# name url sha256, one per line.
PACKAGES="\
libc6_${GLIBC}_armhf.deb $ARCHIVE/g/glibc/libc6_${GLIBC}_armhf.deb ccfc4a10a1654454ad07ac381d55dc1bfe0787ebbcd87b42ac05402777ce41b5
libc6-dev_${GLIBC}_armhf.deb $ARCHIVE/g/glibc/libc6-dev_${GLIBC}_armhf.deb b5af7102716127343a82dfdb42e5b9cd9dc28e28fc14653e0db8f9efdd1fe0a9
libgcc1_${LIBGCC}_armhf.deb $ARCHIVE/g/gcc-4.9/libgcc1_${LIBGCC}_armhf.deb a456c21c4805a3003586a492af1ddd68083de81f75f3da0807732ad44306541c"

build_sysroot() {
    cache="$SYSROOT/cache"
    echo "==> Fetching the armhf link sysroot (Debian jessie glibc $GLIBC)"
    mkdir -p "$cache"

    echo "$PACKAGES" | while read -r name url want; do
        have=$(shasum -a 256 "$cache/$name" 2>/dev/null | cut -d" " -f1)
        if [ "$have" != "$want" ]; then
            if ! curl -fL --progress-bar -o "$cache/$name.part" "$url"; then
                rm -f "$cache/$name.part"
                echo "error: could not download $url" >&2
                echo "       karyll cross-links against glibc and cannot be built without it." >&2
                exit 1
            fi
            mv "$cache/$name.part" "$cache/$name"
            have=$(shasum -a 256 "$cache/$name" | cut -d" " -f1)
        fi
        if [ "$have" != "$want" ]; then
            echo "error: checksum mismatch on $name" >&2
            echo "       expected $want" >&2
            echo "       got      $have" >&2
            rm -f "$cache/$name"
            exit 1
        fi
    done

    work="$SYSROOT/unpacked"
    rm -rf "$work" "$SYSROOT/lib" "$SYSROOT/usr-lib" "$SYSROOT/STAMP"
    mkdir -p "$work" "$SYSROOT/lib" "$SYSROOT/usr-lib"

    # `ar` then `tar`, both of which macOS ships: a .deb is an ar archive
    # holding a compressed tar, and nothing here needs dpkg. The member is
    # matched by glob rather than named — jessie's libc6 is gzipped and its
    # libc6-dev is xz, in the same set.
    for name in $(echo "$PACKAGES" | cut -d" " -f1); do
        (cd "$work" && ar x "$cache/$name" && tar xf data.tar.* &&
            rm -f data.tar.* control.tar.* debian-binary)
    done

    # **Flattened, and every symlink repointed.** Debian is multiarch, so the
    # real files sit under lib/arm-linux-gnueabihf and usr/lib/..., and the
    # `.so` development links beside them are *absolute* into a Debian root
    # that does not exist here. Copying the files and rebuilding the links by
    # basename turns that into a directory this machine's linker can read.
    src_lib="$work/lib/arm-linux-gnueabihf"
    src_usr="$work/usr/lib/arm-linux-gnueabihf"
    find "$src_lib" -maxdepth 1 -type f -exec cp {} "$SYSROOT/lib/" \;
    find "$src_usr" -maxdepth 1 -type f -exec cp {} "$SYSROOT/usr-lib/" \;
    relink "$src_lib" "$SYSROOT/lib" "$SYSROOT/usr-lib"
    relink "$src_usr" "$SYSROOT/usr-lib" "$SYSROOT/lib"

    # The loader under the name the linker records, and the name `-lgcc_s`
    # looks for. **Only when the flattening has not already left one**, or a
    # link written over the real file points at itself: bookworm ships the
    # loader as a real `ld-linux-armhf.so.3`, older glibc packages name it
    # `ld-<version>.so` and leave the stable name as a link.
    if [ ! -e "$SYSROOT/lib/ld-linux-armhf.so.3" ]; then
        loader=$(find "$SYSROOT/lib" -maxdepth 1 -type f -name 'ld-*.so' | head -1)
        [ -n "$loader" ] && ln -sf "$(basename "$loader")" "$SYSROOT/lib/ld-linux-armhf.so.3"
    fi
    [ -e "$SYSROOT/lib/libgcc_s.so" ] || ln -sf libgcc_s.so.1 "$SYSROOT/lib/libgcc_s.so"

    # **Some `.so` files here are GNU ld scripts, and Debian's name absolute
    # paths into a root that does not exist on this machine.** Every one is
    # repointed at this sysroot, keeping whatever the script actually said.
    #
    # `libc.so` is the one that makes a plain `-lc` work at all: it is how
    # `libc_nonshared.a` is pulled in for `__aeabi_read_tp` and how the loader
    # is reached for `__tls_get_addr`, both missing symbols at the very end of a
    # link without it. `libpthread.so` is a script too on a pin this old, and
    # its absolute `GROUP` is what the link fails on first.
    # `LC_ALL=C` throughout: a real shared object sits in this directory too and
    # BSD's grep and sed both refuse its bytes as an illegal sequence in a UTF-8
    # locale. **The `/usr/lib` rule has to come first**, because
    # `/usr/lib/arm-linux-gnueabihf/` contains `/lib/arm-linux-gnueabihf/` and
    # the shorter rewrite otherwise leaves a stray `/usr` in front of an
    # absolute path.
    for script in "$SYSROOT/usr-lib"/*.so; do
        [ -f "$script" ] || continue
        LC_ALL=C grep -q "OUTPUT_FORMAT" "$script" 2>/dev/null || continue
        LC_ALL=C sed -e "s|/usr/lib/arm-linux-gnueabihf/|$SYSROOT/usr-lib/|g" \
                     -e "s|/lib/arm-linux-gnueabihf/|$SYSROOT/lib/|g" \
            "$script" > "$script.rewritten" && mv "$script.rewritten" "$script"
    done

    rm -rf "$work"

    # A link the linker cannot follow fails with a message naming the wrong
    # file.
    dangling=$(find "$SYSROOT/lib" "$SYSROOT/usr-lib" -type l ! -exec test -e {} \; -print)
    if [ -n "$dangling" ]; then
        echo "error: the sysroot has links that go nowhere:" >&2
        echo "$dangling" >&2
        exit 1
    fi

    echo "$SYSROOT_STAMP" > "$SYSROOT/STAMP"
}

relink() {
    for link in "$1"/*; do
        [ -L "$link" ] || continue
        name=$(basename "$link")
        target=$(basename "$(readlink "$link")")
        if [ -e "$2/$target" ]; then
            ln -sf "$target" "$2/$name"
        elif [ -e "$3/$target" ]; then
            ln -sf "../$(basename "$3")/$target" "$2/$name"
        fi
    done
}

[ "$(cat "$SYSROOT/STAMP" 2>/dev/null)" = "$SYSROOT_STAMP" ] || build_sysroot

# Driven as a raw linker rather than through a cc driver, so the C runtime
# objects have to be named explicitly and in order — nothing adds them for us.
# Everything else the link needs is reached through the sysroot's own `libc.so`
# script, rewritten above for exactly that purpose.
SYSROOT_FLAGS="-C linker-flavor=ld.lld"
SYSROOT_FLAGS="$SYSROOT_FLAGS -C link-arg=--dynamic-linker=/lib/ld-linux-armhf.so.3"
SYSROOT_FLAGS="$SYSROOT_FLAGS -C link-arg=-L$SYSROOT/lib -C link-arg=-L$SYSROOT/usr-lib"
for o in Scrt1.o crti.o crtn.o; do
    SYSROOT_FLAGS="$SYSROOT_FLAGS -C link-arg=$SYSROOT/usr-lib/$o"
done

# ---------------------------------------------------------------------------

VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
[ -n "$VERSION" ] || { echo "error: could not read version from Cargo.toml" >&2; exit 1; }

# No KUAL metadata: KUAL does not exist on any Scribe firmware, so the
# home-screen tile is the only way in and menu.json/config.xml would be dead
# files. Everything the menus would have reached is in the app itself.

# Stamp the build so a log on the device names the binary that wrote it.
# Diagnosing a stale copy as though it were the current one costs a round trip
# and a reboot every time.
#
# An inherited KARYLL_BUILD wins, so a release carries its tag; otherwise the
# time of day separates two builds made the same afternoon.
BUILD_STAMP=${KARYLL_BUILD:-$(date +%H%M%S)}
echo "==> Cross-compiling karyll $VERSION for $TARGET  [build $BUILD_STAMP]"
KARYLL_BUILD="$BUILD_STAMP" RUSTFLAGS="$SYSROOT_FLAGS" \
    cargo build --release --target "$TARGET" -p karyll-native

BIN="$ROOT/target/$TARGET/release/karyll"
file "$BIN" | grep -q "dynamically linked" || {
    echo "error: $BIN is not dynamically linked" >&2
    echo "       it must be, or it cannot dlopen the CJK plugin" >&2
    exit 1
}

# **Everything it needs must already be on the device**, and these four are all
# it may need. Anything else would have to be shipped, and shipping Amazon's
# libraries is not something this repo does.
#
# `libdl.so.2`, `libpthread.so.0` and `librt.so.1` are here because the pin is
# below 2.34 and all three are therefore still separate libraries. Every device
# carries them, older ones as the real thing and newer ones as the stubs glibc
# kept for exactly this case, so naming them costs nothing and is what buys the
# older devices. Checked against each device's own `/lib` before being allowed
# here, which is the only reason any name belongs on this list.
#
# Fatal, not a warning: a binary naming a library the Kindle does not have never
# starts, and the only symptom there is a tile that does nothing.
#
# Read with whichever tool can parse a foreign ELF here. macOS has LLVM's
# `objdump`, which reads any target; a Linux box has GNU `readelf`, which is
# architecture-independent, while its `objdump` is often built for the host
# architecture alone and refuses an armhf binary outright.
# Absent tools and empty greps are both ordinary here — only one of the two
# readers exists on any given machine — so neither may trip `set -e`.
elf_needed() {
    { readelf -d "$1" 2>/dev/null | sed -n 's/.*NEEDED.*\[\(.*\)\].*/\1/p'; } || true
    { objdump -p "$1" 2>/dev/null | awk '/NEEDED/ {print $2}'; } || true
}
elf_versions() {
    { readelf --dyn-syms "$1" 2>/dev/null | grep -o "GLIBC_[0-9.]*"; } || true
    { objdump -T "$1" 2>/dev/null | grep -o "GLIBC_[0-9.]*"; } || true
}

NEEDED=$(elf_needed "$BIN" | sort -u)
# A dynamically linked binary always names libc, so an empty list means the file
# was not read rather than that it needs nothing.
[ -n "$NEEDED" ] || {
    echo "error: could not read the NEEDED list from $BIN" >&2
    echo "       install GNU binutils (readelf) or an objdump that reads armhf;" >&2
    echo "       shipping without this check is how a missing library reaches" >&2
    echo "       the device as a tile that does nothing" >&2
    exit 1
}

for lib in $NEEDED; do
    case "$lib" in
        libc.so.6|libgcc_s.so.1|libdl.so.2|libpthread.so.0|librt.so.1) ;;
        *)
            echo "error: karyll now needs $lib, which the device may not have" >&2
            echo "       every NEEDED beyond libc.so.6, libgcc_s.so.1, libdl.so.2," >&2
            echo "       libpthread.so.0 and librt.so.1 has to be accounted for" >&2
            echo "       before this can ship" >&2
            exit 1
            ;;
    esac
done

# The oldest glibc that can run it, decided by the sysroot rather than by
# anything in this repo's source. Logged so a firmware question is answerable
# from a build log rather than from a device.
NEEDS=$(elf_versions "$BIN" | sort -uV | tail -1)
echo "==> Links against ${NEEDS:-an unknown glibc} or newer"

echo "==> Assembling $OUT"
rm -rf "$OUT"
mkdir -p "$EXT/bin" "$EXT/hid" "$EXT/var" "$EXT/share" "$OUT/documents"

cp "$DEVICE"/bin/*.sh "$EXT/bin/"
# The welcome document, which the launcher copies into the documents directory
# the first time it finds it empty. It ships inside the extension rather than
# in documents/ because documents/ is the writer's, and an update replaces the
# extension wholesale — so a file placed here can never overwrite a draft.
cp "$DEVICE"/share/*.md "$EXT/share/"
cp "$BIN" "$EXT/bin/karyll"
chmod 755 "$EXT/bin"/* 2>/dev/null || true

# The home-screen tile. The hotfix indexes documents/*.sh as a library tile, and
# that is the primary way in on this device — KUAL is the fallback, and is not
# present on every firmware.
cp "$DEVICE"/documents/*.sh "$OUT/documents/"
chmod 755 "$OUT/documents"/*.sh 2>/dev/null || true

# The Bluetooth stack. Fetched here rather than committed: it is 49 MB of
# someone else's release, it changes wholesale on every upstream bump, and the
# device must never fetch anything — so the download happens once on this
# machine and is cached in deploy/hid, which is gitignored.
#
# Pinned by version and checksum. An unpinned download would silently change
# what ships to the device.
HID_VERSION=v3.11.0
HID_SHA256=ef30ed4b6f706ea44f789185cb300054d6126b590e8fb464122309be675e0922
HID_URL="https://github.com/zampierilucas/kindle-hid-passthrough/releases/download/$HID_VERSION/kindle-hid-passthrough-armv7.tar.gz"
CACHE="$ROOT/deploy/hid"
TARBALL="$CACHE/kindle-hid-passthrough-$HID_VERSION.tar.gz"

verify() {
    [ -f "$1" ] || return 1
    have=$(shasum -a 256 "$1" 2>/dev/null | cut -d" " -f1)
    [ "$have" = "$HID_SHA256" ]
}

mkdir -p "$CACHE"
if ! verify "$TARBALL"; then
    echo "==> Fetching the Bluetooth stack $HID_VERSION (49 MB, cached in deploy/hid)"
    if ! curl -fL --progress-bar -o "$TARBALL.part" "$HID_URL"; then
        rm -f "$TARBALL.part"
        echo "error: could not download $HID_URL" >&2
        echo "       karyll needs it to see a keyboard; there is no kernel Bluetooth." >&2
        exit 1
    fi
    mv "$TARBALL.part" "$TARBALL"
    if ! verify "$TARBALL"; then
        echo "error: checksum mismatch on $TARBALL" >&2
        echo "       expected $HID_SHA256" >&2
        echo "       got      $(shasum -a 256 "$TARBALL" | cut -d" " -f1)" >&2
        rm -f "$TARBALL"
        exit 1
    fi
fi

echo "==> Bundling the Bluetooth stack $HID_VERSION"
# koreader-plugin/ is omitted: this project excludes KOReader outright, and
# nothing in a Python daemon loads a Lua plugin.
tar xzf "$TARBALL" -C "$EXT/hid" --exclude="./koreader-plugin"
# The release ships no LICENSE; GPLv3 asks that one travel with the binaries.
if [ -f "$ROOT/LICENSE" ]; then
    cp "$ROOT/LICENSE" "$EXT/hid/LICENSE"
fi
printf 'kindle-hid-passthrough %s\nsource: https://github.com/zampierilucas/kindle-hid-passthrough\ntarball sha256: %s\nkoreader-plugin/ omitted; LICENSE added (upstream ships none, and it is GPLv3).\n' \
    "$HID_VERSION" "$HID_SHA256" > "$EXT/hid/PROVENANCE"

# Our overlay last: README, and the config.ini that repoints the stack's state
# into the extension and its log off tmpfs.
cp "$DEVICE"/hid/*.md "$DEVICE"/hid/*.ini "$EXT/hid/" 2>/dev/null || true

echo
echo "==> Ready: $OUT"
du -sh "$OUT" 2>/dev/null || true
echo
echo "Copy both of these into /mnt/us/ over MTP or USB:"
echo "    extensions/karyll    the app"
echo "    documents/Karyll.sh  the home-screen tile you tap to open it"
