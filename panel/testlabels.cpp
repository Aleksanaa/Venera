// SPDX-License-Identifier: MIT
#include "panel.h"
namespace panel {
const wchar_t* testGroupDescription(const TestGroup& group) {
    if (std::string(group.id) == "d3d12") return locale::text(IDS_Test_d3d12_Description);
    if (std::string(group.id) == "d3d11") return locale::text(IDS_Test_d3d11_Description);
    if (std::string(group.id) == "d3d9") return locale::text(IDS_Test_d3d9_Description);
    if (std::string(group.id) == "vulkan") return locale::text(IDS_Test_vulkan_Description);
    return group.desc;
}
const wchar_t* testName(const TestGroup& group, const Test& test) {
    std::string id = std::string(group.id) + "." + test.id;
    if (id == "d3d12.device") return locale::text(IDS_Test_d3d12_device_Name);
    if (id == "d3d12.copy") return locale::text(IDS_Test_d3d12_copy_Name);
    if (id == "d3d12.triangle") return locale::text(IDS_Test_d3d12_triangle_Name);
    if (id == "d3d12.textured") return locale::text(IDS_Test_d3d12_textured_Name);
    if (id == "d3d12.depth") return locale::text(IDS_Test_d3d12_depth_Name);
    if (id == "d3d12.compute") return locale::text(IDS_Test_d3d12_compute_Name);
    if (id == "d3d12.queries") return locale::text(IDS_Test_d3d12_queries_Name);
    if (id == "d3d12.indirect") return locale::text(IDS_Test_d3d12_indirect_Name);
    if (id == "d3d12.placed") return locale::text(IDS_Test_d3d12_placed_Name);
    if (id == "d3d12.present") return locale::text(IDS_Test_d3d12_present_Name);
    if (id == "d3d11.device") return locale::text(IDS_Test_d3d11_device_Name);
    if (id == "d3d11.clear") return locale::text(IDS_Test_d3d11_clear_Name);
    if (id == "d3d11.triangle") return locale::text(IDS_Test_d3d11_triangle_Name);
    if (id == "d3d11.textured") return locale::text(IDS_Test_d3d11_textured_Name);
    if (id == "d3d11.depth") return locale::text(IDS_Test_d3d11_depth_Name);
    if (id == "d3d11.compute") return locale::text(IDS_Test_d3d11_compute_Name);
    if (id == "d3d11.stream-output") return locale::text(IDS_Test_d3d11_stream_output_Name);
    if (id == "d3d11.queries") return locale::text(IDS_Test_d3d11_queries_Name);
    if (id == "d3d11.update") return locale::text(IDS_Test_d3d11_update_Name);
    if (id == "d3d11.features") return locale::text(IDS_Test_d3d11_features_Name);
    if (id == "d3d11.typed-uav-load") return locale::text(IDS_Test_d3d11_typed_uav_load_Name);
    if (id == "d3d11.tiled") return locale::text(IDS_Test_d3d11_tiled_Name);
    if (id == "d3d11.conservative") return locale::text(IDS_Test_d3d11_conservative_Name);
    if (id == "d3d11.trim") return locale::text(IDS_Test_d3d11_trim_Name);
    if (id == "d3d11.kmt-adapter") return locale::text(IDS_Test_d3d11_kmt_adapter_Name);
    if (id == "d3d11.shared") return locale::text(IDS_Test_d3d11_shared_Name);
    if (id == "d3d11.shared-nt") return locale::text(IDS_Test_d3d11_shared_nt_Name);
    if (id == "d3d11.shared-updates") return locale::text(IDS_Test_d3d11_shared_updates_Name);
    if (id == "d3d11.present") return locale::text(IDS_Test_d3d11_present_Name);
    if (id == "d3d11.present-blt") return locale::text(IDS_Test_d3d11_present_blt_Name);
    if (id == "d3d11.present-check") return locale::text(IDS_Test_d3d11_present_check_Name);
    if (id == "d3d9.hal") return locale::text(IDS_Test_d3d9_hal_Name);
    if (id == "d3d9.9on12") return locale::text(IDS_Test_d3d9_9on12_Name);
    if (id == "d3d9.present") return locale::text(IDS_Test_d3d9_present_Name);
    if (id == "vulkan.swapchain") return locale::text(IDS_Test_vulkan_swapchain_Name);
    if (id == "vulkan.swapchain-d3d11") return locale::text(IDS_Test_vulkan_swapchain_d3d11_Name);
    return test.name;
}
const wchar_t* testDescription(const TestGroup& group, const Test& test) {
    std::string id = std::string(group.id) + "." + test.id;
    if (id == "d3d12.device") return locale::text(IDS_Test_d3d12_device_Description);
    if (id == "d3d12.copy") return locale::text(IDS_Test_d3d12_copy_Description);
    if (id == "d3d12.triangle") return locale::text(IDS_Test_d3d12_triangle_Description);
    if (id == "d3d12.textured") return locale::text(IDS_Test_d3d12_textured_Description);
    if (id == "d3d12.depth") return locale::text(IDS_Test_d3d12_depth_Description);
    if (id == "d3d12.compute") return locale::text(IDS_Test_d3d12_compute_Description);
    if (id == "d3d12.queries") return locale::text(IDS_Test_d3d12_queries_Description);
    if (id == "d3d12.indirect") return locale::text(IDS_Test_d3d12_indirect_Description);
    if (id == "d3d12.placed") return locale::text(IDS_Test_d3d12_placed_Description);
    if (id == "d3d12.present") return locale::text(IDS_Test_d3d12_present_Description);
    if (id == "d3d11.device") return locale::text(IDS_Test_d3d11_device_Description);
    if (id == "d3d11.clear") return locale::text(IDS_Test_d3d11_clear_Description);
    if (id == "d3d11.triangle") return locale::text(IDS_Test_d3d11_triangle_Description);
    if (id == "d3d11.textured") return locale::text(IDS_Test_d3d11_textured_Description);
    if (id == "d3d11.depth") return locale::text(IDS_Test_d3d11_depth_Description);
    if (id == "d3d11.compute") return locale::text(IDS_Test_d3d11_compute_Description);
    if (id == "d3d11.stream-output") return locale::text(IDS_Test_d3d11_stream_output_Description);
    if (id == "d3d11.queries") return locale::text(IDS_Test_d3d11_queries_Description);
    if (id == "d3d11.update") return locale::text(IDS_Test_d3d11_update_Description);
    if (id == "d3d11.features") return locale::text(IDS_Test_d3d11_features_Description);
    if (id == "d3d11.typed-uav-load") return locale::text(IDS_Test_d3d11_typed_uav_load_Description);
    if (id == "d3d11.tiled") return locale::text(IDS_Test_d3d11_tiled_Description);
    if (id == "d3d11.conservative") return locale::text(IDS_Test_d3d11_conservative_Description);
    if (id == "d3d11.trim") return locale::text(IDS_Test_d3d11_trim_Description);
    if (id == "d3d11.kmt-adapter") return locale::text(IDS_Test_d3d11_kmt_adapter_Description);
    if (id == "d3d11.shared") return locale::text(IDS_Test_d3d11_shared_Description);
    if (id == "d3d11.shared-nt") return locale::text(IDS_Test_d3d11_shared_nt_Description);
    if (id == "d3d11.shared-updates") return locale::text(IDS_Test_d3d11_shared_updates_Description);
    if (id == "d3d11.present") return locale::text(IDS_Test_d3d11_present_Description);
    if (id == "d3d11.present-blt") return locale::text(IDS_Test_d3d11_present_blt_Description);
    if (id == "d3d11.present-check") return locale::text(IDS_Test_d3d11_present_check_Description);
    if (id == "d3d9.hal") return locale::text(IDS_Test_d3d9_hal_Description);
    if (id == "d3d9.9on12") return locale::text(IDS_Test_d3d9_9on12_Description);
    if (id == "d3d9.present") return locale::text(IDS_Test_d3d9_present_Description);
    if (id == "vulkan.swapchain") return locale::text(IDS_Test_vulkan_swapchain_Description);
    if (id == "vulkan.swapchain-d3d11") return locale::text(IDS_Test_vulkan_swapchain_d3d11_Description);
    return test.desc;
}
}
