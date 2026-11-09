# SPDX-License-Identifier: MIT
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

add_executable(memorytest memorytest.cpp)
target_include_directories(memorytest PRIVATE "${PROJECT_SOURCE_DIR}/kmd/yttrium/viogpu/shared")
target_link_libraries(memorytest PRIVATE venera-umd-common)
target_link_options(memorytest PRIVATE -static -static-libgcc -static-libstdc++)

add_executable(engineprobe engineprobe.cpp "${PROJECT_SOURCE_DIR}/umd/d3d12/engine.cpp"
    $<TARGET_OBJECTS:venera-vkd3d-bridge>)
target_include_directories(engineprobe PRIVATE "${PROJECT_SOURCE_DIR}/umd/d3d12")
target_compile_options(engineprobe PRIVATE
    "SHELL:-idirafter ${VENERA_WINKITS_DIR}/wdk/c/Include/10.0.26100.0/um"
    "SHELL:-idirafter ${VENERA_WINKITS_DIR}/wdk/c/Include/10.0.26100.0/shared"
    "SHELL:-idirafter ${VENERA_WINKITS_DIR}/sdk/c/Include/10.0.26100.0/shared"
    "SHELL:-idirafter ${VENERA_WINKITS_DIR}/sdk/c/Include/10.0.26100.0/um")
target_link_libraries(engineprobe PRIVATE venera-umd-common venera-directx-headers venera-vkd3d-proton)
target_link_options(engineprobe PRIVATE -static -static-libgcc -static-libstdc++)
