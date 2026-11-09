# Cross toolchain for the 32-bit UMDs (i686), which the x86_64 build drives (umd/CMakeLists.txt): mingw-w64.cmake with
# the dev shell's i686 compilers and their mcfgthread.
set(CMAKE_SYSTEM_PROCESSOR i686)
set(VENERA_MINGW_TRIPLE i686-w64-mingw32)
set(VENERA_MINGW_MCFG_INCLUDE "$ENV{VENERA_MINGW32_MCFG_INCLUDE}")
set(VENERA_MINGW_MCFG_LIB "$ENV{VENERA_MINGW32_MCFG_LIB}")
include("${CMAKE_CURRENT_LIST_DIR}/mingw-w64.cmake")
