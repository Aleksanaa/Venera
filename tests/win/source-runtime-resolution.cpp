// SPDX-License-Identifier: MIT
#include "log.h"
#include <cstdio>
#include <nlohmann/json.hpp>

int main()
{
    PrintMessage(L"source-runtime resolution PrintMessage literal\n");
    std::puts(nlohmann::ordered_json({{"pid", GetCurrentProcessId()},
        {"identity", "source-runtime-resolution experiment"}, {"entry", "PrintMessage"}}).dump().c_str());
    return 0;
}
