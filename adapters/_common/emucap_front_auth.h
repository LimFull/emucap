#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <string>
#include <string_view>

namespace emucap_front_auth {

constexpr std::size_t kChallengeSize = 25;
constexpr std::size_t kProducerProofSize = 88;
constexpr std::size_t kDirectionalProofSize = 39;
constexpr std::size_t kListenerOkSize = 7;
constexpr std::size_t kLaunchIdSize = 33;
constexpr std::size_t kNonceSize = 16;
constexpr std::size_t kProofSize = 32;

namespace detail {

inline std::uint32_t RotateRight(std::uint32_t value, unsigned count)
{
  return (value >> count) | (value << (32 - count));
}

class Sha256
{
public:
  Sha256()
      : m_state{0x6a09e667u, 0xbb67ae85u, 0x3c6ef372u, 0xa54ff53au,
                0x510e527fu, 0x9b05688cu, 0x1f83d9abu, 0x5be0cd19u}
  {
  }

  void Update(const void* data, std::size_t size)
  {
    const auto* bytes = static_cast<const std::uint8_t*>(data);
    while (size != 0)
    {
      const std::size_t take = size < (64 - m_used) ? size : (64 - m_used);
      std::memcpy(m_block.data() + m_used, bytes, take);
      m_used += take;
      bytes += take;
      size -= take;
      m_total += take;
      if (m_used == 64)
      {
        Transform(m_block.data());
        m_used = 0;
      }
    }
  }

  std::array<std::uint8_t, kProofSize> Finish()
  {
    const std::uint64_t bits = static_cast<std::uint64_t>(m_total) * 8;
    m_block[m_used++] = 0x80;
    if (m_used > 56)
    {
      std::memset(m_block.data() + m_used, 0, 64 - m_used);
      Transform(m_block.data());
      m_used = 0;
    }
    std::memset(m_block.data() + m_used, 0, 56 - m_used);
    for (unsigned index = 0; index < 8; ++index)
      m_block[63 - index] = static_cast<std::uint8_t>(bits >> (index * 8));
    Transform(m_block.data());

    std::array<std::uint8_t, kProofSize> digest{};
    for (std::size_t word = 0; word < m_state.size(); ++word)
      for (unsigned byte = 0; byte < 4; ++byte)
        digest[word * 4 + byte] =
            static_cast<std::uint8_t>(m_state[word] >> (24 - byte * 8));
    return digest;
  }

private:
  void Transform(const std::uint8_t* block)
  {
    static constexpr std::uint32_t constants[64] = {
        0x428a2f98u, 0x71374491u, 0xb5c0fbcfu, 0xe9b5dba5u, 0x3956c25bu,
        0x59f111f1u, 0x923f82a4u, 0xab1c5ed5u, 0xd807aa98u, 0x12835b01u,
        0x243185beu, 0x550c7dc3u, 0x72be5d74u, 0x80deb1feu, 0x9bdc06a7u,
        0xc19bf174u, 0xe49b69c1u, 0xefbe4786u, 0x0fc19dc6u, 0x240ca1ccu,
        0x2de92c6fu, 0x4a7484aau, 0x5cb0a9dcu, 0x76f988dau, 0x983e5152u,
        0xa831c66du, 0xb00327c8u, 0xbf597fc7u, 0xc6e00bf3u, 0xd5a79147u,
        0x06ca6351u, 0x14292967u, 0x27b70a85u, 0x2e1b2138u, 0x4d2c6dfcu,
        0x53380d13u, 0x650a7354u, 0x766a0abbu, 0x81c2c92eu, 0x92722c85u,
        0xa2bfe8a1u, 0xa81a664bu, 0xc24b8b70u, 0xc76c51a3u, 0xd192e819u,
        0xd6990624u, 0xf40e3585u, 0x106aa070u, 0x19a4c116u, 0x1e376c08u,
        0x2748774cu, 0x34b0bcb5u, 0x391c0cb3u, 0x4ed8aa4au, 0x5b9cca4fu,
        0x682e6ff3u, 0x748f82eeu, 0x78a5636fu, 0x84c87814u, 0x8cc70208u,
        0x90befffau, 0xa4506cebu, 0xbef9a3f7u, 0xc67178f2u};
    std::uint32_t words[64]{};
    for (unsigned index = 0; index < 16; ++index)
      words[index] = (static_cast<std::uint32_t>(block[index * 4]) << 24) |
                     (static_cast<std::uint32_t>(block[index * 4 + 1]) << 16) |
                     (static_cast<std::uint32_t>(block[index * 4 + 2]) << 8) |
                     static_cast<std::uint32_t>(block[index * 4 + 3]);
    for (unsigned index = 16; index < 64; ++index)
    {
      const std::uint32_t s0 = RotateRight(words[index - 15], 7) ^
                               RotateRight(words[index - 15], 18) ^
                               (words[index - 15] >> 3);
      const std::uint32_t s1 = RotateRight(words[index - 2], 17) ^
                               RotateRight(words[index - 2], 19) ^
                               (words[index - 2] >> 10);
      words[index] = words[index - 16] + s0 + words[index - 7] + s1;
    }

    std::uint32_t a = m_state[0];
    std::uint32_t b = m_state[1];
    std::uint32_t c = m_state[2];
    std::uint32_t d = m_state[3];
    std::uint32_t e = m_state[4];
    std::uint32_t f = m_state[5];
    std::uint32_t g = m_state[6];
    std::uint32_t h = m_state[7];
    for (unsigned index = 0; index < 64; ++index)
    {
      const std::uint32_t sum1 = RotateRight(e, 6) ^ RotateRight(e, 11) ^ RotateRight(e, 25);
      const std::uint32_t choose = (e & f) ^ (~e & g);
      const std::uint32_t first = h + sum1 + choose + constants[index] + words[index];
      const std::uint32_t sum0 = RotateRight(a, 2) ^ RotateRight(a, 13) ^ RotateRight(a, 22);
      const std::uint32_t majority = (a & b) ^ (a & c) ^ (b & c);
      const std::uint32_t second = sum0 + majority;
      h = g;
      g = f;
      f = e;
      e = d + first;
      d = c;
      c = b;
      b = a;
      a = first + second;
    }
    m_state[0] += a;
    m_state[1] += b;
    m_state[2] += c;
    m_state[3] += d;
    m_state[4] += e;
    m_state[5] += f;
    m_state[6] += g;
    m_state[7] += h;
  }

