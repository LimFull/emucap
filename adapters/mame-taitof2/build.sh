#!/usr/bin/env bash
set -euo pipefail

# Taito F2 reuses the pinned MAME recipe and generic Lua debugger lifecycle, but
# owns a separate build tree so another MAME subset build cannot replace its driver.
HERE="$(cd "$(dirname "$0")" && pwd)"
export MAME_WORK="${MAME_WORK:-$HERE/work}"
export MAME_SOURCES="${MAME_SOURCES:-src/mame/taito/taito_f2.cpp}"
export MAME_BUILD_LABEL="${MAME_BUILD_LABEL:-MAME Taito F2}"
export MAME_WORK_OWNER_FILE="${MAME_WORK_OWNER_FILE:-.emucap-mame-taitof2-work}"
exec "$HERE/../mame-pc98/build.sh" "$@"
