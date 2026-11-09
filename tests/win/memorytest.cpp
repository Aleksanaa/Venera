// SPDX-License-Identifier: MIT
// Run memorytest.exe. Vulkan calls are fault-injected; no GPU work is submitted.
#include "linear_image.h"
#include "viogpu_resource_refs.h"

#include <array>
#include <cstdio>
#include <cstdlib>
#include <random>

using namespace venera::umd;

#define REQUIRE(expr) do { if (!(expr)) { std::printf("FAIL line %d: %s\n", __LINE__, #expr); std::exit(1); } } while (0)

namespace {
uint32_t external_bits, selected_type;
unsigned images, memories, queries, allocations;
VkResult query_result, allocate_result, bind_result;

VKAPI_ATTR VkResult VKAPI_CALL image_properties(VkPhysicalDevice, const VkPhysicalDeviceImageFormatInfo2*,
                                                VkImageFormatProperties2*) { return VK_SUCCESS; }
VKAPI_ATTR VkResult VKAPI_CALL create_image(VkDevice, const VkImageCreateInfo*, const VkAllocationCallbacks*,
                                            VkImage* image)
{
    ++images;
    *image = reinterpret_cast<VkImage>(uintptr_t(1));
    return VK_SUCCESS;
}
VKAPI_ATTR void VKAPI_CALL destroy_image(VkDevice, VkImage, const VkAllocationCallbacks*) { --images; }
VKAPI_ATTR void VKAPI_CALL layout(VkDevice, VkImage, const VkImageSubresource*, VkSubresourceLayout* out)
{
    *out = {};
    out->rowPitch = 256;
}
VKAPI_ATTR void VKAPI_CALL requirements(VkDevice, const VkImageMemoryRequirementsInfo2*, VkMemoryRequirements2* out)
{
    out->memoryRequirements = {4096, 256, 3};
}
VKAPI_ATTR void VKAPI_CALL memory_properties(VkPhysicalDevice, VkPhysicalDeviceMemoryProperties* out)
{
    *out = {};
    out->memoryTypeCount = 2;
    out->memoryTypes[0].propertyFlags = VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT;
    out->memoryTypes[1].propertyFlags = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
}
VKAPI_ATTR VkResult VKAPI_CALL fd_properties(VkDevice, VkExternalMemoryHandleTypeFlagBits, int,
                                             VkMemoryFdPropertiesKHR* out)
{
    ++queries;
    out->memoryTypeBits = external_bits;
    return query_result;
}
VKAPI_ATTR VkResult VKAPI_CALL allocate_memory(VkDevice, const VkMemoryAllocateInfo* info,
                                               const VkAllocationCallbacks*, VkDeviceMemory* out)
{
    ++allocations;
    selected_type = info->memoryTypeIndex;
    if (allocate_result == VK_SUCCESS) {
        ++memories;
        *out = reinterpret_cast<VkDeviceMemory>(uintptr_t(2));
    }
    return allocate_result;
}
VKAPI_ATTR void VKAPI_CALL free_memory(VkDevice, VkDeviceMemory, const VkAllocationCallbacks*) { --memories; }
VKAPI_ATTR VkResult VKAPI_CALL bind_memory(VkDevice, VkImage, VkDeviceMemory, VkDeviceSize) { return bind_result; }

void import_tests()
{
    VulkanDevice d;
    d.external_memory = true;
    d.vk.vkGetPhysicalDeviceImageFormatProperties2 = image_properties;
    d.vk.vkCreateImage = create_image;
    d.vk.vkDestroyImage = destroy_image;
    d.vk.vkGetImageSubresourceLayout = layout;
    d.vk.vkGetImageMemoryRequirements2 = requirements;
    d.vk.vkGetPhysicalDeviceMemoryProperties = memory_properties;
    d.vk.vkGetMemoryFdPropertiesKHR = fd_properties;
    d.vk.vkAllocateMemory = allocate_memory;
    d.vk.vkFreeMemory = free_memory;
    d.vk.vkBindImageMemory = bind_memory;
    LinearImageDesc desc;
    desc.format = VK_FORMAT_R8G8B8A8_UNORM;
    desc.width = desc.height = 64;
    desc.usage = VK_IMAGE_USAGE_SAMPLED_BIT;
    std::unique_ptr<LinearImage> image;
    bool consumed = false;
    auto import = [&] { return LinearImage::create_imported(d, desc, 123, 4096, &image, &consumed); };

    // A system-memory export must not pick the image's preferred VRAM type.
    external_bits = 2;
    REQUIRE(SUCCEEDED(import()));
    REQUIRE(consumed && selected_type == 1 && images == 1 && memories == 1);
    image.reset();
    REQUIRE(images == 0 && memories == 0);

    external_bits = 4;
    unsigned before = allocations;
    REQUIRE(FAILED(import()) && !consumed && allocations == before && images == 0);
    external_bits = 2;
    query_result = VK_ERROR_INVALID_EXTERNAL_HANDLE;
    REQUIRE(FAILED(import()) && !consumed && allocations == before && images == 0);
    query_result = VK_SUCCESS;
    allocate_result = VK_ERROR_OUT_OF_DEVICE_MEMORY;
    REQUIRE(FAILED(import()) && !consumed && images == 0 && memories == 0);
    allocate_result = VK_SUCCESS;
    bind_result = VK_ERROR_OUT_OF_DEVICE_MEMORY;
    REQUIRE(FAILED(import()) && consumed && images == 0 && memories == 0);
    bind_result = VK_SUCCESS;
    d.external_memory = false;
    before = queries;
    REQUIRE(FAILED(import()) && !consumed && queries == before && images == 0);
    std::puts("PASS external memory type intersection, failures, ownership, extension guard");
}

void reference_tests()
{
    VioGpuResourceRefs refs;
    std::array<VioGpuResourceRefs::Entry, 256> entries{};
    std::array<bool, 256> live{};
    std::array<unsigned, 8> counts{};
    std::mt19937 rng(42);
    unsigned attaches = 0, detaches = 0;
    // All ids collide in one bucket. Independent handles can name one resource.
    for (unsigned i = 0; i < 100000; ++i) {
        const unsigned slot = rng() % entries.size();
        if (live[slot]) {
            const unsigned id = entries[slot].id / 64;
            const bool last = refs.Release(&entries[slot]);
            REQUIRE(last == (--counts[id] == 0));
            detaches += last;
        } else {
            const unsigned id = rng() % counts.size();
            const bool first = refs.Acquire(&entries[slot], id * 64);
            REQUIRE(first == (counts[id]++ == 0));
            attaches += first;
        }
        live[slot] = !live[slot];
    }
    for (unsigned i = 0; i < entries.size(); ++i)
        if (live[i]) {
            const bool last = refs.Release(&entries[i]);
            REQUIRE(last == (--counts[entries[i].id / 64] == 0));
            detaches += last;
        }
    REQUIRE(attaches == detaches);
    // Reuse after the last close, including rollback of a failed first attach.
    REQUIRE(refs.Acquire(&entries[0], 64));
    REQUIRE(refs.Release(&entries[0]));
    REQUIRE(refs.Acquire(&entries[1], 64));
    REQUIRE(!refs.Acquire(&entries[2], 64));
    REQUIRE(!refs.Release(&entries[1]));
    REQUIRE(refs.Release(&entries[2]));
    std::puts("PASS 100000 context-resource open/close operations, collisions, reuse, rollback");
}
} // namespace

int main()
{
    reference_tests();
    import_tests();
}
