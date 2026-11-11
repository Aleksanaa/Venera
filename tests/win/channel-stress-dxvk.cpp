// SPDX-License-Identifier: MIT
#include "log/log.h"
#include <windows.h>
#include <cstdio>
#include <string>

namespace dxvk { Logger Logger::s_instance("channel-stress.log"); }

int main()
{
    LARGE_INTEGER frequency, start, before, after, end;
    QueryPerformanceFrequency(&frequency);
    QueryPerformanceCounter(&start);
    LONGLONG longest = 0;
    unsigned calls = 0, numbered = 0;
    auto emit = [&](const std::string& message) {
        QueryPerformanceCounter(&before);
        dxvk::Logger::info(message);
        QueryPerformanceCounter(&after);
        if (after.QuadPart - before.QuadPart > longest)
            longest = after.QuadPart - before.QuadPart;
        ++calls;
    };
    emit("channel-stress marker literal");
    std::printf("{\"phase\":\"marker\",\"pid\":%lu,\"bitness\":%u,\"calls\":%u}\n",
                GetCurrentProcessId(), unsigned(sizeof(void*) * 8), calls);
    std::fflush(stdout);
    for (unsigned sequence = 1; sequence <= 20000; ++sequence) {
        QueryPerformanceCounter(&end);
        if (end.QuadPart - start.QuadPart >= frequency.QuadPart * 10)
            break;
        char prefix[96];
        std::snprintf(prefix, sizeof(prefix), "channel-stress sequence=%05u literal ", sequence);
        std::string message(prefix);
        message.resize(1024, 'x');
        emit(message);
        ++numbered;
    }
    QueryPerformanceCounter(&end);
    std::printf("{\"phase\":\"completed\",\"pid\":%lu,\"bitness\":%u,\"entry\":\"dxvk::Logger::info\","
                "\"calls\":%u,\"numbered_calls\":%u,\"payload_bytes\":1024,\"elapsed_ms\":%.3f,\"max_call_ms\":%.3f}\n",
                GetCurrentProcessId(), unsigned(sizeof(void*) * 8), calls, numbered,
                double(end.QuadPart - start.QuadPart) * 1000 / double(frequency.QuadPart),
                double(longest) * 1000 / double(frequency.QuadPart));
    return dxvk::Logger::logLevel() == dxvk::LogLevel::Info ? 0 : 2;
}
