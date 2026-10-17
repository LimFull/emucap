#ifndef EMUCAP_ATOMIC_FILE_H
#define EMUCAP_ATOMIC_FILE_H
#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <stdexcept>
#include <string>
#include <system_error>
#ifdef _WIN32
#include <aclapi.h>
#include <windows.h>
#include <vector>
#else
#include <cerrno>
#include <cstdio>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

// Same-directory publication. Errors before replacement preserve the destination;
// cleanup removes only the temporary file exclusively created by this invocation.
class EmucapAtomicFile {
 public:
  static void write(const std::string& path, const void* data, std::size_t size) {
    if (path.empty() || path.find('\0') != std::string::npos)
      throw std::runtime_error("invalid state output path");
    EmucapAtomicFile file(path);
    file.store(static_cast<const std::uint8_t*>(data), size);
    file.publish();
  }
 private:
#ifdef _WIN32
  using Path = std::wstring;
  HANDLE fd_ = INVALID_HANDLE_VALUE;
  class PrivateSecurity {
   public:
    ~PrivateSecurity() { if (token_ != nullptr) CloseHandle(token_); }
    void initialize() {
      if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token_)) fail("open process token");
      DWORD bytes = 0;
      (void)GetTokenInformation(token_, TokenUser, nullptr, 0, &bytes);
      if (GetLastError() != ERROR_INSUFFICIENT_BUFFER || bytes == 0)
        fail("size token user");
      token_user_.resize(bytes);
      if (!GetTokenInformation(token_, TokenUser, token_user_.data(), bytes, &bytes))
        fail("read token user");
      sid_ = reinterpret_cast<TOKEN_USER*>(token_user_.data())->User.Sid;
      const DWORD sid_bytes = GetLengthSid(sid_);
      const DWORD acl_bytes = sizeof(ACL) + sizeof(ACCESS_ALLOWED_ACE) - sizeof(DWORD) + sid_bytes;
      acl_.resize(acl_bytes);
      ACL* acl = reinterpret_cast<ACL*>(acl_.data());
      if (!InitializeAcl(acl, acl_bytes, ACL_REVISION)
          || !AddAccessAllowedAceEx(acl, ACL_REVISION, 0, FILE_ALL_ACCESS, sid_)
          || !InitializeSecurityDescriptor(&descriptor_, SECURITY_DESCRIPTOR_REVISION)
          || !SetSecurityDescriptorOwner(&descriptor_, sid_, FALSE)
          || !SetSecurityDescriptorDacl(&descriptor_, TRUE, acl, FALSE)
          || !SetSecurityDescriptorControl(&descriptor_, SE_DACL_PROTECTED, SE_DACL_PROTECTED))
        fail("build private state descriptor");
      attributes_.nLength = sizeof(attributes_);
      attributes_.lpSecurityDescriptor = &descriptor_;
      attributes_.bInheritHandle = FALSE;
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
  static bool private_descriptor(HANDLE file, PSID expected) {
    PSID owner = nullptr;
    PACL dacl = nullptr;
    PSECURITY_DESCRIPTOR descriptor = nullptr;
    const DWORD status = GetSecurityInfo(file, SE_FILE_OBJECT,
        OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
        &owner, nullptr, &dacl, nullptr, &descriptor);
    if (status != ERROR_SUCCESS || descriptor == nullptr) return false;
    bool valid = owner != nullptr && EqualSid(owner, expected) != FALSE
        && dacl != nullptr && dacl->AceCount == 1;
    SECURITY_DESCRIPTOR_CONTROL control = 0;
    DWORD revision = 0;
    valid = valid && GetSecurityDescriptorControl(descriptor, &control, &revision) != FALSE
        && (control & SE_DACL_PROTECTED) != 0;
    if (valid) {
      void* raw = nullptr;
      valid = GetAce(dacl, 0, &raw) != FALSE;
      if (valid) {
        const ACCESS_ALLOWED_ACE* ace = static_cast<const ACCESS_ALLOWED_ACE*>(raw);
        PSID ace_sid = const_cast<DWORD*>(&ace->SidStart);
        valid = ace->Header.AceType == ACCESS_ALLOWED_ACE_TYPE && ace->Header.AceFlags == 0
            && ace->Mask == FILE_ALL_ACCESS && EqualSid(ace_sid, expected) != FALSE;
      }
    }
    LocalFree(descriptor);
    return valid;
  }
  static Path native(const std::string& path) {
    const int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path.c_str(), -1, nullptr, 0);
    if (!count) fail("decode state output path");
    Path result(count, 0);
    if (!MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path.c_str(), -1, &result[0], count))
      fail("decode state output path");
    result.resize(count - 1);
    return result;
  }
  [[noreturn]] static void fail(const char* operation) {
    throw std::system_error(GetLastError(), std::system_category(), operation);
  }
  static void check_destination(const Path& path) {
    const auto attrs = GetFileAttributesW(path.c_str());
    if (attrs == INVALID_FILE_ATTRIBUTES) {
      if (GetLastError() != ERROR_FILE_NOT_FOUND && GetLastError() != ERROR_PATH_NOT_FOUND)
        fail("inspect state destination");
    } else if (attrs & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT))
      throw std::runtime_error("state destination must be a regular file");
  }
  PrivateSecurity security_;
