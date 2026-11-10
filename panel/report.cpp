// SPDX-License-Identifier: MIT
// The system report: what the Overview shows and `/info` prints, from the system information and the probe's result.
#include "panel.h"

#include <cwchar>

namespace panel {

namespace {

std::wstring probed(const ProbeResult* probe, const char* key)
{
    if (!probe)
        return {};
    auto it = probe->find(key);
    return it == probe->end() ? std::wstring() : it->second;
}

bool apiOk(const ProbeResult* probe, const char* api)
{
    return probed(probe, (std::string(api) + ".status").c_str()) == L"ok";
}

InfoRow api(const ProbeResult* probe, const wchar_t* name, const char* prefix, const std::wstring& okText)
{
    if (!probe)
        return {name, L"Checking…"};
    std::wstring s = probed(probe, (std::string(prefix) + ".status").c_str());
    if (s == L"ok")
        return {name, okText};
    if (s == L"checking")  // started but never finished
        return {name, probed(probe, "timeout").empty() ? L"The check crashed" : L"The check stopped responding", true};
    if (s.empty())
        return {name, L"Not checked", true};
    if (s == L"no adapter")
        return {name, L"No VirtIO GPU adapter", true};
    if (s == L"no loader")
        return {name, L"The Vulkan Runtime is not installed", true};
    if (s == L"no device")
        return {name, L"No Venus device", true};
    return {name, L"Failed (" + s + L")", true};
}

std::wstring gigabytes(ULONGLONG bytes)
{
    wchar_t buf[32];
    double gb = bytes / double(1ull << 30);
    swprintf(buf, 32, gb == double(ULONGLONG(gb)) ? L"%.0f GB" : L"%.1f GB", gb);
    return buf;
}

}  // namespace

void hostGpu(const ProbeResult* probe, std::wstring& gpu, std::wstring& driver)
{
    std::wstring name = probed(probe, "vk.device");
    const std::wstring prefix = L"Virtio-GPU Venus (";
    if (name.compare(0, prefix.size(), prefix) == 0 && name.back() == L')')
        name = name.substr(prefix.size(), name.size() - prefix.size() - 1);
    size_t open = name.rfind(L" (");
    if (!name.empty() && name.back() == L')' && open != std::wstring::npos) {
        driver = name.substr(open + 2, name.size() - open - 3);
        name.resize(open);
    }
    gpu = name;
}

std::vector<InfoSection> describe(const SystemInfo& info, const ProbeResult* probe)
{
    std::vector<InfoSection> out;

    InfoSection driver{L"", L"Driver"};
    driver.rows.push_back({L"Version", info.productVersion.empty() ? L"Not installed" : info.productVersion,
                           info.productVersion.empty()});
    if (!info.installDate.empty())
        driver.rows.push_back({L"Installed", info.installDate});
    if (!info.deviceFound) {
        driver.rows.push_back({L"Device", L"No VirtIO GPU found", true});
    } else {
        if (info.problem)
            driver.rows.push_back(
                {L"Device", L"Device Manager reports a problem, code " + std::to_wstring(info.problem), true});
        InfoRow device{L"Device driver", info.driverDesc.empty() ? L"None" : info.driverDesc, !info.ours};
        if (info.needRestart)
            device.warning = L"Restart Windows to run the driver installed last";
        driver.rows.push_back(device);
        if (!info.driverVersion.empty()) {
            // The package's DriverVer is Venera's version without the commit hash (setup/payload.cmake).
            InfoRow version{L"Driver version",
                            info.driverVersion + (info.driverDate.empty() ? L"" : L", " + info.driverDate)};
            if (info.ours && !info.productVersion.empty() &&
                info.productVersion.compare(0, info.driverVersion.size() + 1, info.driverVersion + L".")) {
                version.warning = L"Not the installed Venera's (" + info.productVersion + L"): install Venera again";
            }
            driver.rows.push_back(version);
        }
        // Our drivers that the probe's processes loaded from elsewhere than the device's folder: stale copies.
        if (info.ours && probe && !info.driverFolder.empty()) {
            std::wstring stale;
            bool any = false;
            for (const auto& [key, path] : *probe) {
                if (key.compare(0, 7, "module."))
                    continue;
                any = true;
                size_t slash = path.rfind(L'\\');
                if (slash == std::wstring::npos || _wcsicmp(path.substr(0, slash).c_str(), info.driverFolder.c_str()))
                    stale += (stale.empty() ? L"" : L", ") + path;
            }
            if (!stale.empty())
                driver.rows.push_back({L"Driver files", L"Not all from the installed driver", false,
                                       L"Loaded from outside the driver's folder: " + stale});
            else if (any)
                driver.rows.push_back({L"Driver files", L"Loaded from the installed driver"});
        }
    }
    driver.rows.push_back({L"Test signing", info.testSigning ? L"On" : L"Off, so Windows does not load the driver",
                           !info.testSigning});
    out.push_back(driver);

    InfoSection host{L"", L"Host"};
    std::wstring gpu, vkDriver;
    hostGpu(probe, gpu, vkDriver);
    if (!probe)
        host.rows.push_back({L"GPU", L"Checking…"});
    else if (!gpu.empty()) {
        host.rows.push_back({L"GPU", gpu});
        // "RADV STRIX_HALO" from the device name, and the driver's own "Mesa 25.2.4" where Venus passes it on
        std::wstring info = probed(probe, "vk.host_driver_info");
        if (vkDriver.empty())
            vkDriver = probed(probe, "vk.host_driver");
        if (!info.empty())
            vkDriver += (vkDriver.empty() ? L"" : L", ") + info;
        if (!vkDriver.empty())
            host.rows.push_back({L"Vulkan driver", vkDriver});
    } else
        host.rows.push_back({L"GPU", L"Unknown: Venus did not answer", true});
    // The virtio-gpu device's options on QEMU's command line (docs/qemu.md), which the guest sees only as features.
    if (info.kmdAnswered) {
        auto feature = [&](const wchar_t* name, bool on, const wchar_t* off) {
            host.rows.push_back({name, on ? L"On" : off, !on});
        };
        feature(L"3D acceleration", info.supports3d, L"Off: QEMU needs virtio-vga-gl or virtio-gpu-gl");
        feature(L"Venus", info.venus, L"Off: add venus=on to the virtio-gpu device");
        feature(L"Blob resources", info.blob, L"Off: add blob=on to the virtio-gpu device");
        if (!info.hostVisible)
            host.rows.push_back({L"Host memory window", L"None: add hostmem=4G to the virtio-gpu device", true});
        else
            host.rows.push_back(
                {L"Host memory window", info.hostVisibleSize ? gigabytes(info.hostVisibleSize) : L"On"});
        feature(L"Context init", info.contextInit, L"Off: the host needs virglrenderer 1.0 or newer");
    } else if (info.ours) {
        host.rows.push_back({L"Features", L"Unknown: the driver did not answer", true});
    }
    out.push_back(host);

    InfoSection guest{L"", L"Guest"};
    if (!info.machine.empty())
        guest.rows.push_back({L"Virtual machine", info.machine});
    if (!info.machineType.empty())
        guest.rows.push_back({L"Machine type", info.machineType});
    if (!info.windows.empty())
        guest.rows.push_back({L"Windows", info.windows});
    if (!info.cpu.empty())
        guest.rows.push_back({L"Processor", info.cpu});
    if (info.threads) {
        auto count = [](DWORD n, const wchar_t* one) {
            return std::to_wstring(n) + L" " + one + (n == 1 ? L"" : L"s");
        };
        // Two sockets of one core each: likely several sockets configured, of which Windows uses at most two. Fewer
        // than four cores: applications race the drivers' translation threads for them. One socket of at least four
        // cores is what to configure.
        InfoRow topology{L"Topology", count(info.sockets, L"socket") + L", " + count(info.cores, L"core") + L", " +
                                          count(info.threads, L"thread")};
        if (info.sockets == 2 && info.cores == 2)
            topology.warning = L"You likely configured each core as its own socket, and Windows uses at most 2: change "
                               L"to 1 socket with at least 4 cores. ";
        else if (info.cores < 4)
            topology.warning = L"Give the virtual machine 1 socket with at least 4 cores. ";
        if (info.cores < 4)
            topology.warning += L"With fewer cores, applications race the drivers' translation threads for them, which "
                                L"makes rendering slow and can leak memory";
        guest.rows.push_back(topology);
    }
    if (info.memory)
        guest.rows.push_back(
            {L"Memory", gigabytes(info.memory) + L" (" + gigabytes(info.usableMemory) + L" usable)"});
    guest.rows.push_back({L"Swap", info.swap ? gigabytes(info.swap) : L"None"});
    for (size_t i = 0; i < info.displays.size(); i++) {
        const Display& d = info.displays[i];
        wchar_t buf[96];
        swprintf(buf, 96, L"%lu × %lu at %lu Hz, %u%% scale", d.width, d.height, d.hz, d.scale);
        guest.rows.push_back({info.displays.size() > 1 ? L"Display " + std::to_wstring(i + 1) : L"Display", buf});
    }
    if (info.displays.empty())
        guest.rows.push_back({L"Display", L"None attached"});
    for (const std::wstring& m : info.mice)
        guest.rows.push_back({L"Mouse", m});
    for (const std::wstring& k : info.keyboards)
        guest.rows.push_back({L"Keyboard", k});
    if (info.mice.empty() && info.keyboards.empty())
        guest.rows.push_back({L"Input", L"No mouse or keyboard found"});
    out.push_back(guest);

    InfoSection apis{L"", L"Graphics APIs"};
    apis.rows.push_back(api(probe, L"Direct3D 12", "d3d12",
                            L"Feature level " + probed(probe, "d3d12.level") + L", shader model " +
                                probed(probe, "d3d12.shader_model")));
    apis.rows.push_back(api(probe, L"Direct3D 11", "d3d11", L"Feature level " + probed(probe, "d3d11.level")));
    if (!probe)
        apis.rows.push_back({L"Direct3D 9", L"Checking…"});
    else
        apis.rows.push_back(apiOk(probe, "d3d12") ? InfoRow{L"Direct3D 9", L"Through D3D9On12"}
                                                  : InfoRow{L"Direct3D 9", L"Needs Direct3D 12", true});
    apis.rows.push_back(api(probe, L"Vulkan", "vk",
                            probed(probe, "vk.api") + L" (" + probed(probe, "vk.driver") + L", " +
                                probed(probe, "vk.driver_info") + L")"));
    // Another program's driver, which applications may pick over ours or which may break the loader.
    for (const std::wstring& d : info.otherVulkanDrivers)
        apis.rows.push_back({L"Other Vulkan driver", d});
    if (!info.otherVulkanDrivers.empty())
        apis.rows.back().warning = std::wstring(L"Applications may use ") +
                                   (info.otherVulkanDrivers.size() > 1 ? L"these" : L"it") + L" instead of Venera's";
    // OpenGL: "4.6 (Compatibility Profile) Mesa 26.1.0" from zink; Windows' own renderer is "GDI Generic".
    std::wstring glVersion = probed(probe, "gl.version"), renderer = probed(probe, "gl.renderer");
    InfoRow gl = api(probe, L"OpenGL", "gl", L"");
    if (apiOk(probe, "gl")) {
        if (renderer.find(L"GDI Generic") != std::wstring::npos) {
            gl = {L"OpenGL", L"Windows' software renderer, not the GPU", true};
        } else {
            std::wstring v = glVersion.substr(0, glVersion.find(L' '));
            size_t mesa = glVersion.find(L"Mesa ");
            std::wstring by = renderer.find(L"zink") != std::wstring::npos ? L"zink" : renderer;
            gl.value = v + L" (" + by + (mesa != std::wstring::npos ? L", " + glVersion.substr(mesa) : L"") + L")";
        }
    }
    apis.rows.push_back(gl);
    out.push_back(apis);
    return out;
}

std::wstring reportText(const std::vector<InfoSection>& sections)
{
    std::wstring text = L"Venera system information\r\n";
    for (const InfoSection& s : sections) {
        text += L"\r\n" + s.title + L"\r\n";
        for (const InfoRow& r : s.rows) {
            text += L"  " + r.key + L": " + r.value + L"\r\n";
            if (!r.warning.empty())
                text += L"    Warning: " + r.warning + L"\r\n";
        }
    }
    return text;
}

bool anyProblem(const std::vector<InfoSection>& sections)
{
    for (const InfoSection& s : sections)
        for (const InfoRow& r : s.rows)
            if (r.bad)
                return true;
    return false;
}

}  // namespace panel