  std::array<std::uint32_t, 8> m_state;
  std::array<std::uint8_t, 64> m_block{};
  std::size_t m_used = 0;
  std::size_t m_total = 0;
};

inline void AppendField(Sha256& hash, std::string_view field)
{
  std::uint8_t length[8]{};
  std::uint64_t value = field.size();
  for (int index = 7; index >= 0; --index)
  {
    length[index] = static_cast<std::uint8_t>(value);
    value >>= 8;
  }
  hash.Update(length, sizeof(length));
  hash.Update(field.data(), field.size());
}

template <typename... Fields>
std::array<std::uint8_t, kProofSize> Hmac(std::string_view key, Fields... fields)
{
  std::array<std::uint8_t, 64> key_block{};
  if (key.size() > key_block.size())
  {
    Sha256 shortened;
    shortened.Update(key.data(), key.size());
    const auto digest = shortened.Finish();
    std::memcpy(key_block.data(), digest.data(), digest.size());
  }
  else
  {
    std::memcpy(key_block.data(), key.data(), key.size());
  }
  std::array<std::uint8_t, 64> inner_pad{};
  std::array<std::uint8_t, 64> outer_pad{};
  for (std::size_t index = 0; index < key_block.size(); ++index)
  {
    inner_pad[index] = key_block[index] ^ 0x36;
    outer_pad[index] = key_block[index] ^ 0x5c;
  }
  Sha256 inner;
  inner.Update(inner_pad.data(), inner_pad.size());
  (AppendField(inner, std::string_view(fields)), ...);
  const auto inner_digest = inner.Finish();
  Sha256 outer;
  outer.Update(outer_pad.data(), outer_pad.size());
  outer.Update(inner_digest.data(), inner_digest.size());
  return outer.Finish();
}

inline bool Equal(const std::uint8_t* left, const std::uint8_t* right, std::size_t size)
{
  std::uint8_t difference = 0;
  for (std::size_t index = 0; index < size; ++index)
    difference |= left[index] ^ right[index];
  return difference == 0;
}

inline bool ValidLaunchId(std::string_view launch_id)
{
  if (launch_id.size() != kLaunchIdSize || launch_id.substr(0, 7) != "launch-")
    return false;
  for (const unsigned char value : launch_id.substr(7))
  {
    if (!((value >= '0' && value <= '9') || (value >= 'a' && value <= 'h') ||
          value == 'j' || value == 'k' || value == 'm' || value == 'n' ||
          (value >= 'p' && value <= 't') || (value >= 'v' && value <= 'z')))
      return false;
  }
  return true;
}

inline std::uint8_t Boundary(std::string_view key)
{
  constexpr std::string_view direct = "front-direct-";
  constexpr std::string_view broker = "front-broker-";
  const auto valid_hex = [](std::string_view value) {
    if (value.size() != 64)
      return false;
    for (const unsigned char byte : value)
      if (!((byte >= '0' && byte <= '9') || (byte >= 'a' && byte <= 'f') ||
            (byte >= 'A' && byte <= 'F')))
        return false;
    return true;
  };
  if (key.substr(0, direct.size()) == direct && valid_hex(key.substr(direct.size())))
    return 1;
  if (key.substr(0, broker.size()) == broker && valid_hex(key.substr(broker.size())))
    return 2;
  return 0;
}

inline bool Header(const std::uint8_t* bytes, std::uint8_t type, std::uint8_t boundary)
{
  return std::memcmp(bytes, "ECFA", 4) == 0 && bytes[4] == 1 && bytes[5] == type &&
         bytes[6] == boundary;
}

inline void SetHeader(std::uint8_t* bytes, std::uint8_t type, std::uint8_t boundary)
{
  std::memcpy(bytes, "ECFA", 4);
  bytes[4] = 1;
  bytes[5] = type;
  bytes[6] = boundary;
}

}  // namespace detail

template <typename ReadExact, typename WriteAll>
bool Authenticate(std::string_view front_key, std::string_view launch_id, std::uint16_t port,
                  ReadExact read_exact, WriteAll write_all)
{
  const std::uint8_t boundary = detail::Boundary(front_key);
  if (boundary == 0 || !detail::ValidLaunchId(launch_id))
    return false;

  std::array<std::uint8_t, kChallengeSize> challenge{};
  if (!read_exact(challenge.data(), challenge.size()) ||
      !detail::Header(challenge.data(), 1, boundary) ||
      challenge[7] != static_cast<std::uint8_t>(port >> 8) ||
      challenge[8] != static_cast<std::uint8_t>(port))
    return false;

  const std::string_view challenge_view(reinterpret_cast<const char*>(challenge.data()),
                                        challenge.size());
  const auto nonce_digest = detail::Hmac(front_key, "emucap-producer-front-nonce-v1",
                                         challenge_view);
  std::array<std::uint8_t, kNonceSize> producer_nonce{};
  std::memcpy(producer_nonce.data(), nonce_digest.data(), producer_nonce.size());
  std::string transcript(reinterpret_cast<const char*>(challenge.data()), challenge.size());
  transcript.append(launch_id.data(), launch_id.size());
  transcript.append(reinterpret_cast<const char*>(producer_nonce.data()), producer_nonce.size());
  const auto producer_proof = detail::Hmac(front_key, "emucap-producer-front-auth-v1",
                                           "producer", std::string_view(transcript));

  std::array<std::uint8_t, kProducerProofSize> producer{};
  detail::SetHeader(producer.data(), 2, boundary);
  std::memcpy(producer.data() + 7, launch_id.data(), launch_id.size());
  std::memcpy(producer.data() + 7 + kLaunchIdSize, producer_nonce.data(), producer_nonce.size());
  std::memcpy(producer.data() + 7 + kLaunchIdSize + kNonceSize, producer_proof.data(),
              producer_proof.size());
  if (!write_all(producer.data(), producer.size()))
    return false;

  std::array<std::uint8_t, kDirectionalProofSize> listener{};
  if (!read_exact(listener.data(), listener.size()) ||
      !detail::Header(listener.data(), 3, boundary))
    return false;
  const auto expected_listener = detail::Hmac(front_key, "emucap-producer-front-auth-v1",
                                               "listener", std::string_view(transcript));
  if (!detail::Equal(listener.data() + 7, expected_listener.data(), expected_listener.size()))
    return false;

  const auto commit_proof = detail::Hmac(front_key, "emucap-producer-front-auth-v1", "commit",
                                         std::string_view(transcript));
  std::array<std::uint8_t, kDirectionalProofSize> commit{};
  detail::SetHeader(commit.data(), 4, boundary);
  std::memcpy(commit.data() + 7, commit_proof.data(), commit_proof.size());
  if (!write_all(commit.data(), commit.size()))
    return false;

  std::array<std::uint8_t, kListenerOkSize> ok{};
  return read_exact(ok.data(), ok.size()) && detail::Header(ok.data(), 5, boundary);
}

}  // namespace emucap_front_auth
