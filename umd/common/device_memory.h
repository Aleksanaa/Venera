// SPDX-License-Identifier: MIT
// The process's Vulkan device memory, counted where every allocation passes: the entry point vulkan_icd.cpp hands to
// DXVK, vkd3d-proton and our own code counts what vkAllocateMemory and vkFreeMemory do. It is what the host's render
// server holds for this process (imports of other processes' memory included). Presents report it over ETW, for the
// panel's frame rate overlay.
#pragma once

#define VK_NO_PROTOTYPES
#include <vulkan/vulkan.h>

#include <cstdint>

namespace venera::umd {

// The ICD's entry point, with device memory counted.
PFN_vkGetInstanceProcAddr counting_gipa(PFN_vkGetInstanceProcAddr icd);

// Whether a report is due: someone listens and a second has passed since the last. Claims it.
bool memory_report_due();
// Reports this process's device memory, and how much of it the engine's allocator (`api` 11: DXVK) holds and uses
// for resources; both 0 where not known.
void report_memory(uint32_t api, uint64_t pool_allocated, uint64_t pool_used);

} // namespace venera::umd
