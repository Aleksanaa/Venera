# Cross toolchain for the Windows UMD and guest probes (x86_64, MinGW-w64 GCC from the dev shell). The 32-bit UMDs
# use mingw-w64-i686.cmake, which sets the variables below before including this.
if(NOT VENERA_MINGW_TRIPLE)
  set(CMAKE_SYSTEM_PROCESSOR x86_64)
  set(VENERA_MINGW_TRIPLE x86_64-w64-mingw32)
  set(VENERA_MINGW_MCFG_INCLUDE "$ENV{VENERA_MINGW_MCFG_INCLUDE}")
  set(VENERA_MINGW_MCFG_LIB "$ENV{VENERA_MINGW_MCFG_LIB}")
endif()
set(CMAKE_SYSTEM_NAME Windows)

set(CMAKE_C_COMPILER ${VENERA_MINGW_TRIPLE}-gcc)
set(CMAKE_CXX_COMPILER ${VENERA_MINGW_TRIPLE}-g++)
set(CMAKE_RC_COMPILER ${VENERA_MINGW_TRIPLE}-windres)

set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)

# GCC from nixpkgs uses the mcfgthread model, whose headers and static library live outside the wrapper's default
# paths (flake.nix; cmake/meson.cmake passes them on to the meson builds).
if(VENERA_MINGW_MCFG_INCLUDE)
  set(CMAKE_C_FLAGS_INIT "-isystem ${VENERA_MINGW_MCFG_INCLUDE}")
  set(CMAKE_CXX_FLAGS_INIT "-isystem ${VENERA_MINGW_MCFG_INCLUDE}")
endif()
if(VENERA_MINGW_MCFG_LIB)
  set(CMAKE_EXE_LINKER_FLAGS_INIT "-L${VENERA_MINGW_MCFG_LIB}")
  set(CMAKE_SHARED_LINKER_FLAGS_INIT "-L${VENERA_MINGW_MCFG_LIB}")
endif()
