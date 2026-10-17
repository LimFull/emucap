#include "emucap_atomic_file.h"

#include <cassert>
#include <cstdio>
#include <fstream>
#include <iterator>
#include <stdexcept>
#include <string>
#ifdef _WIN32
#include <windows.h>
#else
#include <sys/stat.h>
#include <unistd.h>
#endif

static std::string read_file(const std::string& path) {
  std::ifstream input(path, std::ios::binary);
  return std::string((std::istreambuf_iterator<char>(input)), std::istreambuf_iterator<char>());
}

int main(int argc, char** argv) {
  assert(argc == 2);
  const std::string root = argv[1];
#ifdef _WIN32
  assert(CreateDirectoryA(root.c_str(), nullptr));
  const char separator = '\\';
#else
  assert(::mkdir(root.c_str(), 0700) == 0);
  const char separator = '/';
#endif
  const auto path = root + separator + "state.bin";
  EmucapAtomicFile::write(path, "first", 5);
  assert(read_file(path) == "first");
  EmucapAtomicFile::write(path, "replacement", 11);
  assert(read_file(path) == "replacement");

#ifndef _WIN32
  const auto protected_path = root + separator + "protected.bin";
  const auto linked_path = root + separator + "linked.bin";
  {
    std::ofstream output(protected_path, std::ios::binary);
    output << "protected";
  }
  assert(::symlink(protected_path.c_str(), linked_path.c_str()) == 0);
  bool rejected = false;
  try {
    EmucapAtomicFile::write(linked_path, "corrupt", 7);
  } catch (const std::exception&) {
    rejected = true;
  }
  assert(rejected);
  assert(read_file(protected_path) == "protected");
  assert(::unlink(linked_path.c_str()) == 0);
#endif

  std::remove(path.c_str());
#ifndef _WIN32
  std::remove((root + separator + "protected.bin").c_str());
  assert(::rmdir(root.c_str()) == 0);
#else
  assert(RemoveDirectoryA(root.c_str()));
#endif
  std::puts("EMUCAP ATOMIC FILE PASSED");
}
