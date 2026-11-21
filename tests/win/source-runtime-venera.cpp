// SPDX-License-Identifier: MIT
#include "log.h"
#include <windows.h>
#include <cstdio>
#include <nlohmann/json.hpp>

int main()
{
    using namespace venera::umd;
    log_init("venera_d3d11");
    logf(LogLevel::error, "source-runtime venera error literal");
    logf(LogLevel::warn, "source-runtime venera warning literal");
    logf(LogLevel::info, "source-runtime venera info literal");
    logf(LogLevel::debug, "source-runtime venera debug literal");
    VENERA_VERBOSE("source-runtime venera verbose literal");
    std::puts(nlohmann::ordered_json({{"pid", GetCurrentProcessId()},
        {"identity", "source-runtime-venera experiment"}, {"component", "venera_d3d11"},
        {"entry", "log_init/logf/VENERA_VERBOSE"}, {"verbose", verbose()}}).dump().c_str());
    return 0;
}
