// SPDX-License-Identifier: MIT
#include "log/log.h"
#include <windows.h>
#include <cstdio>
#include <nlohmann/json.hpp>

namespace dxvk { Logger Logger::s_instance("source-runtime-dxvk.log"); }
int main()
{
    dxvk::Logger::err("source-runtime dxvk error literal");
    dxvk::Logger::warn("source-runtime dxvk warning literal");
    dxvk::Logger::info("source-runtime dxvk info literal");
    dxvk::Logger::debug("source-runtime dxvk debug literal");
    dxvk::Logger::trace("source-runtime dxvk trace literal");
    std::puts(nlohmann::ordered_json({{"pid", GetCurrentProcessId()},
        {"entry", "dxvk::Logger"}, {"threshold", unsigned(dxvk::Logger::logLevel())}}).dump().c_str());
    return 0;
}
