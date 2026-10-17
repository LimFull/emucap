#include "emucap_native_failure.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <sstream>
#include <string>
#include <vector>

#ifdef _WIN32
#include <aclapi.h>
#include <fcntl.h>
#include <io.h>
#include <windows.h>
#ifdef _MSC_VER
#pragma comment(lib, "Advapi32.lib")
#endif
#else
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace {

std::string json_escape(const std::string& value) {
  std::string escaped;
  escaped.reserve(value.size());
  for (unsigned char ch : value) {
    switch (ch) {
      case '"': escaped += "\\\""; break;
      case '\\': escaped += "\\\\"; break;
      case '\b': escaped += "\\b"; break;
      case '\f': escaped += "\\f"; break;
      case '\n': escaped += "\\n"; break;
      case '\r': escaped += "\\r"; break;
      case '\t': escaped += "\\t"; break;
      default:
        if (ch < 0x20) {
          char buffer[8];
          std::snprintf(buffer, sizeof(buffer), "\\u%04x", ch);
          escaped += buffer;
        } else {
          escaped += static_cast<char>(ch);
        }
    }
  }
  return escaped;
}

std::string bounded_text(
    const std::string& value,
    std::size_t cap,
    bool& truncated) {
  if (value.size() <= cap) return value;
  truncated = true;
  std::size_t cut = cap;
  while (cut > 0 && (static_cast<unsigned char>(value[cut]) & 0xC0) == 0x80)
    --cut;
  return value.substr(0, cut);
}

std::uint64_t unix_time_ms() {
  return static_cast<std::uint64_t>(
      std::chrono::duration_cast<std::chrono::milliseconds>(
          std::chrono::system_clock::now().time_since_epoch())
          .count());
}

#ifndef _WIN32
struct PathParts {
  std::string parent;
  std::string filename;
  char separator;
};

bool split_path(const std::string& path, PathParts& parts) {
  if (path.empty()) return false;
  const std::size_t slash = path.find_last_of("/\\");
  if (slash == std::string::npos) {
    parts.parent = ".";
    parts.filename = path;
    parts.separator = '/';
  } else {
    parts.parent = slash == 0 ? path.substr(0, 1) : path.substr(0, slash);
    parts.filename = path.substr(slash + 1);
    parts.separator = path[slash];
  }
  return !parts.filename.empty();
}
#endif

#ifdef _WIN32
struct WidePathParts {
  std::wstring parent;
  std::wstring filename;
  wchar_t separator;
};

bool split_path(const std::wstring& path, WidePathParts& parts) {
  if (path.empty()) return false;
  const std::size_t slash = path.find_last_of(L"/\\");
  if (slash == std::wstring::npos) {
    parts.parent = L".";
    parts.filename = path;
    parts.separator = L'\\';
  } else {
    parts.parent = slash == 0 ? path.substr(0, 1) : path.substr(0, slash);
    parts.filename = path.substr(slash + 1);
    parts.separator = path[slash];
  }
  return !parts.filename.empty();
}

bool utf8_to_wide(const std::string& value, std::wstring& wide) {
  if (value.empty()) {
    wide.clear();
    return true;
  }
  const int size = ::MultiByteToWideChar(
      CP_UTF8, MB_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()), nullptr, 0);
  if (size <= 0) return false;
  wide.resize(static_cast<std::size_t>(size));
  return ::MultiByteToWideChar(
             CP_UTF8,
             MB_ERR_INVALID_CHARS,
             value.data(),
             static_cast<int>(value.size()),
             &wide[0],
             size) == size;
}

class WindowsPrivateSecurity {
 public:
  ~WindowsPrivateSecurity() {
    if (token_ != nullptr) ::CloseHandle(token_);
  }

