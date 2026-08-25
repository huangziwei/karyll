#!/bin/sh
# Cross-compile karyll and stage device/ for a USB copy:
#   device/extensions/karyll/   -> /mnt/us/extensions/karyll/

set -e

cd "$(dirname "$0")"
ROOT=$(pwd)
EXT="$ROOT/device/extensions/karyll"

# Two ARM ABIs: a hard-float binary names `/lib/ld-linux-armhf.so.3` and a
# soft-float one `/lib/ld-linux.so.3`. Both are built and both ship. Fields:
# architecture, Rust target, multiarch directory, loader, binary name.
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
# The armhf glibc to link against, pinned at jessie's 2.19. Below 2.34 `dlopen`
# and `pthread_*` live in `libdl.so.2` and `libpthread.so.0`, both in NEEDED.
ARCHIVE=http://archive.debian.org/debian/pool/main
GLIBC=2.19-18+deb8u10
LIBGCC=4.9.2-10+deb8u1
# One pin, both architectures, differing only in checksum. The stamp is the
# whole pin. Where it lives is part of it: `usr-lib/libc.so` is a linker script
# naming absolute paths into this directory.

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
    # holding a compressed tar. The member is matched by glob — jessie's libc6
    # is gzipped and its libc6-dev is xz, in the same set.
    for name in $(echo "$PACKAGES" | cut -d" " -f1); do
        (cd "$work" && ar x "$cache/$name" && tar xf data.tar.* &&
            rm -f data.tar.* control.tar.* debian-binary)
    done

    # Flattened, and every symlink repointed. Debian is multiarch, the real
    # files sit under lib/arm-linux-gnueabihf, and the `.so` links beside them
    # are absolute into a Debian root this machine does not have.
    src_lib="$work/lib/$multiarch"
    src_usr="$work/usr/lib/$multiarch"
    find "$src_lib" -maxdepth 1 -type f -exec cp {} "$SYSROOT/lib/" \;
    find "$src_usr" -maxdepth 1 -type f -exec cp {} "$SYSROOT/usr-lib/" \;
    relink "$src_lib" "$SYSROOT/lib" "$SYSROOT/usr-lib"
    relink "$src_usr" "$SYSROOT/usr-lib" "$SYSROOT/lib"

    # The loader under the name the linker records, and the name `-lgcc_s`
    # looks for — only where the flattening has left none. Jessie's glibc
    # names the loader file `ld-<version>.so`.
    if [ ! -e "$SYSROOT/lib/$loader" ]; then
        real=$(find "$SYSROOT/lib" -maxdepth 1 -type f -name 'ld-*.so' | head -1)
        [ -n "$real" ] && ln -sf "$(basename "$real")" "$SYSROOT/lib/$loader"
    fi
    [ -e "$SYSROOT/lib/libgcc_s.so" ] || ln -sf libgcc_s.so.1 "$SYSROOT/lib/libgcc_s.so"

    # Some `.so` files here are GNU ld scripts naming absolute paths into a root
    # this machine does not have; every one is repointed at this sysroot.
    # `LC_ALL=C` throughout, for the shared object beside them.
    for script in "$SYSROOT/usr-lib"/*.so; do
        [ -f "$script" ] || continue
        LC_ALL=C grep -q "OUTPUT_FORMAT" "$script" 2>/dev/null || continue
        LC_ALL=C sed -e "s|/usr/lib/$multiarch/|$SYSROOT/usr-lib/|g" \
                     -e "s|/lib/$multiarch/|$SYSROOT/lib/|g" \
            "$script" > "$script.rewritten" && mv "$script.rewritten" "$script"
    done

    rm -rf "$work"

    # Dangling links fail here, before the link step.
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

# Stamp the build: a log on the device names the binary that wrote it. An
# inherited KARYLL_BUILD wins, carrying a release tag; the time of day separates
# two builds made the same afternoon.
BUILD_STAMP=${KARYLL_BUILD:-$(date +%H%M%S)}

# Read with whichever tool can parse a foreign ELF here: macOS has LLVM's
# `objdump` and a Linux box has GNU `readelf`. Absent tools and empty greps are
# ordinary, and neither may trip `set -e`.
elf_needed() {
    { readelf -d "$1" 2>/dev/null | sed -n 's/.*NEEDED.*\[\(.*\)\].*/\1/p'; } || true
    { objdump -p "$1" 2>/dev/null | awk '/NEEDED/ {print $2}'; } || true
}
elf_versions() {
    { readelf --dyn-syms "$1" 2>/dev/null | grep -o "GLIBC_[0-9.]*"; } || true
    { objdump -T "$1" 2>/dev/null | grep -o "GLIBC_[0-9.]*"; } || true
}

