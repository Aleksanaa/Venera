// SPDX-License-Identifier: MIT
#define VKD3D_DBG_CHANNEL VKD3D_DBG_CHANNEL_API
#include "vkd3d_debug.h"

void adapter_runtime_vkd3d_emit(int shader, const char *message)
{
    vkd3d_dbg_printf(shader ? VKD3D_DBG_CHANNEL_SHADER : VKD3D_DBG_CHANNEL_API,
            VKD3D_DBG_LEVEL_INFO, "adapter_runtime", "%s", message);
}

void adapter_runtime_vkd3d_flush(void)
{
    vkd3d_dbg_flush();
}
