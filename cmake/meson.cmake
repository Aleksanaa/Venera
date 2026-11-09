# SPDX-License-Identifier: MIT
# Third-party code built with its own meson build for the MinGW target (vkd3d-proton.cmake, dxvk.cmake). meson and
# ninja come from the dev shell. Each meson build is configured once in the build tree; every build runs its ninja,
# which does nothing when nothing changed.

find_program(VENERA_MESON meson REQUIRED)
find_program(VENERA_NINJA ninja REQUIRED)

# Writes a meson cross file for this build's MinGW compilers (x86_64 or i686: the toolchain file's VENERA_MINGW_TRIPLE)
# to `path`. GCC from nixpkgs uses the mcfgthread model, whose headers and library live outside the wrapper's paths
# (the toolchain file's VENERA_MINGW_MCFG_*). Written only when it changes: meson rebuilds everything when its cross
# file is touched.
function(venera_meson_cross_file path)
  set(c_args "")
  set(link_args "")
  if(VENERA_MINGW_MCFG_INCLUDE)
    set(c_args "'-isystem', '${VENERA_MINGW_MCFG_INCLUDE}'")
  endif()
  if(VENERA_MINGW_MCFG_LIB)
    set(link_args "'-L${VENERA_MINGW_MCFG_LIB}'")
  endif()
  set(cpu_family "${CMAKE_SYSTEM_PROCESSOR}")
  if(cpu_family STREQUAL "i686")
    set(cpu_family x86)
  endif()
  # meson adds ccache itself only for native compilers: name it in the cross file (VENERA_CCACHE: CMakeLists.txt).
  set(launcher "")
  if(VENERA_CCACHE)
    set(launcher "'${VENERA_CCACHE}', ")
  endif()
  file(CONFIGURE OUTPUT "${path}" @ONLY CONTENT "\
[binaries]
c = [@launcher@'${CMAKE_C_COMPILER}']
cpp = [@launcher@'${CMAKE_CXX_COMPILER}']
ar = '${VENERA_MINGW_TRIPLE}-ar'
strip = '${VENERA_MINGW_TRIPLE}-strip'
windres = '${VENERA_MINGW_TRIPLE}-windres'

[built-in options]
c_args = [@c_args@]
cpp_args = [@c_args@]
c_link_args = [@link_args@]
cpp_link_args = [@link_args@]

[properties]
needs_exe_wrapper = true

[host_machine]
system = 'windows'
cpu_family = '${cpu_family}'
cpu = '${CMAKE_SYSTEM_PROCESSOR}'
endian = 'little'
")
endfunction()

# Configures the meson build of `src` in `build` (once) with the cross file at `cross` and extra options.
function(venera_meson_setup name src build cross)
  if(NOT EXISTS "${build}/build.ninja")
    execute_process(
      COMMAND "${VENERA_MESON}" setup "${build}" "${src}" --cross-file "${cross}" --buildtype=release -Db_ndebug=true
              ${ARGN}
      RESULT_VARIABLE result)
    if(NOT result EQUAL 0)
      message(FATAL_ERROR "meson setup of ${name} failed")
    endif()
  endif()
endfunction()
