# SPDX-License-Identifier: MIT
# venera-setup.exe, the installer: one executable with the driver package (pkg/venera.inf and the files it lists) inside
# as a WIM, which payload.cmake stamps and packs at each build. The KMD and the Mesa drivers come from their own
# builds (kmd/, icd/build.sh), which have to run first.
set(VENERA_ICD_DIR "${CMAKE_SOURCE_DIR}/build/icd" CACHE PATH
    "icd/build.sh's output (venera_vulkan_virtio.dll, viogpu_wgl.dll and their _32 builds)")
set(VENERA_KMD_SYS "${CMAKE_SOURCE_DIR}/build/kmd/signed/viogpu3d.sys" CACHE FILEPATH
    "The test-signed KMD (kmd/ build)")
find_program(WIMLIB wimlib-imagex REQUIRED)
find_package(Git REQUIRED)

# The driver package's files, and the control panel, which is not part of the package: the installer copies it from the
# unpacked package into Program Files. The 32-bit UMDs come from the 32-bit build tree (VENERA_UMD32_DLLS:
# umd/CMakeLists.txt).
set(payload "${VENERA_KMD_SYS}" "${VENERA_ICD_DIR}/venera_vulkan_virtio.dll" "${VENERA_ICD_DIR}/viogpu_wgl.dll"
    "${VENERA_ICD_DIR}/venera_vulkan_virtio_32.dll" "${VENERA_ICD_DIR}/viogpu_wgl_32.dll"
    "${CMAKE_SOURCE_DIR}/pkg/venera_vulkan_virtio.json" "${CMAKE_SOURCE_DIR}/pkg/venera_vulkan_virtio_32.json"
    $<TARGET_FILE:venera_d3d11> $<TARGET_FILE:venera_d3d12> ${VENERA_UMD32_DLLS}
    $<TARGET_FILE:vgpusrv> $<TARGET_FILE:viogpuap>
    $<TARGET_FILE:venera-panel> $<TARGET_FILE:venera-panel-com>)
foreach(f "${VENERA_KMD_SYS}" "${VENERA_ICD_DIR}/venera_vulkan_virtio.dll"
          "${VENERA_ICD_DIR}/venera_vulkan_virtio_32.dll")
  if(NOT EXISTS "${f}")
    message(WARNING "${f} is missing: build the KMD and the ICD (README) before the installer")
  endif()
endforeach()
# A new commit changes the version.
set(head_log "${CMAKE_SOURCE_DIR}/.git/logs/HEAD")
if(NOT EXISTS "${head_log}")
  set(head_log "")
endif()

add_custom_command(
    OUTPUT payload.wim payload.rc version.txt
    # The dev shell sets SOURCE_DATE_EPOCH (1980), which string(TIMESTAMP) would take for the build time.
    COMMAND "${CMAKE_COMMAND}" -E env --unset=SOURCE_DATE_EPOCH "${CMAKE_COMMAND}" "-DSOURCE_DIR=${CMAKE_SOURCE_DIR}" "-DOUT_DIR=${CMAKE_CURRENT_BINARY_DIR}"
            "-DVERSION=${PROJECT_VERSION}" "-DGIT=${GIT_EXECUTABLE}" "-DWIMLIB=${WIMLIB}" "-DFILES=${payload}"
            -P "${CMAKE_CURRENT_SOURCE_DIR}/payload.cmake"
    DEPENDS payload.cmake "${CMAKE_SOURCE_DIR}/pkg/venera.inf" ${payload} ${head_log}
    COMMENT "Packing the driver package and the control panel"
    VERBATIM)
set_source_files_properties("${CMAKE_CURRENT_BINARY_DIR}/payload.rc" PROPERTIES
    OBJECT_DEPENDS "${CMAKE_CURRENT_BINARY_DIR}/payload.wim")
set_source_files_properties(setup.rc PROPERTIES OBJECT_DEPENDS "${CMAKE_SOURCE_DIR}/docs/venera.ico")

