// SPDX-License-Identifier: MIT
#define VKD3D_DBG_CHANNEL VKD3D_DBG_CHANNEL_API
#include "vkd3d_debug.h"
#include <stdio.h>
#include "source-runtime-json.h"

static void api_messages(void)
{
    ERR("source-runtime vkd3d api error literal\n");
    INFO("source-runtime vkd3d api info literal\n");
    FIXME("source-runtime vkd3d api fixme literal\n");
    WARN("source-runtime vkd3d api warning literal\n");
    TRACE("source-runtime vkd3d api trace literal\n");
}
#undef VKD3D_DBG_CHANNEL
#define VKD3D_DBG_CHANNEL VKD3D_DBG_CHANNEL_SHADER
static void shader_messages(void)
{
    ERR("source-runtime vkd3d shader error literal\n");
    INFO("source-runtime vkd3d shader info literal\n");
    FIXME("source-runtime vkd3d shader fixme literal\n");
    WARN("source-runtime vkd3d shader warning literal\n");
    TRACE("source-runtime vkd3d shader trace literal\n");
}
int main(void)
{
    api_messages();
    shader_messages();
    if (GetEnvironmentVariableA("TEST_SHADER_TEXT", NULL, 0)) {
        static char shader[8000 * 11 + 1];
        for (unsigned i = 0; i < 8000; ++i) memcpy(shader + i * 11, "mov r0, r1\n", 11);
        vkd3d_dbg_printf(VKD3D_DBG_CHANNEL_SHADER, VKD3D_DBG_LEVEL_WARN, "compile", "%s", shader);
    }
    vkd3d_dbg_flush();
    source_runtime_vkd3d_json((unsigned long)GetCurrentProcessId(),
#ifdef VKD3D_NO_TRACE_MESSAGES
           0,
#else
           1,
#endif
           vkd3d_dbg_get_level(VKD3D_DBG_CHANNEL_API),
           vkd3d_dbg_get_level(VKD3D_DBG_CHANNEL_SHADER));
    return 0;
}
