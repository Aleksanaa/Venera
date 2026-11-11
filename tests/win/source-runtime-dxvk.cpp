// SPDX-License-Identifier: MIT
#include "log/log.h"
#include <windows.h>
#include <cstdio>

namespace dxvk { Logger Logger::s_instance("source-runtime-dxvk.log"); }
int main()
{
    dxvk::Logger::err("source-runtime dxvk error literal");
    dxvk::Logger::warn("source-runtime dxvk warning literal");
    dxvk::Logger::info("source-runtime dxvk info literal");
    dxvk::Logger::debug("source-runtime dxvk debug literal");
    dxvk::Logger::trace("source-runtime dxvk trace literal");
    std::printf("{\"pid\":%lu,\"entry\":\"dxvk::Logger\",\"threshold\":%u}\n",
                GetCurrentProcessId(), unsigned(dxvk::Logger::logLevel()));
    return 0;
}