# Hard or soft float, from the ARM ELF header's own flag: bit 0x400 of
# `e_flags` is `EF_ARM_ABI_FLOAT_HARD` and 0x200 is `EF_ARM_ABI_FLOAT_SOFT`.
# Read with `od`, the tenth 32-bit word of a little-endian ELF32 header.
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

    # Driven as a raw linker: the C runtime objects are named explicitly and in
    # order. Everything else is reached through the sysroot's own `libc.so`
    # script, rewritten above.
    SYSROOT_FLAGS="-C linker-flavor=ld.lld"
    SYSROOT_FLAGS="$SYSROOT_FLAGS -C link-arg=--dynamic-linker=/lib/$(abi_loader "$abi")"
    SYSROOT_FLAGS="$SYSROOT_FLAGS -C link-arg=-L$SYSROOT/lib -C link-arg=-L$SYSROOT/usr-lib"

    # Not a PIE. Every executable on these firmwares is `ET_EXEC`, and a
    # position-independent executable dies inside the older loader. `crt1.o`
    # with it, `Scrt1.o` being the shared-style start file.
    SYSROOT_FLAGS="$SYSROOT_FLAGS -C relocation-model=static -C link-arg=-no-pie"

    # Lazy binding, which the device's own loader takes. Eager relocation
    # segfaults inside it.
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

    # These five are on every device's `/lib`; any name beyond them is fatal —
    # a binary naming a library the device lacks never starts.
    NEEDED=$(elf_needed "$BIN" | sort -u)
    # A dynamically linked binary always names libc: an empty list means the
    # file was not read.
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

    # **The float ABI, read back out of the file produced.** It decides
    # whether the file can start at all.
    got=$(elf_float_abi "$BIN")
    want=$(abi_float "$abi")
    [ "$got" = "$want" ] || {
        echo "error: the $abi build came out $got-float, not $want-float" >&2
        echo "       a Kindle has one loader or the other, and the wrong one" >&2
        echo "       reaches the device as a tile that does nothing" >&2
        exit 1
    }

    # The oldest glibc that can run it, decided by the sysroot. A build log
    # carries it.
    NEEDS=$(elf_versions "$BIN" | sort -uV | tail -1)
    echo "==> $(abi_binary "$abi"): $got-float, /lib/$(abi_loader "$abi"), ${NEEDS:-an unknown glibc} or newer"
}

for abi in $ABIS; do
    build_abi "$abi"
done

echo "==> Staging $EXT"

# hid/ and fonts/ hold fetched files; config.ini is tracked.
if [ -d "$EXT/hid" ]; then
    find "$EXT/hid" -mindepth 1 -maxdepth 1 ! -name config.ini -exec rm -rf {} +
fi
rm -rf "$EXT/fonts"
mkdir -p "$EXT/bin" "$EXT/hid" "$EXT/var"

for abi in $ABIS; do
    cp "$ROOT/target/$(abi_target "$abi")/release/karyll" "$EXT/bin/$(abi_binary "$abi")"
    chmod 755 "$EXT/bin/$(abi_binary "$abi")"
done

# The KUAL entry's version, held at Cargo.toml's. config.xml and menu.json are
# written by hand and tracked, and this field is all a release changes in them.
# Written through a second file: `sed -i` differs on the two systems here.
sed "s|<version>[^<]*</version>|<version>$VERSION</version>|" \
    "$EXT/config.xml" > "$EXT/config.xml.new"
mv "$EXT/config.xml.new" "$EXT/config.xml"

# The Bluetooth stack: downloaded once, cached in deploy/hid, pinned by
# version and checksum. The device fetches nothing.
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
# ./config.ini in the tarball collides with the tracked $EXT/hid/config.ini.
# ./koreader-plugin holds a KOReader Lua plugin.
tar xzf "$TARBALL" -C "$EXT/hid" \
    --exclude="./koreader-plugin" --exclude="./config.ini"
# The release ships no LICENSE; this copies one beside the binaries.
if [ -f "$ROOT/LICENSE" ]; then
    cp "$ROOT/LICENSE" "$EXT/hid/LICENSE"
fi
printf 'kindle-hid-passthrough %s\nsource: https://github.com/zampierilucas/kindle-hid-passthrough\ntarball sha256: %s\nkoreader-plugin/ omitted; LICENSE added (upstream ships none, and it is GPLv3).\n' \
    "$HID_VERSION" "$HID_SHA256" > "$EXT/hid/PROVENANCE"

# The writing faces. No Kindle carries a monospace text face. Fetched, pinned
# to a commit, and checksummed per file.
FONTS_COMMIT=f32c04c3058a75d7ce28919ce70fe8800817491b
FONTS_CACHE="$ROOT/deploy/fonts"
FONTS_RAW="https://raw.githubusercontent.com/iaolo/iA-Fonts/$FONTS_COMMIT"

# sha256, the directory under the repository root, and the file. Only the
# static cuts: the variable files carry no bold, nothing here sets a variation
# axis, and they draw one weight under four names.
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
# The heredoc keeps the loop in this shell: a failed checksum exits the
# script.
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
echo "==> staged $(du -sh "$EXT" | awk '{print $1}') -> device/extensions/karyll"

cat <<'EOF'

==> install — copy these two onto the device

    device/extensions/karyll/   ->  /mnt/us/extensions/karyll/
    device/documents/Karyll.sh  ->  /mnt/us/documents/Karyll.sh
EOF