  bool initialize() {
    if (!::OpenProcessToken(::GetCurrentProcess(), TOKEN_QUERY, &token_)) return false;
    DWORD token_bytes = 0;
    (void)::GetTokenInformation(token_, TokenUser, nullptr, 0, &token_bytes);
    if (::GetLastError() != ERROR_INSUFFICIENT_BUFFER || token_bytes == 0) return false;
    token_user_.resize(token_bytes);
    if (!::GetTokenInformation(
            token_, TokenUser, token_user_.data(), token_bytes, &token_bytes))
      return false;
    sid_ = reinterpret_cast<TOKEN_USER*>(token_user_.data())->User.Sid;
    if (!::IsValidSid(sid_)) return false;

    const DWORD sid_bytes = ::GetLengthSid(sid_);
    const DWORD acl_bytes =
        sizeof(ACL) + sizeof(ACCESS_ALLOWED_ACE) - sizeof(DWORD) + sid_bytes;
    acl_.resize(acl_bytes);
    ACL* acl = reinterpret_cast<ACL*>(acl_.data());
    if (!::InitializeAcl(acl, acl_bytes, ACL_REVISION)
        || !::AddAccessAllowedAceEx(acl, ACL_REVISION, 0, FILE_ALL_ACCESS, sid_))
      return false;
    if (!::InitializeSecurityDescriptor(&descriptor_, SECURITY_DESCRIPTOR_REVISION)
        || !::SetSecurityDescriptorOwner(&descriptor_, sid_, FALSE)
        || !::SetSecurityDescriptorDacl(&descriptor_, TRUE, acl, FALSE)
        || !::SetSecurityDescriptorControl(
               &descriptor_, SE_DACL_PROTECTED, SE_DACL_PROTECTED))
      return false;

    attributes_.nLength = sizeof(attributes_);
    attributes_.lpSecurityDescriptor = &descriptor_;
    attributes_.bInheritHandle = FALSE;
    return true;
  }

  SECURITY_ATTRIBUTES* attributes() { return &attributes_; }
  PSID sid() const { return sid_; }

 private:
  HANDLE token_ = nullptr;
  std::vector<unsigned char> token_user_;
  std::vector<unsigned char> acl_;
  PSID sid_ = nullptr;
  SECURITY_DESCRIPTOR descriptor_{};
  SECURITY_ATTRIBUTES attributes_{};
};

bool private_descriptor(HANDLE file, PSID expected_sid) {
  PSID owner = nullptr;
  PACL dacl = nullptr;
  PSECURITY_DESCRIPTOR descriptor = nullptr;
  const DWORD status = ::GetSecurityInfo(
      file,
      SE_FILE_OBJECT,
      OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
      &owner,
      nullptr,
      &dacl,
      nullptr,
      &descriptor);
  if (status != ERROR_SUCCESS || descriptor == nullptr) return false;

  bool valid = owner != nullptr && ::EqualSid(owner, expected_sid) != FALSE
      && dacl != nullptr && dacl->AceCount == 1;
  SECURITY_DESCRIPTOR_CONTROL control = 0;
  DWORD revision = 0;
  valid = valid
      && ::GetSecurityDescriptorControl(descriptor, &control, &revision) != FALSE
      && (control & SE_DACL_PROTECTED) != 0;
  if (valid) {
    void* raw_ace = nullptr;
    valid = ::GetAce(dacl, 0, &raw_ace) != FALSE;
    if (valid) {
      const ACCESS_ALLOWED_ACE* ace = static_cast<const ACCESS_ALLOWED_ACE*>(raw_ace);
      PSID ace_sid = const_cast<DWORD*>(&ace->SidStart);
      valid = ace->Header.AceType == ACCESS_ALLOWED_ACE_TYPE
          && ace->Header.AceFlags == 0 && ace->Mask == FILE_ALL_ACCESS
          && ::EqualSid(ace_sid, expected_sid) != FALSE;
    }
  }
  ::LocalFree(descriptor);
  return valid;
}

bool private_path(const std::wstring& path) {
  WindowsPrivateSecurity security;
  if (!security.initialize()) return false;
  HANDLE file = ::CreateFileW(
      path.c_str(),
      READ_CONTROL,
      FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
      nullptr,
      OPEN_EXISTING,
      FILE_ATTRIBUTE_NORMAL,
      nullptr);
  if (file == INVALID_HANDLE_VALUE) return false;
  const bool valid = private_descriptor(file, security.sid());
  ::CloseHandle(file);
  return valid;
}

FILE* open_private_temp(const std::wstring& path) {
  WindowsPrivateSecurity security;
  if (!security.initialize()) return nullptr;
  HANDLE file = ::CreateFileW(
      path.c_str(),
      GENERIC_WRITE | READ_CONTROL,
      0,
      security.attributes(),
      CREATE_NEW,
      FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH,
      nullptr);
  if (file == INVALID_HANDLE_VALUE) return nullptr;
  if (!private_descriptor(file, security.sid())) {
    ::CloseHandle(file);
    (void)::DeleteFileW(path.c_str());
    return nullptr;
  }
  const int descriptor = ::_open_osfhandle(
      reinterpret_cast<std::intptr_t>(file), _O_BINARY | _O_WRONLY);
  if (descriptor < 0) {
    ::CloseHandle(file);
    (void)::DeleteFileW(path.c_str());
    return nullptr;
  }
  FILE* stream = ::_fdopen(descriptor, "wb");
  if (stream == nullptr) {
    ::_close(descriptor);
    (void)::DeleteFileW(path.c_str());
  }
  return stream;
}

