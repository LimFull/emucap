#!/usr/bin/env python3
"""Compile the actual patched control I/O header; no emulator launch is needed."""
import argparse
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--source", type=Path, default=root / "adapters/mame-pc98/work/mame-src")
args = parser.parse_args()
patch = (root / "adapters/mame-pc98/patches/0016-bound-native-control-io.patch").read_text()
section = patch.split("+++ b/src/frontend/mame/emucap_control_io.h\n", 1)[1]
header = "\n".join(line[1:] for line in section.splitlines() if line.startswith("+")) + "\n"
with tempfile.TemporaryDirectory(prefix="mame-control-io-") as name:
    work = Path(name)
    header_dir = work / "src/frontend/mame"
    header_dir.mkdir(parents=True)
    (header_dir / "emucap_control_io.h").write_text(header)
    subprocess.run([
        "git", "apply", "--include=src/frontend/mame/emucap_control_io.h",
        str(root / "adapters/mame-pc98/patches/0019-authenticate-native-control.patch")
    ], cwd=work, check=True)
    binary = work / "control-test"
    common = ["c++", "-std=c++20", "-O1", "-g", "-pthread", "-DLSB_FIRST=1",
              "-fsanitize=address,undefined", "-fno-sanitize-recover=all",
              "-I" + str(header_dir), "-I" + str(args.source / "src/osd"),
              "-I" + str(args.source / "src/lib"),
              "-I" + str(args.source / "src/lib/util"),
              "-I" + str(args.source / "src/emu"),
              "-I" + str(args.source / "3rdparty/asio/include")]
    hashing = work / "hashing.o"
    subprocess.run(common + ["-Wall", "-Wextra", "-c",
                    str(args.source / "src/lib/util/hashing.cpp"), "-o", str(hashing)], check=True)
    strformat = work / "strformat.o"
    subprocess.run(common + ["-Wall", "-Wextra", "-c",
                    str(args.source / "src/lib/util/strformat.cpp"), "-o", str(strformat)], check=True)
    subprocess.run(common + ["-Wall", "-Wextra", "-Werror",
                    str(root / "_tests/native/mame-control-io-test.cpp"), str(hashing), str(strformat),
                    "-lz", "-o", str(binary)], check=True)
    subprocess.run([str(binary), str(work)], check=True, timeout=20)
