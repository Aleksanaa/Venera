// SPDX-License-Identifier: MIT
#include <cstdio>
#include <string>
#include <thread>
#include <vector>

#ifdef ADAPTER_RUNTIME_DXVK
#include "log/log.h"
namespace dxvk {
Logger Logger::s_instance("adapter-runtime.log");
}
static void emit(int, const std::string& message) {
    dxvk::Logger::log(dxvk::LogLevel::Info, message);
}
#else
extern "C" void adapter_runtime_vkd3d_emit(int shader, const char* message);
extern "C" void adapter_runtime_vkd3d_flush(void);
static void emit(int shader, const std::string& message) {
    adapter_runtime_vkd3d_emit(shader, message.c_str());
}
#endif

int main() {
    emit(0, "adapter-runtime unicode: \xe4\xb8\xad\xe6\x96\x87 \xf0\x9f\x98\x80\n");
    emit(1, "adapter-runtime line 1\nadapter-runtime line 2\r\nadapter-runtime line 3\n");
    std::string long_message = "adapter-runtime long bytes=8192:";
    long_message.resize(8192 - 5, 'x');
    long_message += ":END\n";
    emit(0, long_message);
    std::vector<std::thread> threads;
    for (int thread = 0; thread < 4; ++thread)
        threads.emplace_back([thread] {
            for (int sequence = 0; sequence < 16; ++sequence)
                emit(thread % 2, "adapter-runtime thread=" + std::to_string(thread)
                    + " sequence=" + std::to_string(sequence) + "\n");
        });
    for (auto& thread : threads)
        thread.join();
#ifndef ADAPTER_RUNTIME_DXVK
    adapter_runtime_vkd3d_flush();
#endif
    std::puts("adapter-runtime completed logical_messages=67");
    return 0;
}