# The Licenses window's tabs: Venera, then the projects whose code is in the installer's files, as the panel's About page
# lists them (panel/aboutpage.cpp). Each is "Name|file|..."; a file can have a heading ("Heading=file"). Paths are from
# the source tree; setup/licenses holds the texts the tree does not have otherwise (verbatim, from the MinGW-w64 v14.0.0
# and mcfgthread 2.4.2 sources nixpkgs builds from, and the SPDX license list's MPL-2.0).
set(license_tabs
    "Venera|LICENSE"
    "yttrium|virtio-win (VirtIO)=kmd/yttrium/LICENSE|viogpu=kmd/yttrium/viogpu/LICENSE|vgpusrv and viogpuap=kmd/yttrium/viogpu/viogpusc/LICENSE|viogpu3d (Mozilla Public License 2.0)=setup/licenses/MPL-2.0.txt"
    "Mesa|MIT=third_party/virtio-win-mesa/licenses/MIT|Apache-2.0=third_party/virtio-win-mesa/licenses/Apache-2.0|BSL-1.0=third_party/virtio-win-mesa/licenses/BSL-1.0|SGI-B-2.0=third_party/virtio-win-mesa/licenses/SGI-B-2.0"
    "DXVK|third_party/dxvk/LICENSE"
    "vkd3d-proton|third_party/vkd3d-proton/COPYING|third_party/vkd3d-proton/LICENSE"
    "dxil-spirv|third_party/vkd3d-proton/subprojects/dxil-spirv/LICENSE.MIT"
    "dxbc-spirv|third_party/dxvk/subprojects/dxbc-spirv/LICENSE"
    "libdisplay-info|third_party/dxvk/subprojects/libdisplay-info/LICENSE"
    "DirectX-Headers|third_party/DirectX-Headers/LICENSE"
    "Vulkan-Headers|third_party/vkd3d-proton/khronos/Vulkan-Headers/LICENSE.md|MIT=third_party/vkd3d-proton/khronos/Vulkan-Headers/LICENSES/MIT.txt"
    "MinGW-w64|setup/licenses/mingw-w64-runtime.txt"
    "GCC runtime and mcfgthread|mcfgthread=setup/licenses/mcfgthread.md|GCC Runtime Library Exception 3.1=setup/licenses/gcc-exception-3.1.txt|GNU Lesser General Public License 3.0=setup/licenses/lgpl-3.0.txt|GNU General Public License 3.0=setup/licenses/gpl-3.0.txt")
# Written at configure time, one text per tab, starting with its name on a line of its own; licenses.rc puts them in
# as RCDATA 200, 201, ... (ui.cpp). A changed text configures again. Not file(CONFIGURE): it would expand @...@ in them.
function(write_if_changed path content)
  file(WRITE "${path}.tmp" "${content}")
  file(COPY_FILE "${path}.tmp" "${path}" ONLY_IF_DIFFERENT)
  file(REMOVE "${path}.tmp")
endfunction()
set(license_rc "// Generated by setup/CMakeLists.txt.\n")
set(id 200)
foreach(tab ${license_tabs})
  string(REPLACE "|" ";" parts "${tab}")
  list(POP_FRONT parts name)
  set(text "${name}\n")
  foreach(part ${parts})
    if(part MATCHES "^(.*)=(.*)$")
      set(heading "${CMAKE_MATCH_1}")
      set(file "${CMAKE_MATCH_2}")
      string(LENGTH "${heading}" n)
      string(REPEAT "=" ${n} rule)
      string(APPEND text "${heading}\n${rule}\n\n")
    else()
      set(file "${part}")
    endif()
    file(READ "${CMAKE_SOURCE_DIR}/${file}" content)
    string(STRIP "${content}" content)
    string(APPEND text "${content}\n\n\n")
    set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${CMAKE_SOURCE_DIR}/${file}")
  endforeach()
  write_if_changed("${CMAKE_CURRENT_BINARY_DIR}/licenses/${id}.txt" "${text}")
  string(APPEND license_rc "${id} RCDATA \"${CMAKE_CURRENT_BINARY_DIR}/licenses/${id}.txt\"\n")
  math(EXPR id "${id} + 1")
endforeach()
write_if_changed("${CMAKE_CURRENT_BINARY_DIR}/licenses.rc" "${license_rc}")

enable_language(RC)
add_executable(venera-setup WIN32 main.cpp system.cpp install.cpp ui.cpp setup.rc
    "${CMAKE_CURRENT_BINARY_DIR}/payload.rc" "${CMAKE_CURRENT_BINARY_DIR}/licenses.rc")
# The WIM holds the panel: it has to be built first.
add_dependencies(venera-setup venera-panel venera-panel-com)
target_compile_definitions(venera-setup PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN _WIN32_WINNT=0x0A00)
target_link_libraries(venera-setup PRIVATE setupapi crypt32 cryptui wintrust ncrypt comctl32 shell32 shlwapi
    ole32 uuid advapi32 ntdll)
target_link_options(venera-setup PRIVATE -municode -s -static -static-libgcc -static-libstdc++)
