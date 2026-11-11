// SPDX-License-Identifier: MIT
#pragma once
#ifdef __cplusplus
extern "C" {
#endif
void source_runtime_mesa_json(unsigned long pid);
void source_runtime_vkd3d_json(unsigned long pid, int trace_compiled,
                               unsigned api_threshold, unsigned shader_threshold);
#ifdef __cplusplus
}
#endif
