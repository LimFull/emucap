// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
// Production field layout, admission, save selection and load implementation;
// controlled allocation/upload/readback and cache lifetime dependencies. No GPU proof.
#include <cassert>
#include <cstring>
#include <iostream>
#include <limits>
#include "Common/ChunkFile.h"
#include "VideoCommon/AbstractFramebuffer.h"
#include "VideoCommon/AbstractStagingTexture.h"
#include "VideoCommon/TextureCacheBase.h"
#include "VideoCommon/VideoConfig.h"

#undef ERROR_LOG_FMT
#define ERROR_LOG_FMT(...) ((void)0)
#undef ASSERT_MSG
#define ASSERT_MSG(category, condition, ...) assert(condition)

BackendInfo g_backend_info;
static unsigned alerts = 0, uploads = 0;
namespace Common
{
bool MsgAlertFmtImpl(bool, MsgType, Log::LogType, const char*, int, fmt::string_view,
                     const fmt::format_args&)
{
  ++alerts;
  return false;
}
std::string GetStringT(const char* text)
{
  return text;
}
}  // namespace Common

TCacheEntry::TCacheEntry(std::unique_ptr<AbstractTexture> tex,
                         std::unique_ptr<AbstractFramebuffer> fb)
    : texture(std::move(tex)), framebuffer(std::move(fb))
{
}
// Actual destructor is extracted below; only its backend pool is substituted.
struct FixturePool
{
  void ReleaseToPool(TCacheEntry*) {}
} fixture_pool;
static auto* g_fixture_texture_cache = &fixture_pool;

struct TestTexture : AbstractTexture
{
  using AbstractTexture::AbstractTexture;
  void CopyRectangleFromTexture(const AbstractTexture*, const MathUtil::Rectangle<int>&, u32, u32,
                                const MathUtil::Rectangle<int>&, u32, u32) override
  {
    std::abort();
  }
  void ResolveFromTexture(const AbstractTexture*, const MathUtil::Rectangle<int>&, u32,
                          u32) override
  {
    std::abort();
  }
  void Load(u32, u32, u32, u32, const u8*, size_t, u32) override { ++uploads; }
};
namespace TestConfig
{
constexpr int GFX_SAVE_TEXTURE_CACHE_TO_STATE = 0;
bool save_optional = true;
bool Get(int)
{
  return save_optional;
}
}  // namespace TestConfig
struct TestPresenter
{
  RcTcacheEntry presented;
  RcTcacheEntry GetPresentationEntry() { return presented; }
} presenter;
static auto* g_presenter = &presenter;

struct FixtureCache
{
  using TexPoolEntry = TextureCacheBase::TexPoolEntry;
  u64 m_last_entry_id = 100000;
  std::array<RcTcacheEntry, 8> m_bound_textures{};
  std::multimap<u32, RcTcacheEntry> m_textures_by_address;
  std::multimap<u64, RcTcacheEntry> m_textures_by_hash;
  std::unique_ptr<AbstractStagingTexture> m_readback_texture;
  unsigned clears = 0, allocations = 0, serializations = 0;
  unsigned fail_allocation = 0;
  void DoLoadState(PointerWrap&, std::vector<RcTcacheEntry>&);
  void DoSaveState(PointerWrap&);
  std::optional<TexPoolEntry> DeserializeTexture(PointerWrap&);
  void FlushStaleBinds() {}
  void ClearCacheEntries()
  {
    ++clears;
    m_bound_textures.fill({});
    m_textures_by_address.clear();
    m_textures_by_hash.clear();
  }
  std::optional<TexPoolEntry> AllocateTexture(const TextureConfig& config)
  {
    ++allocations;
    if (allocations == fail_allocation)
      return std::nullopt;
    return TexPoolEntry(std::make_unique<TestTexture>(config), nullptr);
  }
  void SerializeTexture(AbstractTexture*, const TextureConfig& config, PointerWrap& p)
  {
    ++serializations;
    p.Do(config);
    u32 size = *config.GetSerializedSize(g_backend_info.MaxTextureSize);
    u8* pixels = p.DoExternal(size);
    if (p.IsWriteMode())
      std::memset(pixels, 0, size);
  }
};
TextureCacheBase::TexPoolEntry::TexPoolEntry(std::unique_ptr<AbstractTexture> tex,
                                             std::unique_ptr<AbstractFramebuffer> fb)
    : texture(std::move(tex)), framebuffer(std::move(fb))
{
}
#include "texture-state.inc"

