// SPDX-License-Identifier: MIT
#include "log.h"
#include <cstdio>

int main()
{
    PrintMessage(L"source-runtime resolution PrintMessage literal\n");
    std::printf("{\"pid\":%lu,\"identity\":\"source-runtime-resolution experiment\",\"entry\":\"PrintMessage\"}\n",
                GetCurrentProcessId());
    return 0;
}
