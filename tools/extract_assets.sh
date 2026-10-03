#!/usr/bin/env bash
# Unpacks the original CD image (raw BIN, Mode 1, 2352-byte sectors) into a tree equivalent
# to a Spanish install.
#
# Usage: tools/extract_assets.sh <path/to/Harry_Potter_CoS_Spa.bin> [dest=game]
# Requires: python3, 7z, unshield.
set -euo pipefail

BIN="${1:?usage: $0 <image.bin> [dest]}"
DEST="${2:-game}"
WORK="$DEST/.work"
ROOT="$DEST/HP2"

mkdir -p "$WORK"

echo ">> BIN -> ISO"
python3 - "$BIN" "$WORK/hpcos.iso" <<'PY'
import sys
src, dst = open(sys.argv[1], 'rb'), open(sys.argv[2], 'wb')
while True:
    s = src.read(2352)
    if len(s) < 2352:
        break
    if s[15] != 1:
        sys.exit(f"unexpected sector mode {s[15]}")
    dst.write(s[16:16 + 2048])
PY

echo ">> ISO -> setup/"
7z x -y -o"$WORK/iso" "$WORK/hpcos.iso" setup >/dev/null

echo ">> cabs InstallShield"
# unshield fails on an irrelevant Dummy.txt (Component_10); the files that matter are checked below.
unshield -d "$WORK/cab" x "$WORK/iso/setup/data1.cab" >/dev/null || true
for f in Component_5/Core.u Component_2/Adv1Willow.unr Component_8/AllDialog.SPA_uax; do
  [ -f "$WORK/cab/$f" ] || { echo "$f missing after unshield" >&2; exit 1; }
done

C="$WORK/cab"
rm -rf "$ROOT"
mkdir -p "$ROOT"/{System,Maps,Music,Textures,Sounds,Extra}

cp -r "$C/Component_5/." "$ROOT/System/"            # system (English base)
cp -r "$C/Component_6/." "$ROOT/System/CUTSCENES/"  # Spanish cutscene scripts
cp -r "$C/Component_7/." "$ROOT/System/"            # .spa
# The installer runs on a case-insensitive filesystem: Component_7's default.ini (Language=spa)
# replaces Default.ini.
mv -f "$ROOT/System/default.ini" "$ROOT/System/Default.ini"
cp "$WORK/iso/setup/hgame.u" "$ROOT/System/HGame.u"
cp -r "$C/Component_2/." "$ROOT/Maps/"
cp -r "$C/Component_3/." "$ROOT/Music/"
cp -r "$C/Component_4/." "$ROOT/Textures/"
cp "$C/Component_8/AllDialog.SPA_uax" "$ROOT/Sounds/AllDialog.uax"
# Other languages on the disc, kept to validate the parser.
cp "$C/Component_10/AllDialog.ITA_uax" "$C/Component_12/AllDialog.POR_uax" "$ROOT/Extra/"

# Project rule: keep data only; no original binaries (DLL, EXE, drivers) or KnowWonder VCS leftovers.
find "$ROOT" -type f \( -iname '*.dll' -o -iname '*.exe' -o -iname '*.sys' -o -iname '*.ex_' \
  -o -iname '*.scc' -o -iname '*.dsp' -o -iname '*.log' \) -delete

rm -rf "$WORK"
echo ">> done: $ROOT"
