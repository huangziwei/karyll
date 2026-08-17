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
DEVICE="$ROOT/device"
OUT="$ROOT/deploy/out"
EXT="$OUT/extensions/karyll"

# **Two ARM ABIs, and which one a Kindle wants is not discoverable at runtime.**
# A hard-float binary names `/lib/ld-linux-armhf.so.3` as its interpreter and a
# soft-float one names `/lib/ld-linux.so.3`; a device has one of them and not
# the other, and the loader it does not have is a file that does not exist — so
# the shell reports the missing interpreter as `not found`, which reads exactly
# like the binary itself being absent. Measured on the devices' own `/usr/bin/kb`:
# the two newer Kindles are hard-float, the Oasis 2 is soft-float.
#
# So both are built and both ship. This is the one thing in karyll that is a
# build rather than a discovery, because it decides which file can be started
# at all — and the launcher still discovers, by asking which loader is there.
#
# Fields: Debian architecture, Rust target, Debian multiarch directory, the
# loader to record, and the name the binary ships under.
ABIS="armhf armel"

abi_target() {
    case "$1" in
        armhf) echo armv7-unknown-linux-gnueabihf ;;
        armel) echo armv7-unknown-linux-gnueabi ;;
    esac
}
abi_multiarch() {
    case "$1" in
        armhf) echo arm-linux-gnueabihf ;;
        armel) echo arm-linux-gnueabi ;;
    esac
}
abi_loader() {
    case "$1" in
        armhf) echo ld-linux-armhf.so.3 ;;
        armel) echo ld-linux.so.3 ;;
    esac
}
abi_binary() {
    case "$1" in
        armhf) echo karyll ;;
        armel) echo karyll-softfloat ;;
    esac
}
abi_float() {
    case "$1" in
        armhf) echo hard ;;
        armel) echo soft ;;
    esac
}

for abi in $ABIS; do
    target=$(abi_target "$abi")
    if ! rustup target list --installed | grep -qx "$target"; then
        echo "error: rustup target '$target' is not installed" >&2
        echo "       fix: rustup target add $target" >&2
        exit 1
    fi
done

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
# One pin, both architectures: the same Debian release builds the same glibc for
# each, so the floor is the same either way and only the checksums differ.
#
# The stamp is the whole pin, so bumping any part of it rebuilds the sysroot
# instead of leaving a stale one in place.
#
# **Where it lives is part of the pin.** `usr-lib/libc.so` is a linker script
# naming absolute paths into this directory, so a checkout that moves carries
# its sysroot with it and leaves the script naming a directory that is gone: the
# link then fails on a missing `libc.so.6` that is present at the new path.
# Rebuilding is a relink — the .debs stay in `cache/`, which this never removes.

# name url sha256, one per line, for the architecture asked for.
abi_packages() {
    case "$1" in
        armhf)
            libc=ccfc4a10a1654454ad07ac381d55dc1bfe0787ebbcd87b42ac05402777ce41b5
            dev=b5af7102716127343a82dfdb42e5b9cd9dc28e28fc14653e0db8f9efdd1fe0a9
            gcc=a456c21c4805a3003586a492af1ddd68083de81f75f3da0807732ad44306541c
            ;;
        armel)
            libc=0b53023e421022a6b25b0e63cbfe1bd15df01cd41bf8cc62a1de8e375a674ebd
            dev=fe1beac9ff010229e4d075430a89d6f19bdac4510c14c47f7717927c51ad2399
            gcc=b09156d94a289361b8116d351700b2e44055f709c25c06128d3a79e6a177678b
            ;;
    esac
    echo "\
libc6_${GLIBC}_$1.deb $ARCHIVE/g/glibc/libc6_${GLIBC}_$1.deb $libc
libc6-dev_${GLIBC}_$1.deb $ARCHIVE/g/glibc/libc6-dev_${GLIBC}_$1.deb $dev
libgcc1_${LIBGCC}_$1.deb $ARCHIVE/g/gcc-4.9/libgcc1_${LIBGCC}_$1.deb $gcc"
}