static TextureConfig Tiny()
{
  return TextureConfig(2, 2, 1, 1, 1, AbstractTextureFormat::RGBA8, 0,
                       AbstractTextureType::Texture_2DArray);
}
static RcTcacheEntry Entry(u64 id, const TextureConfig& config = Tiny())
{
  auto entry = std::make_shared<TCacheEntry>(std::make_unique<TestTexture>(config), nullptr);
  entry->id = id;
  entry->is_xfb_copy = true;
  return entry;
}
struct State
{
  std::vector<u8> bytes;
  size_t marker = 0;
  std::array<size_t, 4> lists{};
  std::vector<size_t> metadata;
};
static State Encoded(u32 count, const TextureConfig& image = Tiny())
{
  State state;
  const u32 payload_size = *image.GetSerializedSize(g_backend_info.MaxTextureSize);
  state.bytes.resize(size_t(count) * (256 + payload_size) + 128);
  auto* cursor = state.bytes.data();
  PointerWrap p(&cursor, state.bytes.size(), PointerWrap::Mode::Write);
  p.Do(count);
  for (u32 i = 0; i < count; ++i)
  {
    auto config = image;
    p.Do(config);
    u32 size = *config.GetSerializedSize(g_backend_info.MaxTextureSize);
    auto* data = p.DoExternal(size);
    std::memset(data, 0, size);
    state.metadata.push_back(cursor - state.bytes.data());
    TCacheEntry entry(nullptr, nullptr);
    entry.id = i + 1;
    entry.DoState(p);
  }
  state.marker = cursor - state.bytes.data();
  p.DoMarker("TextureCacheEntries");
  for (unsigned list = 0; list < 4; ++list)
  {
    state.lists[list] = cursor - state.bytes.data();
    u32 size = list == 0 ? 0 : list == 3 ? std::min(count, 8u) : count;
    p.Do(size);
    for (u32 i = 0; i < size; ++i)
    {
      u64 hash = i;
      if (list == 2)
        p.Do(hash);
      else
        p.Do(i);
      p.Do(i);
    }
  }
  assert(p.IsWriteMode() && !p.HasError());
  state.bytes.resize(cursor - state.bytes.data());
  return state;
}
static void Put(State& state, size_t offset, u32 value)
{
  std::memcpy(state.bytes.data() + offset, &value, sizeof(value));
}
static void Reject(State state)
{
  FixtureCache cache;
  const auto sentinel = Entry(99999);
  cache.m_bound_textures[0] = sentinel;
  cache.m_textures_by_address.emplace(4, sentinel);
  std::vector<RcTcacheEntry> restored{sentinel};
  auto* cursor = state.bytes.data();
  PointerWrap p(&cursor, state.bytes.size(), PointerWrap::Mode::Read);
  const auto before_uploads = uploads;
  cache.DoLoadState(p, restored);
  assert(p.HasError());
  assert(cache.allocations == 0 && cache.clears == 0 && uploads == before_uploads);
  assert(restored.size() == 1 && restored[0] == sentinel);
  assert(cache.m_bound_textures[0] == sentinel && cache.m_textures_by_address.size() == 1);
  assert(cursor == state.bytes.data());
}
static void Accept(State state, u32 count)
{
  FixtureCache cache;
  std::vector<RcTcacheEntry> restored;
  auto* cursor = state.bytes.data();
  PointerWrap p(&cursor, state.bytes.size(), PointerWrap::Mode::Read);
  cache.DoLoadState(p, restored);
  assert(!p.HasError() && p.IsReadMode());
  assert(restored.size() == count && cache.allocations == count && cache.clears == 1);
  assert(cursor == state.bytes.data() + state.bytes.size());
}
static State Associations(std::array<u32, 3> counts)
{
  State state = Encoded(1);
  const auto start = state.lists[0];
  state.bytes.resize(start + (size_t(counts[0]) + counts[1] + counts[2]) * 12 + 16);
  auto* cursor = state.bytes.data() + start;
  PointerWrap p(&cursor, state.bytes.size() - start, PointerWrap::Mode::Write);
  for (unsigned list = 0; list < 4; ++list)
  {
    u32 count = list == 3 ? 0 : counts[list];
    p.Do(count);
    for (u32 i = 0; i < count; ++i)
    {
      u32 id = 0;
      u64 hash = i;
      if (list == 2)
        p.Do(hash);
      else
        p.Do(id);
      p.Do(id);
    }
  }
  assert(p.IsWriteMode() && !p.HasError());
  state.bytes.resize(cursor - state.bytes.data());
  return state;
}
int main(int argc, char** argv)
{
  const std::string which = argc > 1 ? argv[1] : "all";
  if (which == "legitimate" || which == "all")
  {
    Accept(Encoded(0), 0);
    Accept(Encoded(4), 4);
    std::cout << "PASS empty and ordinary native collection decode\n";
  }
  if (which == "entry-limit" || which == "all")
  {
    Reject(Encoded(4097));
    std::cout << "PASS entry count rejected before cache mutation/allocation\n";
  }
  if (which == "late-invalid" || which == "all")
  {
    auto invalid = Encoded(4);
    Put(invalid, invalid.lists[3] + sizeof(u32), 8);
    Reject(invalid);
    std::cout << "PASS invalid final binding rejected before cache mutation/allocation\n";
  }
  if (which == "self-reference" || which == "all")
  {
    Accept(Associations({1, 1, 0}), 1);
    std::cout << "PASS self-reference decode and native destructor\n";
  }
#ifdef HAS_TEXTURE_ADMISSION
  if (which == "cube-budget" || which == "all")
  {
    TextureStateBudget cube_budget;
    auto cube = TextureConfig(4096, 4096, 1, 1, 1, AbstractTextureFormat::RGBA16F, 0,
                              AbstractTextureType::Texture_CubeMap);
    assert(cube_budget.Admit(cube, *cube.GetSerializedSize(16384)));
    assert(cube_budget.native == 6 * (u64{1} << 27));
    assert(!cube_budget.Admit(cube, *cube.GetSerializedSize(16384)));
    std::cout << "PASS cube storage charges all six native faces\n";
  }
#endif
  if (which != "all")
    return 0;
#ifdef HAS_TEXTURE_ADMISSION
  Accept(Encoded(4096), 4096);
  Accept(Encoded(2, TextureConfig(4, 2, 3, 2, 1, AbstractTextureFormat::RGBA8, 0,
                                  AbstractTextureType::Texture_2DArray)),
         2);
  Accept(Encoded(2, TextureConfig(4, 4, 1, 1, 1, AbstractTextureFormat::DXT1, 0,
                                  AbstractTextureType::Texture_2DArray)),
         2);
  Accept(Associations({32768, 32768, 0}), 1);
  Reject(Associations({32768, 32768, 1}));
  const auto good = Encoded(3);
  // Every truncation point, including partial count/metadata/map entries.
  for (size_t length = 0; length < good.bytes.size(); ++length)
  {
    auto short_state = good;
    short_state.bytes.resize(length);
    Reject(std::move(short_state));
  }
  auto bad = good;
  Put(bad, bad.marker, 0);
  Reject(bad);
  bad = good;
  // Duplicate logical ID via the actual serialized metadata extent.
  const auto metadata_size = good.metadata[1] - good.metadata[0] - sizeof(TextureConfig) - 4 - 16;
  std::memcpy(bad.bytes.data() + bad.metadata[1], bad.bytes.data() + bad.metadata[0],
              metadata_size);
  Reject(bad);
  bad = good;
  Put(bad, sizeof(u32) + sizeof(TextureConfig), std::numeric_limits<u32>::max());
  Reject(bad);
  for (unsigned list = 0; list < 4; ++list)
  {
    bad = good;
    Put(bad, bad.lists[list], list == 3 ? 9 : 65537);
    Reject(bad);
  }
  bad = good;
  Put(bad, bad.lists[1] + 2 * sizeof(u32), 3);
  Reject(bad);
  bad = good;
  Put(bad, bad.lists[2] + sizeof(u32) + sizeof(u64), 3);
  Reject(bad);

  TextureStateBudget budget;
  auto config = TextureConfig(8192, 8192, 1, 1, 1, AbstractTextureFormat::RGBA16F, 0,
                              AbstractTextureType::Texture_2DArray);
  assert(budget.Admit(config, *config.GetSerializedSize(16384)));
  assert(budget.Admit(config, *config.GetSerializedSize(16384)));
  assert(budget.serialized == (u64{1} << 30) && budget.native == (u64{1} << 30));
  const auto before = budget;
  assert(!budget.Admit(Tiny(), 16));
  assert(budget.serialized == before.serialized && budget.native == before.native &&
         budget.uploads == before.uploads);
  budget = {};
  config.format = AbstractTextureFormat::RGBA8;
  assert(budget.Admit(config, *config.GetSerializedSize(16384)));
  assert(budget.Admit(config, *config.GetSerializedSize(16384)));
  assert(budget.serialized < TextureStateBudget::MaxBytes);
  assert(!budget.Admit(Tiny(), 16));  // Native estimate independently exhausted.
  budget = {};
  config = Tiny();
  config.layers = 65536;
  assert(budget.Admit(config, *config.GetSerializedSize(16384)));
  assert(!budget.Admit(Tiny(), 16));  // Upload count independently exhausted.
  budget = {};
  assert(!budget.Admit(Tiny(), 15) && budget.serialized == 0);
  config = Tiny();
  config.layers = std::numeric_limits<u32>::max();
  assert(!budget.Admit(config, 0) && budget.serialized == 0);
  config = TextureConfig(16384, 16384, 1, 1, 1, AbstractTextureFormat::RGBA16F, 0,
                         AbstractTextureType::Texture_2DArray);
  assert(config.GetSerializedSize(16384) == 2147483648u);
  assert(!budget.Admit(config, *config.GetSerializedSize(16384)));
  std::cout << "PASS valid limits, all truncations, malformed IDs/lists, independent budgets\n";

  // Serialize actual selected presentation dependencies with optional caching both ways.
  for (bool optional : {true, false})
  {
    TestConfig::save_optional = optional;
    FixtureCache cache;
    auto first = Entry(1), second = Entry(2);
    second->is_xfb_copy = false;
    first->CreateReference(second.get());
    cache.m_textures_by_address.emplace(0x100, first);
    cache.m_textures_by_hash.emplace(0x200, second);
    cache.m_bound_textures[0] = second;
    presenter.presented = first;
    State saved;
    saved.bytes.resize(4096);
    auto* cursor = saved.bytes.data();
    PointerWrap write(&cursor, saved.bytes.size(), PointerWrap::Mode::Write);
    cache.DoSaveState(write);
    assert(write.IsWriteMode() && !write.HasError() && cache.serializations == 2);
    saved.bytes.resize(cursor - saved.bytes.data());
    FixtureCache restored_cache;
    std::vector<RcTcacheEntry> restored;
    cursor = saved.bytes.data();
    PointerWrap read(&cursor, saved.bytes.size(), PointerWrap::Mode::Read);
    restored_cache.DoLoadState(read, restored);
    assert(!read.HasError() && restored.size() == 2);
    assert(restored[0]->references.contains(restored[1].get()));
    assert(restored_cache.m_bound_textures[0] == restored[1]);
    assert(restored_cache.m_textures_by_address.size() == 1);
    assert(restored_cache.m_textures_by_hash.size() == 1);
    presenter.presented.reset();
  }
  // An over-budget ordinary save/undo refuses before texture serialization/readback.
  for (bool optional : {true, false})
  {
    TestConfig::save_optional = optional;
    FixtureCache cache;
    for (u32 i = 0; i < 4097; ++i)
      cache.m_textures_by_address.emplace(i, Entry(i + 1));
    std::array<u8, 32> storage{};
    auto* cursor = storage.data();
    PointerWrap p(&cursor, storage.size(), PointerWrap::Mode::Write);
    cache.DoSaveState(p);
    assert(p.HasError() && cache.serializations == 0 && cursor == storage.data());
  }
  // Save rejection uses the byte/upload budgets too, including required presentation.
  for (bool optional : {true, false})
  {
    TestConfig::save_optional = optional;
    for (bool upload_limit : {true, false})
    {
      FixtureCache cache;
      auto large = upload_limit ? Tiny() :
                                  TextureConfig(8192, 8192, 1, 1, 1, AbstractTextureFormat::RGBA16F,
                                                0, AbstractTextureType::Texture_2DArray);
      if (upload_limit)
        large.layers = 65536;
      for (u32 i = 0; i < 3; ++i)
        cache.m_textures_by_address.emplace(i, Entry(i + 1, large));
      std::array<u8, 32> storage{};
      auto* cursor = storage.data();
      PointerWrap p(&cursor, storage.size(), PointerWrap::Mode::Write);
      cache.DoSaveState(p);
      assert(p.HasError() && cache.serializations == 0 && cursor == storage.data());
    }
  }
  // Backend failure remains sticky; a subsequent independent decode has a fresh budget.
  FixtureCache failing;
  failing.fail_allocation = 2;
  auto state = Encoded(3);
  auto* cursor = state.bytes.data();
  PointerWrap p(&cursor, state.bytes.size(), PointerWrap::Mode::Read);
  std::vector<RcTcacheEntry> restored;
  failing.DoLoadState(p, restored);
  assert(p.HasError() && failing.allocations == 2);
  Accept(Encoded(3), 3);
  std::cout << "PASS save/load with optional cache on/off, save refusal and backend failure\n";
#else
  std::abort();  // The full contract requires the admission patch.
#endif
}
