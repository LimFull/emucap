#!/usr/bin/env bash
# Build the pinned Azahar libretro core (software renderer only) with the emucap debug ABI.
#
# The archive is the upstream unified source release, which carries every submodule. Patches
# apply to a fresh extract on each build; the CMake build directory is reused.
#
# Optional environment:
#   EMUCAP_AZAHAR_WORK=/path/to/worktree   default: adapters/azahar-3ds/work
#   AZAHAR_JOBS=<n>                        parallel build jobs (default: CPU count)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../_common/build-lock.sh"
. "$HERE/../_common/build-env.sh"
emucap_scrub_build_env
. "$HERE/upstream.lock"

WORK_INPUT="${EMUCAP_AZAHAR_WORK:-$HERE/work}"
[ ! -L "$WORK_INPUT" ] || { echo "ERROR: Azahar work path must not be a symlink: $WORK_INPUT" >&2; exit 1; }
mkdir -p "$WORK_INPUT"
WORK="$(cd "$WORK_INPUT" && pwd -P)"
OWNER="$WORK/.emucap-azahar-work"
if [ ! -f "$OWNER" ] && [ -n "$(find "$WORK" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
  echo "ERROR: Azahar work directory is not empty or emucap-owned: $WORK" >&2
  exit 2
fi
emucap_acquire_build_lock "$WORK/.build.lock" "Azahar 3DS"
: >"$OWNER"

SRC="$WORK/src"
BUILD="$WORK/build"
ARCHIVE="$WORK/$(basename "$AZAHAR_ARCHIVE_URL")"
JOBS="${AZAHAR_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}"
sha256() { if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$@"; else sha256sum "$@"; fi; }

[ -f "$ARCHIVE" ] || { echo "-> Downloading Azahar $AZAHAR_VERSION source"; curl -fsSL -o "$ARCHIVE" "$AZAHAR_ARCHIVE_URL"; }
[ "$(sha256 "$ARCHIVE" | awk '{print $1}')" = "$AZAHAR_ARCHIVE_SHA256" ] || {
  echo "ERROR: Azahar source archive checksum mismatch" >&2; exit 1; }

PATCHES=()
while IFS= read -r patch; do PATCHES+=("$patch"); done < <(find "$HERE/patches" -type f -name '*.patch' | LC_ALL=C sort)
[ "$(cat "${PATCHES[@]}" | sha256 | awk '{print $1}')" = "$AZAHAR_PATCHSET_SHA256" ] || {
  echo "ERROR: Azahar patch stack does not match upstream.lock" >&2; exit 1; }

echo "-> Extracting fresh source"
rm -rf "$SRC"
mkdir -p "$SRC"
tar xf "$ARCHIVE" -C "$SRC" --strip-components=1
for patch in "${PATCHES[@]}"; do
  echo "-> Applying $(basename "$patch")"
  patch -d "$SRC" -p1 <"$patch"
done

echo "-> Building Azahar $AZAHAR_VERSION libretro core"
# Vulkan and OpenGL stay off: the emucap host has no GPU context, so the core selects its
# software renderer and presents through the software framebuffer callback.
cmake -S "$SRC" -B "$BUILD" -G Ninja -DCMAKE_BUILD_TYPE=Release -DENABLE_LIBRETRO=ON \
  -DENABLE_VULKAN=OFF -DENABLE_OPENGL=OFF -DENABLE_TESTS=OFF -DENABLE_LTO=OFF \
  -DCITRA_WARNINGS_AS_ERRORS=OFF
cmake --build "$BUILD" --target azahar_libretro -j "$JOBS"

CORE="$(find "$BUILD/bin" -type f \( -name 'azahar_libretro.dylib' -o -name 'azahar_libretro.so' -o -name 'azahar_libretro.dll' \) | head -n 1)"
[ -n "$CORE" ] || { echo "ERROR: built Azahar libretro core not found under $BUILD/bin" >&2; exit 1; }
STAGED="$WORK/$(basename "$CORE")"
cp "$CORE" "$STAGED.tmp" && mv "$STAGED.tmp" "$STAGED"
cat >"$WORK/emucap-azahar-build.json.tmp" <<JSON
{
  "version": "$AZAHAR_VERSION",
  "archive_sha256": "$AZAHAR_ARCHIVE_SHA256",
  "patchset_sha256": "$AZAHAR_PATCHSET_SHA256",
  "core": "$(basename "$STAGED")",
  "core_sha256": "$(sha256 "$STAGED" | awk '{print $1}')"
}
JSON
mv "$WORK/emucap-azahar-build.json.tmp" "$WORK/emucap-azahar-build.json"
echo "Azahar 3DS core ready: $STAGED"