# Build the link sysroot for one architecture, in a directory of its own.
build_sysroot() {
    abi=$1
    SYSROOT="$ROOT/deploy/sysroot-$abi"
    multiarch=$(abi_multiarch "$abi")
    loader=$(abi_loader "$abi")
    SYSROOT_STAMP="jessie $GLIBC $LIBGCC $SYSROOT"
    PACKAGES=$(abi_packages "$abi")

    [ "$(cat "$SYSROOT/STAMP" 2>/dev/null)" = "$SYSROOT_STAMP" ] && return 0

    cache="$SYSROOT/cache"
    echo "==> Fetching the $abi link sysroot (Debian jessie glibc $GLIBC)"
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
    src_lib="$work/lib/$multiarch"
    src_usr="$work/usr/lib/$multiarch"
    find "$src_lib" -maxdepth 1 -type f -exec cp {} "$SYSROOT/lib/" \;
    find "$src_usr" -maxdepth 1 -type f -exec cp {} "$SYSROOT/usr-lib/" \;
    relink "$src_lib" "$SYSROOT/lib" "$SYSROOT/usr-lib"
    relink "$src_usr" "$SYSROOT/usr-lib" "$SYSROOT/lib"

    # The loader under the name the linker records, and the name `-lgcc_s`
    # looks for. **Only when the flattening has not already left one**, or a
    # link written over the real file points at itself: bookworm ships the
    # loader as a real `ld-linux-armhf.so.3`, older glibc packages name it
    # `ld-<version>.so` and leave the stable name as a link.
    if [ ! -e "$SYSROOT/lib/$loader" ]; then
        real=$(find "$SYSROOT/lib" -maxdepth 1 -type f -name 'ld-*.so' | head -1)
        [ -n "$real" ] && ln -sf "$(basename "$real")" "$SYSROOT/lib/$loader"
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
        LC_ALL=C sed -e "s|/usr/lib/$multiarch/|$SYSROOT/usr-lib/|g" \
                     -e "s|/lib/$multiarch/|$SYSROOT/lib/|g" \
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

# Read with whichever tool can parse a foreign ELF here. macOS has LLVM's
# `objdump`, which reads any target; a Linux box has GNU `readelf`, which is
# architecture-independent, while its `objdump` is often built for the host
# architecture alone and refuses an ARM binary outright.
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

# Hard or soft float, from the ARM ELF header's own flag rather than from
# anything this script asked for: bit 0x400 of `e_flags` is
# `EF_ARM_ABI_FLOAT_HARD` and 0x200 is `EF_ARM_ABI_FLOAT_SOFT`. Read with `od`
# so it works with no toolchain at all — the flags are the tenth 32-bit word of
# a little-endian ELF32 header.
elf_float_abi() {
    flags=$(od -An -tx4 -j 36 -N 4 "$1" 2>/dev/null | tr -d " \n")
    [ -n "$flags" ] || { echo unknown; return; }
    if [ $(( 0x$flags & 0x400 )) -ne 0 ]; then echo hard; else echo soft; fi
}

# Build one binary for one ABI, and check it can start where it is going.
build_abi() {
    abi=$1
    TARGET=$(abi_target "$abi")
    build_sysroot "$abi"
    SYSROOT="$ROOT/deploy/sysroot-$abi"

    # Driven as a raw linker rather than through a cc driver, so the C runtime
    # objects have to be named explicitly and in order — nothing adds them for
    # us. Everything else the link needs is reached through the sysroot's own
    # `libc.so` script, rewritten above for exactly that purpose.
    SYSROOT_FLAGS="-C linker-flavor=ld.lld"
    SYSROOT_FLAGS="$SYSROOT_FLAGS -C link-arg=--dynamic-linker=/lib/$(abi_loader "$abi")"
    SYSROOT_FLAGS="$SYSROOT_FLAGS -C link-arg=-L$SYSROOT/lib -C link-arg=-L$SYSROOT/usr-lib"

    # **Not a PIE.** Rust builds one by default and the Kindles do not run them:
    # every executable on these firmwares is `ET_EXEC`, `ET_DYN` there is only
    # ever a shared library, and a position-independent executable dies inside
    # the older loader — after it has found every library and before it runs a
    # single init, so nothing the program itself would print ever appears. It
    # starts on glibc 2.35 and segfaults on 2.20, on two different devices, at
    # the same point.
    #
    # `crt1.o` rather than `Scrt1.o` with it: the `S` one is the start file
    # built for a shared-style executable, and pairing it with a fixed-address
    # link is a mismatch at the entry point itself.
    SYSROOT_FLAGS="$SYSROOT_FLAGS -C relocation-model=static -C link-arg=-no-pie"

    # **Lazy binding, which is what the device's own loader survives.** Rust
    # links `-z now` by default, so every one of the hundred-odd PLT symbols is
    # resolved before the program starts. `LD_DEBUG=reloc` on a Kindle running
    # glibc 2.20 shows every library relocating `(lazy)` and coming through,
    # and karyll — the one object relocated eagerly — segfaulting inside the
    # loader with nothing of its own printed. Bound on first call instead, the
    # way everything else on the device is.
    SYSROOT_FLAGS="$SYSROOT_FLAGS -C relro-level=partial"
    for o in crt1.o crti.o crtn.o; do
        SYSROOT_FLAGS="$SYSROOT_FLAGS -C link-arg=$SYSROOT/usr-lib/$o"
    done

    echo "==> Cross-compiling karyll $VERSION for $TARGET  [build $BUILD_STAMP]"
    KARYLL_BUILD="$BUILD_STAMP" RUSTFLAGS="$SYSROOT_FLAGS" \
        cargo build --release --target "$TARGET" -p karyll-native

    BIN="$ROOT/target/$TARGET/release/karyll"
    file "$BIN" | grep -q "dynamically linked" || {
        echo "error: $BIN is not dynamically linked" >&2
        echo "       it must be, or it cannot dlopen the CJK plugin" >&2
        exit 1
    }

    # **Everything it needs must already be on the device**, and these five are
    # all it may need. Anything else would have to be shipped, and shipping
    # Amazon's libraries is not something this repo does.
    #
    # `libdl.so.2`, `libpthread.so.0` and `librt.so.1` are here because the pin
    # is below 2.34 and all three are therefore still separate libraries. Every
    # device carries them, older ones as the real thing and newer ones as the
    # stubs glibc kept for exactly this case, so naming them costs nothing and
    # is what buys the older devices. Checked against each device's own `/lib`
    # before being allowed here, which is the only reason any name belongs on
    # this list.
    #
    # Fatal, not a warning: a binary naming a library the Kindle does not have
    # never starts, and the only symptom there is a tile that does nothing.
    NEEDED=$(elf_needed "$BIN" | sort -u)
    # A dynamically linked binary always names libc, so an empty list means the
    # file was not read rather than that it needs nothing.
    [ -n "$NEEDED" ] || {
        echo "error: could not read the NEEDED list from $BIN" >&2
        echo "       install GNU binutils (readelf) or an objdump that reads ARM;" >&2
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

    # **The float ABI, read back out of what was actually produced.** It is the
    # one property that decides whether the file can start at all, and getting
    # it from the build flags would only say what was asked for.
    got=$(elf_float_abi "$BIN")
    want=$(abi_float "$abi")
    [ "$got" = "$want" ] || {
        echo "error: the $abi build came out $got-float, not $want-float" >&2
        echo "       a Kindle has one loader or the other, and the wrong one" >&2
        echo "       reaches the device as a tile that does nothing" >&2
        exit 1
    }

    # The oldest glibc that can run it, decided by the sysroot rather than by
    # anything in this repo's source. Logged so a firmware question is
    # answerable from a build log rather than from a device.
    NEEDS=$(elf_versions "$BIN" | sort -uV | tail -1)
    echo "==> $(abi_binary "$abi"): $got-float, /lib/$(abi_loader "$abi"), ${NEEDS:-an unknown glibc} or newer"
}

for abi in $ABIS; do
    build_abi "$abi"
done

echo "==> Assembling $OUT"
rm -rf "$OUT"
mkdir -p "$EXT/bin" "$EXT/hid" "$EXT/var" "$EXT/share" "$OUT/documents"

cp "$DEVICE"/bin/*.sh "$EXT/bin/"
# The welcome document, which the launcher copies into the documents directory
# the first time it finds it empty. It ships inside the extension rather than
# in documents/ because documents/ is the writer's, and an update replaces the
# extension wholesale — so a file placed here can never overwrite a draft.
cp "$DEVICE"/share/*.md "$EXT/share/"
# Both binaries, because which one a Kindle can start is a property of the
# Kindle. The launcher picks by looking for the loader each one names.
for abi in $ABIS; do
    cp "$ROOT/target/$(abi_target "$abi")/release/karyll" "$EXT/bin/$(abi_binary "$abi")"
done
chmod 755 "$EXT/bin"/* 2>/dev/null || true

# The home-screen tile. The hotfix indexes documents/*.sh as a library tile,
# which is the only way in on this device.
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

# The writing faces. **No Kindle carries a monospace text face**: the devices
# probed ship the same 87 faces, one of them adds a set of handwriting styles,
# and the only file named for the word is a symbol font. So a face to write code
# and tables in has to come from here, and iA's three come with the argument for
# what a writing face is as well as the files.
#
# Fetched rather than committed, for the reason the Bluetooth stack is: it is
# someone else's release. Pinned to a commit because the repository has neither
# tags nor releases, and checksummed **per file rather than per archive**, since
# GitHub's generated tarballs are not byte-stable across recompressions while
# the blob at a commit is.
FONTS_COMMIT=f32c04c3058a75d7ce28919ce70fe8800817491b
FONTS_CACHE="$ROOT/deploy/fonts"
FONTS_RAW="https://raw.githubusercontent.com/iaolo/iA-Fonts/$FONTS_COMMIT"

# sha256, the directory under the repository root, and the file. Only the static
# cuts: the variable files carry no bold at all, and nothing here sets a
# variation axis, so they would draw one weight under four names.
FONTS_MANIFEST="
454a20d2b4569ba66810f0f710bb022065cbaac11c82fdcef677545ab27329f2 iA%20Writer%20Duo/Static iAWriterDuoS-Regular.ttf
8e15abab476026abd362d079fd519e9c1220e0ab32b3ce3e4c13695af53e7153 iA%20Writer%20Duo/Static iAWriterDuoS-Italic.ttf
779963585007973753ba1c4aa85d67b21c29854c1f9730411d80dc0c879b0908 iA%20Writer%20Duo/Static iAWriterDuoS-Bold.ttf
830443f3ec75a277ec00917a7ed0523a93869ea9a7ea5f8d9f1d643b25b6cd47 iA%20Writer%20Duo/Static iAWriterDuoS-BoldItalic.ttf
929605302a57250e712908cb5f6e1ce80c7d0accd5fd2555345f29a5e8d4e30b iA%20Writer%20Mono/Static iAWriterMonoS-Regular.ttf
c7e7e36e8167bf50f27e46c9cab447d04cc570bd388b998044e2e29f4cebc216 iA%20Writer%20Mono/Static iAWriterMonoS-Italic.ttf
76aa5b5b4f9091a9c686a2a6fe5ff3495bb791994d7079857e5b24ae98063743 iA%20Writer%20Mono/Static iAWriterMonoS-Bold.ttf
b0cf9571234528b0896aacf97eb3ec45712da40b8410c799fa43ea123bc19e35 iA%20Writer%20Mono/Static iAWriterMonoS-BoldItalic.ttf
6e367e0e00e057d383680ffae7b64f520d06e1f96abd28bddd67d424fee8e8df iA%20Writer%20Quattro/Static iAWriterQuattroS-Regular.ttf
84c19517be57e8c0521f43a1d5c29766b1f0cb9353300e819b193da1b02f47ac iA%20Writer%20Quattro/Static iAWriterQuattroS-Italic.ttf
40dbb1ffed472cdc96a0133073bc777a40782883678b80dfd31677d5963b72b9 iA%20Writer%20Quattro/Static iAWriterQuattroS-Bold.ttf
f61aa3c97d611dec01c7414e07b9212a164501b6d1a800af0dcda11acf4eabb0 iA%20Writer%20Quattro/Static iAWriterQuattroS-BoldItalic.ttf
2eb84d6d03a9af6e99816f82f50a77c26e7ff6681293f4619cd33a392a8c13b6 iA%20Writer%20Quattro LICENSE.md
"

fonts_have() {
    [ -f "$2" ] || return 1
    [ "$(shasum -a 256 "$2" 2>/dev/null | cut -d" " -f1)" = "$1" ]
}

fonts_fetch() {
    out="$FONTS_CACHE/$3"
    fonts_have "$1" "$out" && return 0
    if ! curl -fsSL -o "$out.part" "$FONTS_RAW/$2/$3"; then
        rm -f "$out.part"
        echo "error: could not download $FONTS_RAW/$2/$3" >&2
        exit 1
    fi
    if ! fonts_have "$1" "$out.part"; then
        echo "error: checksum mismatch on $3" >&2
        echo "       expected $1" >&2
        echo "       got      $(shasum -a 256 "$out.part" | cut -d" " -f1)" >&2
        rm -f "$out.part"
        exit 1
    fi
    mv "$out.part" "$out"
}

mkdir -p "$FONTS_CACHE" "$EXT/fonts"
echo "==> Bundling the writing faces (iA-Fonts $(echo "$FONTS_COMMIT" | cut -c1-7))"
# Redirected rather than piped: a pipeline runs the loop in a subshell, where a
# failed checksum would exit that shell and let the build carry on regardless.
while read -r sha dir name; do
    [ -n "$name" ] || continue
    fonts_fetch "$sha" "$dir" "$name"
    cp "$FONTS_CACHE/$name" "$EXT/fonts/$name"
done <<EOF
$FONTS_MANIFEST
EOF

printf 'iA-Fonts @ %s\nsource: https://github.com/iaolo/iA-Fonts\nlicence: SIL Open Font License 1.1, see LICENSE.md\nStatic cuts only; each file pinned by sha256 in build.sh.\n' \
    "$FONTS_COMMIT" > "$EXT/fonts/PROVENANCE"

echo
echo "==> Ready: $OUT"
du -sh "$OUT" 2>/dev/null || true
echo
echo "Copy both of these into /mnt/us/ over MTP or USB:"
echo "    extensions/karyll    the app"
echo "    documents/Karyll.sh  the home-screen tile you tap to open it"
