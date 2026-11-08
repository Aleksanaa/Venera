// SPDX-License-Identifier: MIT
#include "device_memory.h"

#include <windows.h>
#include <evntprov.h>

#include <atomic>
#include <cstring>
#include <mutex>
#include <unordered_map>

namespace venera::umd {

namespace {

// The ETW provider "Venera" and its one event, as panel/fpsoverlay.cpp reads it.
constexpr GUID kProvider = {0xc9b4c00a, 0x01bd, 0x4a11, {0xb5, 0x49, 0xd7, 0x3a, 0xc3, 0xf0, 0x55, 0xee}};
constexpr USHORT kMemoryEvent = 1;
struct MemoryEvent {
    uint32_t api;              // 11 or 12
    uint32_t reserved;
    uint64_t total;            // device memory, bytes
    uint64_t pool_allocated;   // of it, what the engine's allocator holds; 0 if not known
    uint64_t pool_used;        // and uses for resources
};

std::mutex g_lock;
PFN_vkGetInstanceProcAddr g_icd;
PFN_vkGetDeviceProcAddr g_get_device_proc;
PFN_vkAllocateMemory g_allocate;
PFN_vkFreeMemory g_free;
std::unordered_map<VkDeviceMemory, VkDeviceSize> g_sizes;
uint64_t g_total;

VKAPI_ATTR VkResult VKAPI_CALL allocate_memory(VkDevice device, const VkMemoryAllocateInfo* info,
                                               const VkAllocationCallbacks* callbacks, VkDeviceMemory* memory)
{
    const VkResult r = g_allocate(device, info, callbacks, memory);
    if (r == VK_SUCCESS) {
        std::lock_guard g(g_lock);
        g_sizes[*memory] = info->allocationSize;
        g_total += info->allocationSize;
    }
    return r;
}

VKAPI_ATTR void VKAPI_CALL free_memory(VkDevice device, VkDeviceMemory memory, const VkAllocationCallbacks* callbacks)
{
    if (memory) {
        std::lock_guard g(g_lock);
        const auto s = g_sizes.find(memory);
        if (s != g_sizes.end()) {
            g_total -= s->second;
            g_sizes.erase(s);
        }
    }
    g_free(device, memory, callbacks);
}

// Keeps the ICD's function in `slot` and returns ours in its place, or null if the ICD has none.
template <class T>
PFN_vkVoidFunction replace(T* slot, PFN_vkVoidFunction icd, T ours)
{
    if (!icd)
        return nullptr;
    std::lock_guard g(g_lock);
    *slot = reinterpret_cast<T>(icd);
    return reinterpret_cast<PFN_vkVoidFunction>(ours);
}

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL get_device_proc_addr(VkDevice device, const char* name);

// Ours for the functions we count, given the ICD's; null for others.
PFN_vkVoidFunction counting(const char* name, PFN_vkVoidFunction icd)
{
    if (!strcmp(name, "vkGetDeviceProcAddr"))
        return replace(&g_get_device_proc, icd, get_device_proc_addr);
    if (!strcmp(name, "vkAllocateMemory"))
        return replace(&g_allocate, icd, allocate_memory);
    if (!strcmp(name, "vkFreeMemory"))
        return replace(&g_free, icd, free_memory);
    return nullptr;
}

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL get_device_proc_addr(VkDevice device, const char* name)
{
    const PFN_vkVoidFunction icd = g_get_device_proc(device, name);
    const PFN_vkVoidFunction ours = counting(name, icd);
    return ours ? ours : icd;
}

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL get_instance_proc_addr(VkInstance instance, const char* name)
{
    if (!strcmp(name, "vkGetInstanceProcAddr"))
        return reinterpret_cast<PFN_vkVoidFunction>(get_instance_proc_addr);
    const PFN_vkVoidFunction icd = g_icd(instance, name);
    const PFN_vkVoidFunction ours = counting(name, icd);
    return ours ? ours : icd;
}

struct Provider {
    REGHANDLE handle = 0;
    Provider() { EventRegister(&kProvider, nullptr, nullptr, &handle); }
    ~Provider() { EventUnregister(handle); }
};

Provider& provider()
{
    static Provider p;
    return p;
}

} // namespace

PFN_vkGetInstanceProcAddr counting_gipa(PFN_vkGetInstanceProcAddr icd)
{
    g_icd = icd;
    return get_instance_proc_addr;
}

bool memory_report_due()
{
    static std::atomic<ULONGLONG> last{0};
    if (!EventProviderEnabled(provider().handle, 0, 0))
        return false;
    const ULONGLONG now = GetTickCount64();
    ULONGLONG then = last.load(std::memory_order_relaxed);
    return now - then >= 1000 && last.compare_exchange_strong(then, now, std::memory_order_relaxed);
}

void report_memory(uint32_t api, uint64_t pool_allocated, uint64_t pool_used)
{
    MemoryEvent e{api, 0, 0, pool_allocated, pool_used};
    {
        std::lock_guard g(g_lock);
        e.total = g_total;
    }
    EVENT_DESCRIPTOR desc{};
    desc.Id = kMemoryEvent;
    desc.Level = 4;   // information
    EVENT_DATA_DESCRIPTOR data;
    EventDataDescCreate(&data, &e, sizeof(e));
    EventWrite(provider().handle, &desc, 1, &data);
}

} // namespace venera::umd
