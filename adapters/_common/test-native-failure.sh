#!/bin/sh
set -eu
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
OUT="${TMPDIR:-/tmp}/emucap-native-failure-test-$$"
WIN_OUT="${TMPDIR:-/tmp}/emucap-native-failure-windows-test-$$.exe"
trap 'rm -f "$OUT" "$WIN_OUT"' EXIT INT TERM
"${CXX:-c++}" -std=c++11 -Wall -Wextra -Werror \
  "$HERE/emucap_native_failure.cpp" "$HERE/emucap_native_failure_test.cpp" -o "$OUT"
"$OUT"

if [ "${EMUCAP_NATIVE_FAILURE_WINDOWS_CROSS:-0}" = "1" ]; then
  ZIG_BIN="${ZIG:-zig}"
  command -v "$ZIG_BIN" >/dev/null 2>&1 || {
    printf 'EMUCAP_NATIVE_FAILURE_WINDOWS_CROSS=1 requires zig\n' >&2
    exit 1
  }
  "$ZIG_BIN" c++ -target x86_64-windows-gnu -std=c++11 \
    -Wno-nullability-completeness \
    "$HERE/emucap_native_failure.cpp" "$HERE/emucap_native_failure_test.cpp" \
    -ladvapi32 -o "$WIN_OUT"
fi
