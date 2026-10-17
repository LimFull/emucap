#!/usr/bin/env python3
"""Compile the patched PCSX2 PINE authentication state machine with sanitizers."""

import argparse
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument(
    "--source",
    type=Path,
    default=root / "adapters/pcsx2/work/upgrade-build/pcsx2",
    help="clean or patched pinned PCSX2 source tree",
)
args = parser.parse_args()

patch = (root / "adapters/pcsx2/patches/0020-authenticate-pine-control.patch").read_text()
section = patch.split("+++ b/pcsx2/EmuCapPineAuth.h\n", 1)[1].split("diff --git ", 1)[0]
header = "\n".join(line[1:] for line in section.splitlines() if line.startswith("+")) + "\n"

with tempfile.TemporaryDirectory(prefix="pcsx2-pine-auth-") as directory:
    work = Path(directory)
    (work / "EmuCapPineAuth.h").write_text(header)
    include = args.source / "3rdparty/lzma/include"
    sources = args.source / "3rdparty/lzma/src"
    objects = []
    for name in ["Sha256.c", "Sha256Opt.c", "CpuArch.c"]:
        obj = work / f"{name}.o"
        subprocess.run(
            ["cc", "-D_7ZIP_ST", "-I", str(include), "-c", str(sources / name), "-o", str(obj)],
            check=True,
        )
        objects.append(obj)
    binary = work / "pine-auth-test"
    subprocess.run(
        [
            "c++",
            "-std=c++17",
            "-O1",
            "-g",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fsanitize=address,undefined",
            "-fno-sanitize-recover=all",
            "-I",
            str(work),
            "-I",
            str(include),
            str(root / "_tests/native/ps2-pine-auth-test.cpp"),
            *map(str, objects),
            "-o",
            str(binary),
        ],
        check=True,
    )
    subprocess.run([str(binary)], check=True)

    patch_lines = patch.splitlines()
    start = next(index for index, line in enumerate(patch_lines) if line == "+namespace")
    end = next(
        index
        for index in range(start, len(patch_lines))
        if patch_lines[index] == " void PINEServer::MainLoop()"
    )
    native_block = "\n".join(
        line[1:]
        for line in patch_lines[start:end]
        if line.startswith(("+", " "))
    )
    admission_source = work / "pine-admission.cpp"
    admission_source.write_text(
        """
#include "EmuCapPineAuth.h"
#include <algorithm>
#include <array>
#include <cassert>
#include <cerrno>
#include <chrono>
#include <cstdint>
#include <string_view>
#include <sys/socket.h>
#include <fcntl.h>
#include <unistd.h>
#include <vector>
using u8 = std::uint8_t;
#define read_portable(a, b, c) read(a, b, c)
#define write_portable(a, b, c) write(a, b, c)
static constexpr std::string_view ProductionToken =
    "native-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
"""
        + native_block
        + "\n"
        + (root / "_tests/native/ps2-pine-admission-test.cpp").read_text()
    )
    admission_binary = work / "pine-admission-test"
    subprocess.run(
        [
            "c++",
            "-std=c++17",
            "-O1",
            "-g",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fsanitize=address,undefined",
            "-fno-sanitize-recover=all",
            "-I",
            str(work),
            "-I",
            str(include),
            str(admission_source),
            *map(str, objects),
            "-o",
            str(admission_binary),
        ],
        check=True,
    )
    subprocess.run([str(admission_binary)], check=True)

print("PCSX2 PINE authentication and actual admission helpers passed ASan/UBSan")
