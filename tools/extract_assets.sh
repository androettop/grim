#!/usr/bin/env bash
# Unpacks the original game disc into a tree laid out like an installed copy.
#
# Usage: tools/extract_assets.sh <disc image> [dest=game] [language]
#   <disc image>  an .iso (2048-byte sectors) or a raw .bin (Mode 1, 2352-byte sectors)
#   [language]    which of the disc's languages to install: int (English) when the disc has
#                 it, otherwise required; the script lists what the disc offers.
# Requires: python3, 7z, unshield.
#
# Checked against the Spanish (spa, ita, por) and the English/European (int, fre, ger, dut)
# discs: both carry the same InstallShield cabinet, with one component per language.
set -euo pipefail

IMAGE="${1:?usage: $0 <disc image> [dest] [language]}"
DEST="${2:-game}"
LANGUAGE="${3:-}"
WORK="$DEST/.work"
ROOT="$DEST/HP2"

mkdir -p "$WORK"

echo ">> disc image -> ISO"
python3 - "$IMAGE" "$WORK/hpcos.iso" <<'PY'
import shutil, sys
src = open(sys.argv[1], 'rb')
head = src.read(16)
src.seek(0)
# A raw sector starts with the CD sync pattern; an ISO starts with 32 KiB of zeros.
if head[:12] != b'\x00' + b'\xff' * 10 + b'\x00':
    shutil.copyfileobj(src, open(sys.argv[2], 'wb'))
    sys.exit()
dst = open(sys.argv[2], 'wb')
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

echo ">> InstallShield cabinet"
# unshield fails on an irrelevant Dummy.txt; the files that matter are checked below.
unshield -d "$WORK/cab" x "$WORK/iso/setup/data1.cab" >/dev/null || true
C="$WORK/cab"

# The component that holds a given file, by name.
component_of() {
  local hit
  hit=$(find "$C" -maxdepth 2 -type f -name "$1" -print -quit)
  [ -n "$hit" ] || { echo "$1 not found in the cabinet" >&2; exit 1; }
  dirname "$hit"
}

# Every language beside English ships a component with its own default.ini and HpMenu.<lang>.
available="int"
for f in "$C"/Component_*/HpMenu.*; do
  available="$available ${f##*.}"
done
if [ -z "$LANGUAGE" ]; then
  if find "$C" -maxdepth 2 -name AllDialog.uax | grep -q .; then
    LANGUAGE=int
  else
    echo "this disc has no English dialog; pass one of its languages: ${available#int }" >&2
    exit 1
  fi
fi
LANGUAGE=$(echo "$LANGUAGE" | tr 'A-Z' 'a-z')
case " $available " in
  *" $LANGUAGE "*) ;;
  *) echo "language '$LANGUAGE' is not on this disc; it has: $available" >&2; exit 1 ;;
esac
echo ">> language: $LANGUAGE"

if [ "$LANGUAGE" = int ]; then
  DIALOG=$(component_of AllDialog.uax)/AllDialog.uax
else
  UPPER=$(echo "$LANGUAGE" | tr 'a-z' 'A-Z')
  DIALOG=$(component_of "AllDialog.${UPPER}_uax")/AllDialog.${UPPER}_uax
fi

rm -rf "$ROOT"
mkdir -p "$ROOT"/{System,Maps,Music,Textures,Sounds,Extra}

cp -r "$(component_of Core.u)/." "$ROOT/System/"                       # system, English base
cp -r "$(component_of 00001PrivetIntro.int)/." "$ROOT/System/CUTSCENES/" # cutscene scripts
if [ "$LANGUAGE" != int ]; then
  cp -r "$(component_of "HpMenu.$LANGUAGE")/." "$ROOT/System/"
  # The installer runs on a case-insensitive filesystem: the language's default.ini
  # (Language=<lang>) replaces Default.ini.
  mv -f "$ROOT/System/default.ini" "$ROOT/System/Default.ini"
fi
cp "$WORK/iso/setup/hgame.u" "$ROOT/System/HGame.u"
cp -r "$(component_of Adv1Willow.unr)/." "$ROOT/Maps/"
cp -r "$(component_of Adv4Greenhouse_Music.ogg)/." "$ROOT/Music/"
cp -r "$(component_of HP2_Menu.utx)/." "$ROOT/Textures/"
cp "$DIALOG" "$ROOT/Sounds/AllDialog.uax"
# The other languages' dialog on the disc, kept to validate the parser.
find "$C" -maxdepth 2 -type f -name 'AllDialog.*_uax' ! -path "$DIALOG" -exec cp {} "$ROOT/Extra/" \;

# Project rule: keep data only; no original binaries (DLL, EXE, drivers) or KnowWonder VCS leftovers.
find "$ROOT" -type f \( -iname '*.dll' -o -iname '*.exe' -o -iname '*.sys' -o -iname '*.ex_' \
  -o -iname '*.scc' -o -iname '*.dsp' -o -iname '*.log' \) -delete

rm -rf "$WORK"
echo ">> done: $ROOT"
