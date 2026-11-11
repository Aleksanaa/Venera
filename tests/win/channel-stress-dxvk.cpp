// SPDX-License-Identifier: MIT
#include "log/log.h"
#include <windows.h>
#include <cstdio>
#include <string>
#include <cmath>
#include <nlohmann/json.hpp>

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
    std::puts(nlohmann::ordered_json({{"phase", "marker"}, {"pid", GetCurrentProcessId()},
        {"bitness", unsigned(sizeof(void*) * 8)}, {"calls", calls}}).dump().c_str());
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
    auto milliseconds = [&](LONGLONG ticks) {
        return std::round(double(ticks) * 1000000 / double(frequency.QuadPart)) / 1000;
    };
    std::puts(nlohmann::ordered_json({{"phase", "completed"}, {"pid", GetCurrentProcessId()},
        {"bitness", unsigned(sizeof(void*) * 8)}, {"entry", "dxvk::Logger::info"},
        {"calls", calls}, {"numbered_calls", numbered}, {"payload_bytes", 1024},
        {"elapsed_ms", milliseconds(end.QuadPart - start.QuadPart)},
        {"max_call_ms", milliseconds(longest)}}).dump().c_str());
    return dxvk::Logger::logLevel() == dxvk::LogLevel::Info ? 0 : 2;
}
