// SPDX-License-Identifier: MIT
// Run beside venera_vulkan_virtio.dll. Uses a private device and empty submissions.
#include "engine.h"
#include "log.h"
#include <cstdio>
#include <cstring>

using namespace venera::umd;

namespace {
VulkanDevice::Fns original;
unsigned allocations, fences, submissions, recordings;
bool fail_fence = true, fail_begin = false, fail_wait = false;

VKAPI_ATTR VkResult VKAPI_CALL allocate(VkDevice d, const VkCommandBufferAllocateInfo* info, VkCommandBuffer* out)
{
    allocations += info->commandBufferCount;
    return original.vkAllocateCommandBuffers(d, info, out);
}
VKAPI_ATTR VkResult VKAPI_CALL fence(VkDevice d, const VkFenceCreateInfo* info, const VkAllocationCallbacks* a, VkFence* out)
{
    ++fences;
    if (fail_fence) {
        fail_fence = false;
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    }
    return original.vkCreateFence(d, info, a, out);
}
VKAPI_ATTR VkResult VKAPI_CALL begin(VkCommandBuffer cmd, const VkCommandBufferBeginInfo* info)
{
    if (fail_begin) {
        fail_begin = false;
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    }
    return original.vkBeginCommandBuffer(cmd, info);
}
VKAPI_ATTR VkResult VKAPI_CALL submit(VkQueue q, uint32_t n, const VkSubmitInfo* info, VkFence f)
{
    ++submissions;
    return original.vkQueueSubmit(q, n, info, f);
}
VKAPI_ATTR VkResult VKAPI_CALL wait(VkDevice d, uint32_t n, const VkFence* f, VkBool32 all, uint64_t timeout)
{
    const VkResult r = original.vkWaitForFences(d, n, f, all, timeout);
    // Complete real GPU work before injecting failure so teardown stays safe.
    return r == VK_SUCCESS && fail_wait ? VK_ERROR_DEVICE_LOST : r;
}
} // namespace

#define REQUIRE(expr) do { if (!(expr)) { std::printf("FAIL line %d: %s\n", __LINE__, #expr); return 1; } } while (0)

int main(int argc, char** argv)
{
    std::setvbuf(stdout, nullptr, _IONBF, 0);
    log_init("engineprobe");
    const bool reuse_only = argc == 2 && std::strcmp(argv[1], "--reuse-only") == 0;
    if (argc > 1 && !reuse_only)
        return 2;
    fail_fence = !reuse_only;
    std::shared_ptr<Engine> e;
    const HRESULT hr = Engine::get(&e);
    std::printf("Engine::get: 0x%08lx\n", hr);
    REQUIRE(SUCCEEDED(hr));
    std::printf("GPU: %s\n", e->name.c_str());
    original = e->vulkan.vk;
    e->vulkan.vk.vkAllocateCommandBuffers = allocate;
    e->vulkan.vk.vkCreateFence = fence;
    e->vulkan.vk.vkBeginCommandBuffer = begin;
    e->vulkan.vk.vkQueueSubmit = submit;
    e->vulkan.vk.vkWaitForFences = wait;
    auto record = [](VkCommandBuffer) { ++recordings; };
    if (!reuse_only) {
        REQUIRE(FAILED(e->run(record)));
        REQUIRE(allocations == 0 && recordings == 0);
    }
    for (unsigned i = 0; i < 256; ++i)
        REQUIRE(SUCCEEDED(e->run(record)));
    std::printf("submissions=%u command_buffers=%u fence_attempts=%u\n", submissions, allocations, fences);
    REQUIRE(allocations == 1 && fences == (reuse_only ? 1u : 2u) && submissions == 256 && recordings == 256);
    if (reuse_only)
        return 0;
    std::puts("PASS 256 GPU submissions: one command buffer, fence allocation retry");

    fail_begin = true;
    REQUIRE(FAILED(e->run(record)) && recordings == 256 && submissions == 256);
    REQUIRE(SUCCEEDED(e->run(record)));
    fail_wait = true;
    REQUIRE(FAILED(e->run(record)));
    const unsigned submitted = submissions;
    REQUIRE(FAILED(e->run(record)) && submissions == submitted && allocations == 1);
    std::puts("PASS begin failure recovery and failed-wait reuse guard");
    e.reset();
    std::puts("PASS engine teardown");
}