bool atomic_replace(const std::wstring& source, const std::wstring& target) {
  if (!::MoveFileExW(
          source.c_str(),
          target.c_str(),
          MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH))
    return false;
  if (private_path(target)) return true;
  (void)::DeleteFileW(target.c_str());
  return false;
}

void remove_file(const std::wstring& path) {
  (void)::DeleteFileW(path.c_str());
}
#endif

#ifndef _WIN32
FILE* open_private_temp(const std::string& path) {
  const int fd = ::open(path.c_str(), O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC, 0600);
  return fd < 0 ? nullptr : ::fdopen(fd, "wb");
}
#endif

bool sync_file(FILE* file) {
  if (std::fflush(file) != 0) return false;
#ifdef _WIN32
  return ::_commit(::_fileno(file)) == 0;
#else
  return ::fsync(::fileno(file)) == 0;
#endif
}

#ifndef _WIN32
bool atomic_replace(
    const std::string& source,
    const std::string& target,
    const std::string& parent) {
  if (::rename(source.c_str(), target.c_str()) != 0) return false;
  const int dir = ::open(parent.c_str(), O_RDONLY | O_DIRECTORY | O_CLOEXEC);
  if (dir >= 0) {
    (void)::fsync(dir);
    ::close(dir);
  }
  return true;
}

void remove_file(const std::string& path) {
  (void)::unlink(path.c_str());
}
#endif

#ifdef _WIN32
bool write_native_failure_path(
    const std::wstring& path,
    const std::string& json,
    std::string* error) {
  if (path.empty() || json.size() > EMUCAP_NATIVE_FAILURE_FILE_CAP) {
    if (error != nullptr)
      *error = path.empty() ? "failure path is empty" : "failure JSON exceeds 128 KiB";
    return false;
  }
  WidePathParts parts;
  if (!split_path(path, parts)) {
    if (error != nullptr) *error = "failure path has no filename";
    return false;
  }

  static std::atomic<unsigned long long> sequence(0);
  const long long stamp =
      std::chrono::steady_clock::now().time_since_epoch().count();
  const std::wstring temporary =
      parts.parent + parts.separator + L"." + parts.filename + L"."
      + std::to_wstring(stamp) + L"." + std::to_wstring(sequence.fetch_add(1)) + L".tmp";
  FILE* file = open_private_temp(temporary);
  if (file == nullptr) {
    if (error != nullptr) *error = "cannot create private failure temp file";
    return false;
  }
  const bool wrote = std::fwrite(json.data(), 1, json.size(), file) == json.size();
  const bool synced = wrote && sync_file(file);
  const bool closed = std::fclose(file) == 0;
  if (!synced || !closed || !atomic_replace(temporary, path)) {
    remove_file(temporary);
    if (error != nullptr) *error = "cannot publish failure file atomically";
    return false;
  }
  return true;
}
#endif

}  // namespace

std::string emucap_native_failure_json(const EmucapNativeFailureSnapshot& snapshot) {
  bool truncated = false;
  const std::string launch_id = bounded_text(snapshot.launch_id, 128, truncated);
  const std::string adapter = bounded_text(snapshot.adapter, 64, truncated);
  const std::string emulator_build =
      bounded_text(snapshot.emulator_build, 128, truncated);
  const std::string content = bounded_text(snapshot.content, 1024, truncated);
  const std::string operation = bounded_text(snapshot.operation, 128, truncated);
  const std::string reason = bounded_text(snapshot.reason, 1024, truncated);
  const std::string execution_state =
      bounded_text(snapshot.execution_state, 16, truncated);

  std::ostringstream out;
  out << "{\"schema_version\":1"
      << ",\"launch_id\":\"" << json_escape(launch_id) << '"'
      << ",\"adapter\":\"" << json_escape(adapter) << '"'
      << ",\"emulator_build\":\"" << json_escape(emulator_build) << '"'
      << ",\"content\":\"" << json_escape(content) << '"'
      << ",\"kind\":\"adapter_internal_error\""
      << ",\"operation\":\"" << json_escape(operation) << '"'
      << ",\"reason\":\"" << json_escape(reason) << '"'
      << ",\"observed_at_unix_ms\":" << snapshot.observed_at_unix_ms
      << ",\"frame\":" << snapshot.frame
      << ",\"active\":" << (snapshot.active ? "true" : "false")
      << ",\"execution_state\":\"" << json_escape(execution_state) << '"'
      << ",\"truncated\":" << (truncated ? "true" : "false") << '}';
  return out.str();
}

