# SPDX-License-Identifier: MIT
# D3D9 caps and an FP16 blend, for both builds: d3d9caps_32 runs on the 32-bit UMDs (D3D9On12 on venera_d3d12_32).
add_executable(d3d9caps d3d9caps.cpp)
target_link_libraries(d3d9caps PRIVATE d3d9)
target_link_options(d3d9caps PRIVATE -static -static-libgcc -static-libstdc++)
if(CMAKE_SIZEOF_VOID_P EQUAL 4)
  set_target_properties(d3d9caps PROPERTIES OUTPUT_NAME d3d9caps_32)
  return()
endif()

add_executable(vkprobe vkprobe.cpp)
target_link_libraries(vkprobe PRIVATE venera-vulkan-headers)
target_link_options(vkprobe PRIVATE -static -static-libgcc -static-libstdc++)

add_executable(vkdeviceprobe vkdeviceprobe.cpp)
target_link_libraries(vkdeviceprobe PRIVATE venera-vulkan-headers)
target_link_options(vkdeviceprobe PRIVATE -static -static-libgcc -static-libstdc++)

# The guest scripts next to the probes, so a copy of build/ is all the guest needs (they find the rest from there).
foreach(script venera-test.ps1 kmt-trace.wds)
  configure_file(${script} ${script} COPYONLY)
endforeach()

add_executable(stackdump stackdump.cpp)
target_link_libraries(stackdump PRIVATE dbghelp)
target_link_options(stackdump PRIVATE -static -static-libgcc -static-libstdc++)

add_executable(d3d11caps d3d11caps.cpp)
target_link_libraries(d3d11caps PRIVATE d3d11 dxgi dxguid)
target_link_options(d3d11caps PRIVATE -static -static-libgcc -static-libstdc++)

add_executable(uibench uibench.cpp)
target_link_libraries(uibench PRIVATE dwmapi winmm user32 gdi32)
target_link_options(uibench PRIVATE -static -static-libgcc -static-libstdc++)

# Records performance traces (tests/perf/vtrace.py drives it).
add_executable(venera-trace venera-trace.cpp)
target_link_libraries(venera-trace PRIVATE tdh advapi32)
target_compile_options(venera-trace PRIVATE -Wall -Wextra)
target_link_options(venera-trace PRIVATE -static -static-libgcc -static-libstdc++)
