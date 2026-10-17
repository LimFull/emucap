#include "emucap_native_failure.h"

#include <cassert>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

#ifdef _WIN32
#include <aclapi.h>
#include <direct.h>
#include <io.h>
#include <process.h>
#include <windows.h>
#else
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace {

#ifdef _WIN32
std::vector<unsigned char> current_token_user() {
  HANDLE token = nullptr;
  assert(::OpenProcessToken(::GetCurrentProcess(), TOKEN_QUERY, &token));
  DWORD bytes = 0;
  (void)::GetTokenInformation(token, TokenUser, nullptr, 0, &bytes);
  assert(::GetLastError() == ERROR_INSUFFICIENT_BUFFER);
  std::vector<unsigned char> buffer(bytes);
  assert(::GetTokenInformation(token, TokenUser, buffer.data(), bytes, &bytes));
  ::CloseHandle(token);
  return buffer;
}

void assert_private_file(const std::wstring& path) {
  std::vector<unsigned char> token_user = current_token_user();
  PSID expected_sid = reinterpret_cast<TOKEN_USER*>(token_user.data())->User.Sid;
  PSID owner = nullptr;
  PACL dacl = nullptr;
  PSECURITY_DESCRIPTOR descriptor = nullptr;
  assert(::GetNamedSecurityInfoW(
             const_cast<wchar_t*>(path.c_str()),
             SE_FILE_OBJECT,
             OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
             &owner,
             nullptr,
             &dacl,
             nullptr,
             &descriptor)
      == ERROR_SUCCESS);
  assert(descriptor != nullptr);
  assert(owner != nullptr && ::EqualSid(owner, expected_sid));
  assert(dacl != nullptr && dacl->AceCount == 1);
  SECURITY_DESCRIPTOR_CONTROL control = 0;
  DWORD revision = 0;
  assert(::GetSecurityDescriptorControl(descriptor, &control, &revision));
  assert((control & SE_DACL_PROTECTED) != 0);
  void* raw_ace = nullptr;
  assert(::GetAce(dacl, 0, &raw_ace));
  const ACCESS_ALLOWED_ACE* ace = static_cast<const ACCESS_ALLOWED_ACE*>(raw_ace);
  assert(ace->Header.AceType == ACCESS_ALLOWED_ACE_TYPE);
  assert(ace->Header.AceFlags == 0);
  assert(ace->Mask == FILE_ALL_ACCESS);
  assert(::EqualSid(const_cast<DWORD*>(&ace->SidStart), expected_sid));
  ::LocalFree(descriptor);
}
#endif

int process_id() {
#ifdef _WIN32
  return ::_getpid();
#else
  return static_cast<int>(::getpid());
#endif
}

#ifndef _WIN32
int make_dir(const std::string& path) {
  return ::mkdir(path.c_str(), 0700);
}

void remove_file(const std::string& path) {
  (void)::unlink(path.c_str());
}

void remove_dir(const std::string& path) {
  (void)::rmdir(path.c_str());
}
#endif

}  // namespace

int main() {
  EmucapNativeFailureSnapshot snapshot;
  snapshot.launch_id = "launch-test";
  snapshot.adapter = "mednafen-native";
  snapshot.emulator_build = "build-test";
  snapshot.content = "/content/test.cue";
  snapshot.operation = "service";
  snapshot.reason = "failure \"with\" newline\n";
  snapshot.execution_state = "unknown";
  snapshot.observed_at_unix_ms = 1730000000000ULL;
  snapshot.frame = 12345;
  snapshot.active = true;

  const std::string json = emucap_native_failure_json(snapshot);
  assert(json.size() <= EMUCAP_NATIVE_FAILURE_FILE_CAP);
  assert(json.find("\"kind\":\"adapter_internal_error\"") != std::string::npos);
  assert(json.find("\"operation\":\"service\"") != std::string::npos);
  assert(json.find("\"reason\":\"failure \\\"with\\\" newline\\n\"")
      != std::string::npos);
  assert(json.find("\"active\":true") != std::string::npos);
  assert(json.find("\"execution_state\":\"unknown\"") != std::string::npos);

  snapshot.reason.assign(EMUCAP_NATIVE_FAILURE_FILE_CAP * 2, 'x');
  const std::string bounded = emucap_native_failure_json(snapshot);
  assert(bounded.size() <= EMUCAP_NATIVE_FAILURE_FILE_CAP);
  assert(bounded.find("\"truncated\":true") != std::string::npos);

#ifdef _WIN32
  wchar_t temp_path[MAX_PATH + 1];
  const DWORD temp_size = GetTempPathW(sizeof(temp_path) / sizeof(temp_path[0]), temp_path);
  assert(temp_size > 0 && temp_size < sizeof(temp_path) / sizeof(temp_path[0]));
  const std::wstring root = std::wstring(temp_path) + L"emucap-native-failure-"
      + std::to_wstring(process_id());
  (void)::RemoveDirectoryW(root.c_str());
  assert(::CreateDirectoryW(root.c_str(), nullptr));
  const std::wstring output = root + L"\\adapter-failure-한글-測試.json";
  HANDLE old = ::CreateFileW(
      output.c_str(),
      GENERIC_WRITE,
      FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
      nullptr,
      CREATE_ALWAYS,
      FILE_ATTRIBUTE_NORMAL,
      nullptr);
  assert(old != INVALID_HANDLE_VALUE);
  const char old_bytes[] = "old";
  DWORD old_written = 0;
  assert(::WriteFile(old, old_bytes, sizeof(old_bytes) - 1, &old_written, nullptr));
  assert(old_written == sizeof(old_bytes) - 1);
  ::CloseHandle(old);
  std::string error;
  assert(emucap_write_native_failure_atomic_wide(output, json, &error));
  FILE* file = ::_wfopen(output.c_str(), L"rb");
  assert(file != nullptr);
  std::string read(json.size(), '\0');
  assert(std::fread(&read[0], 1, read.size(), file) == read.size());
  assert(std::fgetc(file) == EOF);
  assert(std::fclose(file) == 0);
  assert(read == json);
  assert_private_file(output);
  assert(::DeleteFileW(output.c_str()));
  assert(::RemoveDirectoryW(root.c_str()));
#else
  const char* tmp = std::getenv("TMPDIR");
  const std::string root =
      std::string(tmp != nullptr && tmp[0] != '\0' ? tmp : "/tmp")
      + "/emucap native failure $()-" + std::to_string(process_id());
  remove_dir(root);
  assert(make_dir(root) == 0);
  const std::string output = root + "/adapter-failure.json";
  std::string error;
  assert(emucap_write_native_failure_atomic(output, json, &error));

  std::ifstream file(output.c_str(), std::ios::binary);
  const std::string read(
      (std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
  assert(read == json);
  file.close();
  struct stat status {};
  assert(::stat(output.c_str(), &status) == 0);
  assert((status.st_mode & 0777) == 0600);
  remove_file(output);
  remove_dir(root);
#endif
  return 0;
}
