#pragma once
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <vector>
#include <cstdio>
#include <memory>
#include <stdexcept>
#include <fcntl.h>
#include <sys/stat.h>
#ifdef _WIN32
#include <io.h>
#else
#include <unistd.h>
#endif

// Managed states carry the interpreter continuation omitted by upstream raw states.
// Header fields are little-endian; the native payload retains its upstream encoding.
constexpr std::size_t EMUCAP_FLYCAST_STATE_HEADER = 24;
// Managed artifact resource policy, shared by capture and restore.
constexpr std::size_t EMUCAP_FLYCAST_STATE_MAX = 512ULL * 1024 * 1024;
inline bool emucap_state_payload_size_valid(std::size_t size) {
    return size >= 8 && size <= EMUCAP_FLYCAST_STATE_MAX - EMUCAP_FLYCAST_STATE_HEADER;
}
inline void emucap_state_put(std::uint8_t* p, std::uint64_t value, unsigned count) {
    for (unsigned i = 0; i < count; ++i) p[i] = static_cast<std::uint8_t>(value >> (8 * i));
}
inline std::uint64_t emucap_state_get(const std::uint8_t* p, unsigned count) {
    std::uint64_t value = 0;
    for (unsigned i = 0; i < count; ++i) value |= std::uint64_t(p[i]) << (8 * i);
    return value;
}
inline void emucap_state_header(std::vector<std::uint8_t>& bytes, std::uint32_t unit,
                                std::uint32_t memory_ops) {
    std::memcpy(bytes.data(), "EMUCAPFC", 8);
    emucap_state_put(bytes.data() + 8, unit, 4);
    emucap_state_put(bytes.data() + 12, memory_ops, 4);
    emucap_state_put(bytes.data() + 16, bytes.size() - EMUCAP_FLYCAST_STATE_HEADER, 8);
}
inline bool emucap_state_parse_header(const std::uint8_t* bytes, std::size_t size,
                                      std::uint32_t& unit, std::uint32_t& memory_ops) {
    if (size < EMUCAP_FLYCAST_STATE_HEADER
        || !emucap_state_payload_size_valid(size - EMUCAP_FLYCAST_STATE_HEADER)
        || std::memcmp(bytes, "EMUCAPFC", 8) != 0)
        return false;
    const auto parsed_unit = emucap_state_get(bytes + 8, 4);
    const auto parsed_memory = emucap_state_get(bytes + 12, 4);
    if (parsed_unit > 5 || parsed_memory > 4
        || emucap_state_get(bytes + 16, 8) != size - EMUCAP_FLYCAST_STATE_HEADER)
        return false;
    unit = static_cast<std::uint32_t>(parsed_unit);
    memory_ops = static_cast<std::uint32_t>(parsed_memory);
    return true;
}
inline bool emucap_state_parse(const std::vector<std::uint8_t>& bytes,
                               std::uint32_t& unit, std::uint32_t& memory_ops) {
    return emucap_state_parse_header(bytes.data(), bytes.size(), unit, memory_ops);
}

inline std::vector<std::uint8_t> emucap_state_read(const char* path) {
#ifdef _WIN32
    const int fd = _open(path, _O_RDONLY | _O_BINARY | _O_NOINHERIT);
    if (fd < 0) throw std::runtime_error("failed to open state file");
    FILE* raw = _fdopen(fd, "rb");
    if (!raw) { _close(fd); throw std::runtime_error("failed to open state stream"); }
#else
    const int fd = open(path, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (fd < 0) throw std::runtime_error("failed to open state file");
    FILE* raw = fdopen(fd, "rb");
    if (!raw) { close(fd); throw std::runtime_error("failed to open state stream"); }
#endif
    std::unique_ptr<FILE, int(*)(FILE*)> file(raw, fclose);
#ifdef _WIN32
    struct _stat64 st;
    const bool regular = _fstat64(fd, &st) == 0 && (st.st_mode & _S_IFMT) == _S_IFREG;
#else
    struct stat st;
    const bool regular = fstat(fd, &st) == 0 && S_ISREG(st.st_mode);
#endif
    if (!regular || st.st_size <= static_cast<std::int64_t>(EMUCAP_FLYCAST_STATE_HEADER)
        || static_cast<std::uint64_t>(st.st_size) > EMUCAP_FLYCAST_STATE_MAX)
        throw std::runtime_error("state must be a regular file within the 512 MiB limit");
    const auto size = static_cast<std::size_t>(st.st_size);
    std::uint8_t header[EMUCAP_FLYCAST_STATE_HEADER];
    if (fread(header, 1, sizeof(header), raw) != sizeof(header))
        throw std::runtime_error("failed to read state header");
    std::uint32_t unit, memory;
    if (!emucap_state_parse_header(header, size, unit, memory))
        throw std::invalid_argument("state lacks a valid interpreter timing continuation");
    std::vector<std::uint8_t> bytes(size);
    std::memcpy(bytes.data(), header, sizeof(header));
    const auto payload = size - sizeof(header);
    if (fread(bytes.data() + sizeof(header), 1, payload, raw) != payload
        || fgetc(raw) != EOF || ferror(raw))
        throw std::runtime_error("state size changed or read failed");
    return bytes;
}
