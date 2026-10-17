#!/usr/bin/env bash
# Build pinned desktop dependencies into an explicit prefix, without system installation.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/build-lock.sh"
. "$HERE/sdl.lock"
[ "$#" = 2 ] || { echo "usage: $0 PREFIX WORK" >&2; exit 2; }
for path in "$1" "$2"; do
  [ ! -L "$path" ] || { echo "ERROR: dependency directory must not be a symlink: $path" >&2; exit 1; }
  mkdir -p "$path"
done
PREFIX="$(cd "$1" && pwd -P)"
WORK="$(cd "$2" && pwd -P)"
[ "$PREFIX" != "$WORK" ] || { echo "ERROR: prefix and work directory must differ" >&2; exit 1; }
emucap_acquire_build_lock "$WORK/.build.lock" "PPSSPP SDL dependencies"
JOBS="${PPSSPP_JOBS:-2}"
sha256_path() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}
fetch_source() {
  local name="$1" url="$2" digest="$3" archive="$WORK/$1.tar.gz" temporary
  if [ ! -f "$archive" ]; then
    temporary="$(mktemp "$WORK/.download.XXXXXX")"
    if ! curl -fL --retry 2 --connect-timeout 30 -o "$temporary" "$url"; then
      rm -f "$temporary"; return 1
    fi
    if [ "$(sha256_path "$temporary")" != "$digest" ]; then
      rm -f "$temporary"; echo "ERROR: dependency digest mismatch: $name" >&2; return 1
    fi
    mv "$temporary" "$archive"
  fi
  [ "$(sha256_path "$archive")" = "$digest" ] || { echo "ERROR: cached dependency digest mismatch: $name" >&2; return 1; }
  mkdir -p "$WORK/$name"
  tar -xzf "$archive" -C "$WORK/$name" --strip-components=1
}
fetch_source "SDL3-$SDL3_VERSION" "$SDL3_ARCHIVE_URL" "$SDL3_ARCHIVE_SHA256"
fetch_source "SDL3_ttf-$SDL3_TTF_VERSION" "$SDL3_TTF_ARCHIVE_URL" "$SDL3_TTF_ARCHIVE_SHA256"
cmake -S "$WORK/SDL3-$SDL3_VERSION" -B "$WORK/sdl-build" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$PREFIX" -DCMAKE_INSTALL_LIBDIR=lib \
  -DSDL_SHARED=ON -DSDL_STATIC=OFF -DSDL_TESTS=OFF -DSDL_TEST_LIBRARY=OFF
cmake --build "$WORK/sdl-build" -j "$JOBS"
cmake --install "$WORK/sdl-build"
cmake -S "$WORK/SDL3_ttf-$SDL3_TTF_VERSION" -B "$WORK/ttf-build" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$PREFIX" -DCMAKE_INSTALL_LIBDIR=lib \
  -DCMAKE_PREFIX_PATH="$PREFIX" -DBUILD_SHARED_LIBS=ON -DSDLTTF_VENDORED=OFF \
  -DSDLTTF_STRICT=ON -DSDLTTF_HARFBUZZ=ON -DSDLTTF_PLUTOSVG=OFF -DSDLTTF_SAMPLES=OFF
cmake --build "$WORK/ttf-build" -j "$JOBS"
cmake --install "$WORK/ttf-build"
mkdir -p "$PREFIX/share/licenses/SDL3" "$PREFIX/share/licenses/SDL3_ttf"
cp "$WORK/SDL3-$SDL3_VERSION/LICENSE.txt" "$PREFIX/share/licenses/SDL3/LICENSE.txt"
cp "$WORK/SDL3_ttf-$SDL3_TTF_VERSION/LICENSE.txt" "$PREFIX/share/licenses/SDL3_ttf/LICENSE.txt"
cp "$HERE/sdl.lock" "$PREFIX/emucap-sdl.lock"
printf 'built PPSSPP desktop dependencies: %s\n' "$PREFIX"
