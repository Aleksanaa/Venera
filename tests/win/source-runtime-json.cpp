// SPDX-License-Identifier: MIT
#include "source-runtime-json.h"
#include <nlohmann/json.hpp>
#include <cstdio>

void source_runtime_mesa_json(unsigned long pid)
{
    std::puts(nlohmann::ordered_json({{"pid", pid}, {"entry", "mesa_log/os_log_message"}}).dump().c_str());
}

void source_runtime_vkd3d_json(unsigned long pid, int trace_compiled,
                               unsigned api_threshold, unsigned shader_threshold)
{
    std::puts(nlohmann::ordered_json({{"pid", pid}, {"entry", "ERR/INFO/FIXME/WARN/TRACE"},
        {"trace_compiled", bool(trace_compiled)}, {"api_threshold", api_threshold},
        {"shader_threshold", shader_threshold}}).dump().c_str());
}
