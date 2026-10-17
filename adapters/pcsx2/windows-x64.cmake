# Match vcvars' x64 target, including when the build host is ARM64 Windows.
set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_SYSTEM_PROCESSOR AMD64)
# Windows runs the x64 configure tools on both supported host architectures.
# Keep try_run and Qt's host-tool build enabled, as with MSVC platform builds.
set(CMAKE_CROSSCOMPILING FALSE)