#else
  using Path = std::string;
  int fd_ = -1;
  int parent_fd_ = -1;
  static Path native(const std::string& path) { return path; }
  [[noreturn]] static void fail(const char* operation) {
    throw std::system_error(errno, std::generic_category(), operation);
  }
  void check_destination(const Path& name) const {
    struct stat st;
    if (::fstatat(parent_fd_, name.c_str(), &st, AT_SYMLINK_NOFOLLOW)) {
      if (errno != ENOENT) fail("inspect state destination");
    } else if (!S_ISREG(st.st_mode))
      throw std::runtime_error("state destination must be a regular file");
  }
#endif
  Path destination_, temporary_;
  bool owned_ = false;

  explicit EmucapAtomicFile(const std::string& path) {
    try {
#ifdef _WIN32
      security_.initialize();
      destination_ = native(path);
      check_destination(destination_);
      const auto slash = path.find_last_of("/\\");
      const auto directory = slash == std::string::npos
          ? (path.size() > 1 && path[1] == ':' ? path.substr(0, 2) : std::string())
          : path.substr(0, slash + 1);
      const auto pid = GetCurrentProcessId();
#else
      const auto slash = path.find_last_of('/');
      const auto directory = slash == std::string::npos ? std::string(".")
          : slash == 0 ? std::string("/") : path.substr(0, slash);
      destination_ = slash == std::string::npos ? path : path.substr(slash + 1);
      if (destination_.empty() || destination_ == "." || destination_ == "..")
        throw std::runtime_error("invalid state output name");
      parent_fd_ = ::open(directory.c_str(), O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
      if (parent_fd_ < 0) fail("open state output directory");
      check_destination(destination_);
      const auto pid = getpid();
#endif
      static std::atomic<unsigned long long> serial{0};
      const auto tick = std::chrono::steady_clock::now().time_since_epoch().count();
      for (unsigned attempt = 0; attempt < 128; ++attempt) {
        temporary_ = native(
#ifdef _WIN32
            directory +
#endif
            ".emucap-state-" + std::to_string(pid) + "-"
            + std::to_string(tick) + "-" + std::to_string(serial.fetch_add(1)) + ".tmp");
#ifdef _WIN32
        fd_ = CreateFileW(temporary_.c_str(), GENERIC_WRITE | READ_CONTROL, 0,
                          security_.attributes(), CREATE_NEW,
                          FILE_ATTRIBUTE_NORMAL, nullptr);
        if (fd_ != INVALID_HANDLE_VALUE) {
          owned_ = true;
          if (!private_descriptor(fd_, security_.sid()))
            throw std::runtime_error("temporary state file is not private");
          return;
        }
        if (GetLastError() != ERROR_FILE_EXISTS && GetLastError() != ERROR_ALREADY_EXISTS)
          fail("create temporary state file");
#else
        fd_ = ::openat(parent_fd_, temporary_.c_str(),
                       O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
        if (fd_ >= 0) { owned_ = true; return; }
        if (errno != EEXIST) fail("create temporary state file");
#endif
      }
      throw std::runtime_error("temporary state filename collisions");
    } catch (...) {
      cleanup();
      throw;
    }
  }
  void cleanup() noexcept {
#ifdef _WIN32
    if (fd_ != INVALID_HANDLE_VALUE) CloseHandle(fd_);
    fd_ = INVALID_HANDLE_VALUE;
    if (owned_) DeleteFileW(temporary_.c_str());
#else
    if (fd_ >= 0) ::close(fd_);
    fd_ = -1;
    if (owned_ && parent_fd_ >= 0) ::unlinkat(parent_fd_, temporary_.c_str(), 0);
    if (parent_fd_ >= 0) ::close(parent_fd_);
    parent_fd_ = -1;
#endif
    owned_ = false;
  }
  ~EmucapAtomicFile() noexcept { cleanup(); }
  void store(const std::uint8_t* data, std::size_t size) {
    while (size) {
      const auto chunk = std::min<std::size_t>(size, 1024 * 1024);
#ifdef _WIN32
      DWORD done = 0;
      if (!WriteFile(fd_, data, DWORD(chunk), &done, nullptr)) fail("write state file");
#else
      const auto done = ::write(fd_, data, chunk);
      if (done < 0) { if (errno == EINTR) continue; fail("write state file"); }
#endif
      if (!done) throw std::runtime_error("state file write made no progress");
      data += done; size -= done;
    }
#ifdef _WIN32
    if (!FlushFileBuffers(fd_)) fail("sync state file");
    const auto handle = fd_; fd_ = INVALID_HANDLE_VALUE;
    if (!CloseHandle(handle)) fail("close state file");
#else
    while (::fsync(fd_)) { if (errno != EINTR) fail("sync state file"); }
    const auto handle = fd_; fd_ = -1;
    if (::close(handle)) fail("close state file");
#endif
  }
  void publish() {
    check_destination(destination_);
#ifdef _WIN32
    if (!MoveFileExW(temporary_.c_str(), destination_.c_str(),
                     MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) fail("publish state file");
    HANDLE published = CreateFileW(destination_.c_str(), READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr, OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
    if (published == INVALID_HANDLE_VALUE) fail("verify published state file");
    const bool is_private = private_descriptor(published, security_.sid());
    CloseHandle(published);
    if (!is_private) {
      DeleteFileW(destination_.c_str());
      throw std::runtime_error("published state file is not private");
    }
#else
    if (::renameat(parent_fd_, temporary_.c_str(), parent_fd_, destination_.c_str()))
      fail("publish state file");
    while (::fsync(parent_fd_)) { if (errno != EINTR) fail("sync state output directory"); }
#endif
    owned_ = false;
  }
  EmucapAtomicFile(const EmucapAtomicFile&) = delete;
  EmucapAtomicFile& operator=(const EmucapAtomicFile&) = delete;
};
#endif
