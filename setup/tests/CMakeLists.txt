# SPDX-License-Identifier: MIT
cmake_minimum_required(VERSION 3.24)
if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
  project(venera-setup-tests LANGUAGES CXX)
  set(CMAKE_CXX_STANDARD 20)
  set(CMAKE_CXX_STANDARD_REQUIRED ON)
endif()

enable_testing()
add_executable(setup-vioserial-state-test vioserial-state.cpp)
add_test(NAME setup-vioserial-state COMMAND setup-vioserial-state-test)

if(WIN32)
  add_executable(setup-uninstaller-copy-test uninstaller-copy.cpp ../uninstaller-copy.cpp)
  target_compile_definitions(setup-uninstaller-copy-test PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN)
  target_link_options(setup-uninstaller-copy-test PRIVATE -static -static-libgcc -static-libstdc++)
  target_link_options(setup-vioserial-state-test PRIVATE -static -static-libgcc -static-libstdc++)
  add_test(NAME setup-uninstaller-copy COMMAND setup-uninstaller-copy-test)
endif()
