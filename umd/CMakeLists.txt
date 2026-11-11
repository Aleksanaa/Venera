# SPDX-License-Identifier: MIT
# The user-mode drivers: venera_d3d12.dll (D3D12, on vkd3d-proton) and venera_d3d11.dll (D3D10/11, on DXVK), and what
# they share (common/). WDK headers are searched after MinGW's own (-idirafter), so MinGW's windows.h wins and only the
# DDI headers MinGW lacks come from the WDK.
set(WDK_INC "${VENERA_WINKITS_DIR}/wdk/c/Include/10.0.26100.0")
set(SDK_INC "${VENERA_WINKITS_DIR}/sdk/c/Include/10.0.26100.0")
if(NOT EXISTS "${WDK_INC}/um/d3d12umddi.h")
  message(FATAL_ERROR "d3d12umddi.h not found under ${WDK_INC}; see plan.md (Build) for fetching the WDK headers")
endif()

set(VENERA_UMD_WARNINGS -Wall -Wextra -Wno-unused-parameter -Wno-missing-field-initializers)
set(VENERA_UMD_WDK_INCLUDES
    "SHELL:-idirafter ${WDK_INC}/um" "SHELL:-idirafter ${WDK_INC}/shared"
    "SHELL:-idirafter ${SDK_INC}/shared" "SHELL:-idirafter ${SDK_INC}/um")
# Self-contained DLLs: the runtime loads them into arbitrary processes, so no MinGW runtime DLLs alongside.
set(VENERA_UMD_LINK_OPTIONS -static -static-libgcc -static-libstdc++ -Wl,--exclude-all-symbols)
# The 32-bit UMDs (for 32-bit processes: UserModeDriverNameWow) sit in the DriverStore next to the 64-bit ones, as
# venera_d3d11_32.dll and so on. Their exports are stdcall, which the runtime looks up undecorated (OpenAdapter12,
# not OpenAdapter12@4).
if(CMAKE_SIZEOF_VOID_P EQUAL 4)
  set(VENERA_UMD_SUFFIX _32)
  list(APPEND VENERA_UMD_LINK_OPTIONS -Wl,--kill-at)
else()
  set(VENERA_UMD_SUFFIX "")
endif()

# Slot names of the DDI tables we fill, for logging stubs (see cmake/ddi_slots.cmake).
include(${PROJECT_SOURCE_DIR}/cmake/ddi_slots.cmake)

# --- Shared --------------------------------------------------------------------------------------------------------
add_library(venera-umd-common STATIC
    common/device_memory.cpp
    common/kmt.cpp
    common/linear_image.cpp
    common/log.cpp
    common/shared_surface.cpp
    common/shader_container.cpp
    common/stacks.cpp
    common/trace.cpp
    common/vulkan_device.cpp
    common/vulkan_icd.cpp)
target_include_directories(venera-umd-common PUBLIC common)
target_link_libraries(venera-umd-common PUBLIC venera-vulkan-headers)
target_compile_options(venera-umd-common PRIVATE ${VENERA_UMD_WARNINGS} ${VENERA_UMD_WDK_INCLUDES})

add_subdirectory(d3d12)
add_subdirectory(d3d11)

# --- 32-bit --------------------------------------------------------------------------------------------------------
# The x86_64 build builds the 32-bit UMDs too, as a build tree of this project of its own (umd32/) with the i686
# toolchain, which has only the UMDs and the d3d9caps probe (CMakeLists.txt). The installer packs both sets of UMDs
# (setup/CMakeLists.txt).
if(CMAKE_SIZEOF_VOID_P EQUAL 8)
  include(ExternalProject)
  set(dir "${CMAKE_BINARY_DIR}/umd32")
  set(dlls "${dir}/umd/venera_d3d11_32.dll" "${dir}/umd/venera_d3d12_32.dll")
  ExternalProject_Add(venera-umd32
      SOURCE_DIR "${PROJECT_SOURCE_DIR}"
      BINARY_DIR "${dir}"
      CMAKE_ARGS "-DCMAKE_TOOLCHAIN_FILE=${PROJECT_SOURCE_DIR}/cmake/mingw-w64-i686.cmake"
                 "-DVENERA_WINKITS_DIR=${VENERA_WINKITS_DIR}" "-DVENERA_RELEASE=${VENERA_RELEASE}"
      # Its own build decides what is out of date.
      BUILD_ALWAYS ON
      BUILD_COMMAND "${CMAKE_COMMAND}" --build "${dir}" --target venera_d3d11 venera_d3d12 d3d9caps
      BUILD_BYPRODUCTS ${dlls}
      INSTALL_COMMAND ""
      USES_TERMINAL_CONFIGURE ON
      USES_TERMINAL_BUILD ON)
  set(VENERA_UMD32_DLLS ${dlls} PARENT_SCOPE)
endif()