bool emucap_write_native_failure_atomic(
    const std::string& path,
    const std::string& json,
    std::string* error) {
#ifdef _WIN32
  std::wstring wide;
  if (!utf8_to_wide(path, wide)) {
    if (error != nullptr) *error = "failure path is not valid UTF-8";
    return false;
  }
  return write_native_failure_path(wide, json, error);
#else
  if (path.empty() || json.size() > EMUCAP_NATIVE_FAILURE_FILE_CAP) {
    if (error != nullptr)
      *error = path.empty() ? "failure path is empty" : "failure JSON exceeds 128 KiB";
    return false;
  }

  PathParts parts;
  if (!split_path(path, parts)) {
    if (error != nullptr) *error = "failure path has no filename";
    return false;
  }

  static std::atomic<unsigned long long> sequence(0);
  const long long stamp =
      std::chrono::steady_clock::now().time_since_epoch().count();
  const std::string temporary =
      parts.parent + parts.separator + "." + parts.filename + "."
      + std::to_string(stamp) + "." + std::to_string(sequence.fetch_add(1)) + ".tmp";

  FILE* file = open_private_temp(temporary);
  if (file == nullptr) {
    if (error != nullptr) *error = "cannot create private failure temp file";
    return false;
  }
  const bool wrote = std::fwrite(json.data(), 1, json.size(), file) == json.size();
  const bool synced = wrote && sync_file(file);
  const bool closed = std::fclose(file) == 0;
  if (!synced || !closed || !atomic_replace(temporary, path, parts.parent)) {
    remove_file(temporary);
    if (error != nullptr) *error = "cannot publish failure file atomically";
    return false;
  }
  return true;
#endif
}

#ifdef _WIN32
bool emucap_write_native_failure_atomic_wide(
    const std::wstring& path,
    const std::string& json,
    std::string* error) {
  return write_native_failure_path(path, json, error);
}
#endif

bool emucap_publish_native_failure(
    const char* adapter,
    const char* emulator_build,
    std::uint64_t frame,
    const char* operation,
    const char* reason,
    bool active,
    const char* execution_state,
    std::string* error) {
#ifdef _WIN32
  const wchar_t* failure_path = ::_wgetenv(L"EMUCAP_FAILURE_FILE");
  if (failure_path == nullptr || failure_path[0] == L'\0') {
#else
  const char* failure_path = std::getenv("EMUCAP_FAILURE_FILE");
  if (failure_path == nullptr || failure_path[0] == '\0') {
#endif
    if (error != nullptr) *error = "EMUCAP_FAILURE_FILE is missing";
    return false;
  }

  EmucapNativeFailureSnapshot snapshot;
  const char* launch_id = std::getenv("EMUCAP_LAUNCH_ID");
  const char* content = std::getenv("EMUCAP_CONTENT");
  snapshot.launch_id = launch_id != nullptr ? launch_id : "";
  snapshot.adapter = adapter != nullptr ? adapter : "";
  snapshot.emulator_build = emulator_build != nullptr ? emulator_build : "";
  snapshot.content = content != nullptr ? content : "";
  snapshot.operation = operation != nullptr ? operation : "unknown";
  snapshot.reason = reason != nullptr ? reason : "unknown native adapter exception";
  snapshot.execution_state = execution_state != nullptr ? execution_state : "unknown";
  snapshot.observed_at_unix_ms = unix_time_ms();
  snapshot.frame = frame;
  snapshot.active = active;
#ifdef _WIN32
  return emucap_write_native_failure_atomic_wide(
      std::wstring(failure_path), emucap_native_failure_json(snapshot), error);
#else
  return emucap_write_native_failure_atomic(
      failure_path, emucap_native_failure_json(snapshot), error);
#endif
}
