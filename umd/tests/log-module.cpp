// SPDX-License-Identifier: MIT
#include "../common/log.h"
#include <windows.h>

namespace venera::umd {
void log_exceptions() {}
}

extern "C" __declspec(dllexport) void emit_logs()
{
    venera::umd::log_init(LOG_COMPONENT);
    for (auto level : {venera::umd::LogLevel::error, venera::umd::LogLevel::warn,
                       venera::umd::LogLevel::info, venera::umd::LogLevel::debug})
        venera::umd::logf(level, "shared logf %s %d", "repeated", 42);
}
