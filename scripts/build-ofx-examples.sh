#!/usr/bin/env bash
# Builds the open-source OpenFX example plug-ins (https://github.com/AcademySoftwareFoundation/openfx,
# BSD-3-Clause) into target/ofx-examples as .ofx.bundle folders, for the OFX host tests.
# Needs git and a C++17 compiler. Usage: scripts/build-ofx-examples.sh [output-dir]
set -euo pipefail
OUT="${1:-$(cd "$(dirname "$0")/.." && pwd)/target/ofx-examples}"
SRC="${OFX_SRC:-$OUT/../ofx-src}"
REV="${OFX_REV:-}"
if [ ! -d "$SRC/.git" ]; then
  git clone --quiet https://github.com/AcademySoftwareFoundation/openfx.git "$SRC"
fi
[ -n "$REV" ] && git -C "$SRC" checkout --quiet "$REV"
case "$(uname -s)" in
  Linux) ARCH="Linux-x86-64"; [ "$(uname -m)" = "aarch64" ] && ARCH="Linux-aarch64"; EXT="so"; LINK="-shared -fPIC" ;;
  Darwin) ARCH="MacOS"; EXT="dylib"; LINK="-dynamiclib -fPIC" ;;
  *) echo "unsupported platform for this script (use MSYS2/MinGW on Windows)"; exit 1 ;;
esac
CXX="${CXX:-c++}"
FLAGS="-std=c++17 -O2 -w -I$SRC/include -I$SRC/Support/include -I$SRC/Support/Plugins/include -I$SRC/Examples/include"
build() { # name sources...
  local name="$1"; shift
  local dir="$OUT/$name.ofx.bundle/Contents/$ARCH"
  mkdir -p "$dir"
  if $CXX $FLAGS $LINK "$@" -o "$dir/$name.ofx" 2>/dev/null; then echo "built  $name"; else echo "FAILED $name"; rm -rf "$OUT/$name.ofx.bundle"; fi
}
# raw C API examples
for p in Invert Basic Rectangle ChoiceParams DepthConverter; do build "$p" "$SRC"/Examples/$p/*.cpp; done
# examples written with the OpenFX C++ support library
for p in Invert Generator Transition Retimer Field; do build "S$p" "$SRC"/Support/Plugins/$p/*.cpp "$SRC"/Support/Library/*.cpp; done
echo "plug-ins in $OUT"
