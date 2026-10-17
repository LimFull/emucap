#include "emucap_front_auth.h"

#include <cassert>
#include <cstdio>
#include <string>
#include <vector>

using namespace emucap_front_auth;

std::string Hex(const std::array<std::uint8_t, kProofSize>& bytes)
{
  static constexpr char digits[] = "0123456789abcdef";
  std::string result;
  for (const std::uint8_t byte : bytes)
  {
    result.push_back(digits[byte >> 4]);
    result.push_back(digits[byte & 15]);
  }
  return result;
}

struct Peer
{
  std::string key;
  std::array<std::uint8_t, kChallengeSize> challenge{};
  std::vector<std::uint8_t> producer;
  unsigned read_stage = 0;
  bool valid_listener = true;
  bool commit_seen = false;

  bool Read(std::uint8_t* bytes, std::size_t size)
  {
    if (read_stage++ == 0)
    {
      assert(size == challenge.size());
      std::memcpy(bytes, challenge.data(), size);
      return true;
    }
    assert(producer.size() == kProducerProofSize);
    const std::string transcript(
        reinterpret_cast<const char*>(challenge.data()), challenge.size());
    std::string bound = transcript;
    bound.append(reinterpret_cast<const char*>(producer.data() + 7),
                 kLaunchIdSize + kNonceSize);
    if (read_stage == 2)
    {
      assert(size == kDirectionalProofSize);
      detail::SetHeader(bytes, 3, 1);
      const auto proof = detail::Hmac(key, "emucap-producer-front-auth-v1", "listener",
                                      std::string_view(bound));
      std::memcpy(bytes + 7, proof.data(), proof.size());
      if (!valid_listener)
        bytes[7] ^= 1;
      return true;
    }
    assert(size == kListenerOkSize);
    detail::SetHeader(bytes, 5, 1);
    return true;
  }

  bool Write(const std::uint8_t* bytes, std::size_t size)
  {
    if (producer.empty())
    {
      assert(size == kProducerProofSize);
      producer.assign(bytes, bytes + size);
      return true;
    }
    commit_seen = true;
    assert(size == kDirectionalProofSize);
    assert(detail::Header(bytes, 4, 1));
    std::string transcript(reinterpret_cast<const char*>(challenge.data()), challenge.size());
    transcript.append(reinterpret_cast<const char*>(producer.data() + 7),
                      kLaunchIdSize + kNonceSize);
    const auto expected = detail::Hmac(key, "emucap-producer-front-auth-v1", "commit",
                                       std::string_view(transcript));
    assert(detail::Equal(bytes + 7, expected.data(), expected.size()));
    return true;
  }
};

int main()
{
  assert(Hex(detail::Hmac("key", "a", "bc")) ==
         "ba41d3aaace0b4bb31fb49a559ae1df182e1fd8add9b6b3b5603e6cb342dce22");

  Peer peer;
  peer.key = "front-direct-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
  detail::SetHeader(peer.challenge.data(), 1, 1);
  peer.challenge[7] = 0xba;
  peer.challenge[8] = 0xb8;
  for (std::size_t index = 9; index < peer.challenge.size(); ++index)
    peer.challenge[index] = static_cast<std::uint8_t>(index);
  const bool authenticated = Authenticate(
      peer.key, "launch-01arz3ndektsv4rrffq69g5fav", 47800,
      [&](std::uint8_t* bytes, std::size_t size) { return peer.Read(bytes, size); },
      [&](const std::uint8_t* bytes, std::size_t size) { return peer.Write(bytes, size); });
  assert(authenticated);
  assert(detail::Header(peer.producer.data(), 2, 1));
  assert(peer.commit_seen);

  Peer counterfeit;
  counterfeit.key = peer.key;
  counterfeit.challenge = peer.challenge;
  counterfeit.valid_listener = false;
  assert(!Authenticate(
      counterfeit.key, "launch-01arz3ndektsv4rrffq69g5fav", 47800,
      [&](std::uint8_t* bytes, std::size_t size) { return counterfeit.Read(bytes, size); },
      [&](const std::uint8_t* bytes, std::size_t size) {
        return counterfeit.Write(bytes, size);
      }));
  assert(!counterfeit.commit_seen);

  std::puts("producer front authentication vectors passed");
  return 0;
}
