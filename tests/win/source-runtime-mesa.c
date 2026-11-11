// SPDX-License-Identifier: MIT
#include "util/log.h"
#include "util/os_misc.h"
#include <windows.h>
#include <stdio.h>
#include "source-runtime-json.h"

int main(void)
{
    mesa_log(MESA_LOG_ERROR, "MESA", "source-runtime mesa error literal");
    mesa_log(MESA_LOG_WARN, "MESA", "source-runtime mesa warning literal");
    mesa_log(MESA_LOG_INFO, "MESA", "source-runtime mesa info literal");
    mesa_log(MESA_LOG_DEBUG, "MESA", "source-runtime mesa debug literal");
    mesa_log(MESA_LOG_INFO, "MESA-VIRTIO", "source-runtime mesa virtio info literal");
    os_log_message("source-runtime mesa os_log_message literal\n");
    os_log_message("MESA-VIRTIO: source-runtime mesa os virtio literal\n");
    source_runtime_mesa_json((unsigned long)GetCurrentProcessId());
    return 0;
}
