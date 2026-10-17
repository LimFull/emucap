#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

constexpr std::size_t EMUCAP_NATIVE_FAILURE_FILE_CAP = 128 * 1024;

struct EmucapNativeFailureSnapshot {
  std::string launch_id;
  std::string adapter;
  std::string emulator_build;
  std::string content;
  std::string operation;
  std::string reason;
  std::string execution_state;
  std::uint64_t observed_at_unix_ms = 0;
  std::uint64_t frame = 0;
  bool active = true;
};

std::string emucap_native_failure_json(const EmucapNativeFailureSnapshot& snapshot);

bool emucap_write_native_failure_atomic(
    const std::string& path,
    const std::string& json,
    std::string* error = nullptr);

#ifdef _WIN32
bool emucap_write_native_failure_atomic_wide(
    const std::wstring& path,
    const std::string& json,
    std::string* error = nullptr);
#endif

bool emucap_publish_native_failure(
    const char* adapter,
    const char* emulator_build,
    std::uint64_t frame,
    const char* operation,
    const char* reason,
    bool active,
    const char* execution_state,
    std::string* error = nullptr);
