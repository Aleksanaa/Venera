# SPDX-License-Identifier: MIT
# venera-setup.exe, the installer: one executable with the driver package (pkg/venera.inf and the files it lists) inside
# as a WIM, which payload.cmake stamps and packs at each build. The KMD and the Mesa drivers come from their own
# builds (kmd/, icd/build.sh), which have to run first.
set(VENERA_ICD_DIR "${CMAKE_SOURCE_DIR}/build/icd" CACHE PATH
    "icd/build.sh's output (venera_vulkan_virtio.dll, viogpu_wgl.dll and their _32 builds)")
set(VENERA_KMD_SYS "${CMAKE_SOURCE_DIR}/build/kmd/signed/viogpu3d.sys" CACHE FILEPATH
    "The test-signed KMD (kmd/ build)")
set(VENERA_VIOSERIAL_DIR "" CACHE PATH "VirtIO serial driver package directory")
set(vioserial_files)
if(VENERA_VIOSERIAL_DIR)
  foreach(name vioser.inf vioser.sys vioser.cat LICENSE.txt)
    if(NOT EXISTS "${VENERA_VIOSERIAL_DIR}/${name}")
      message(FATAL_ERROR "${VENERA_VIOSERIAL_DIR}/${name} is required for the VirtIO serial package")
    endif()
  endforeach()
  file(GLOB_RECURSE vioserial_files CONFIGURE_DEPENDS "${VENERA_VIOSERIAL_DIR}/*")
endif()
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
    $<TARGET_FILE:venera-panel> $<TARGET_FILE:venera-panel-com> "${VENERA_DEBUG_GUEST_EXE}")
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
            "-DVIOSERIAL_DIR=${VENERA_VIOSERIAL_DIR}"
            -P "${CMAKE_CURRENT_SOURCE_DIR}/payload.cmake"
    DEPENDS payload.cmake "${CMAKE_SOURCE_DIR}/pkg/venera.inf" ${payload} ${vioserial_files} ${head_log}
    COMMENT "Packing the driver package and the control panel"
    VERBATIM)
set_source_files_properties("${CMAKE_CURRENT_BINARY_DIR}/payload.rc" PROPERTIES
    OBJECT_DEPENDS "${CMAKE_CURRENT_BINARY_DIR}/payload.wim")
set_source_files_properties(setup.rc PROPERTIES OBJECT_DEPENDS "${CMAKE_SOURCE_DIR}/docs/venera.ico")

enable_language(RC)
add_executable(venera-setup WIN32 main.cpp system.cpp install.cpp uninstaller-copy.cpp ui.cpp setup.rc
    "${CMAKE_CURRENT_BINARY_DIR}/payload.rc")
# The WIM holds the panel: it has to be built first.
add_dependencies(venera-setup venera-panel venera-panel-com venera-debug-guest)
target_compile_definitions(venera-setup PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN _WIN32_WINNT=0x0A00)
if(VENERA_VIOSERIAL_DIR)
  target_compile_definitions(venera-setup PRIVATE VENERA_HAS_VIOSERIAL=1)
endif()
target_link_libraries(venera-setup PRIVATE setupapi crypt32 cryptui wintrust ncrypt comctl32 shell32 shlwapi
    ole32 uuid advapi32 ntdll cfgmgr32)
target_link_options(venera-setup PRIVATE -municode -s -static -static-libgcc -static-libstdc++)

add_subdirectory(tests)
