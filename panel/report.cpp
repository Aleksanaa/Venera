// SPDX-License-Identifier: MIT
// The system report: what the Overview shows and `/info` prints, from the system information and the probe's result.
#include "panel.h"
#include "debugjson.h"

#include <cwchar>

namespace panel {

namespace {

const char* messageId(unsigned id)
{
    switch (id) {
    case IDS_ReportDriver: return "report.driver";
    case IDS_ReportVersion: return "report.version";
    case IDS_ReportInstalled: return "report.installed";
    case IDS_ReportDevice: return "report.device";
    case IDS_ReportNoDevice: return "report.noDevice";
    case IDS_ReportDeviceProblem: return "report.deviceProblem";
    case IDS_ReportDeviceDriver: return "report.deviceDriver";
    case IDS_ReportRestart: return "report.restart";
    case IDS_ReportDriverVersion: return "report.driverVersion";
    case IDS_ReportVersionMismatch: return "report.versionMismatch";
    case IDS_ReportDriverFiles: return "report.driverFiles";
    case IDS_ReportFilesMixed: return "report.filesMixed";
    case IDS_ReportFilesOutside: return "report.filesOutside";
    case IDS_ReportFilesInstalled: return "report.filesInstalled";
    case IDS_ReportTestSigning: return "report.testSigning";
    case IDS_ReportSigningOff: return "report.signingOff";
    case IDS_ReportHost: return "report.host";
    case IDS_ReportVulkanDriver: return "report.vulkanDriver";
    case IDS_ReportVenusUnknown: return "report.venusUnknown";
    case IDS_ReportGpuFailed: return "report.gpuFailed";
    case IDS_ReportAcceleration: return "report.acceleration";
    case IDS_ReportAccelerationOff: return "report.accelerationOff";
    case IDS_ReportVenusOff: return "report.venusOff";
    case IDS_ReportBlob: return "report.blob";
    case IDS_ReportBlobOff: return "report.blobOff";
    case IDS_ReportHostMemory: return "report.hostMemory";
    case IDS_ReportHostMemoryOff: return "report.hostMemoryOff";
    case IDS_ReportContext: return "report.context";
    case IDS_ReportContextOff: return "report.contextOff";
    case IDS_ReportFeatures: return "report.features";
    case IDS_ReportDriverUnknown: return "report.driverUnknown";
    case IDS_ReportGuest: return "report.guest";
    case IDS_ReportVm: return "report.vm";
    case IDS_ReportMachine: return "report.machine";
    case IDS_ReportProcessor: return "report.processor";
    case IDS_ReportTopology: return "report.topology";
    case IDS_ReportTopologyValue: return "report.topologyValue";
    case IDS_ReportSocketsWarning: return "report.socketsWarning";
    case IDS_ReportCoresWarning: return "report.coresWarning";
    case IDS_ReportMemory: return "report.memory";
    case IDS_ReportMemoryValue: return "report.memoryValue";
    case IDS_ReportSwap: return "report.swap";
    case IDS_ReportDisplay: return "report.display";
    case IDS_ReportDisplayNumber: return "report.displayNumber";
    case IDS_ReportDisplayValue: return "report.displayValue";
    case IDS_ReportDisplayMissing: return "report.displayMissing";
    case IDS_ReportMouse: return "report.mouse";
    case IDS_ReportKeyboard: return "report.keyboard";
    case IDS_ReportInput: return "report.input";
    case IDS_ReportInputMissing: return "report.inputMissing";
    case IDS_ReportApis: return "report.apis";
    case IDS_ReportFeatureLevel: return "report.featureLevel";
    case IDS_ReportD3d12Features: return "report.d3d12Features";
    case IDS_ReportD3d9On12: return "report.d3d9On12";
    case IDS_ReportD3d12Needed: return "report.d3d12Needed";
    case IDS_ReportOtherVulkan: return "report.otherVulkan";
    case IDS_ReportOtherVulkanWarning: return "report.otherVulkanWarning";
    case IDS_ReportOtherVulkanWarnings: return "report.otherVulkanWarnings";
    case IDS_ReportSoftwareGl: return "report.softwareGl";
    case IDS_ReportVideoMemory: return "report.videoMemory";
    case IDS_ReportDedicated: return "report.dedicated";
    case IDS_ReportShared: return "report.shared";
    case IDS_ReportBudget: return "report.budget";
    case IDS_ReportBudgetValue: return "report.budgetValue";
    case IDS_ReportHeap: return "report.heap";
    case IDS_ReportHeapLocal: return "report.heapLocal";
    case IDS_ReportHeapMappable: return "report.heapMappable";
    case IDS_ReportHeapBoth: return "report.heapBoth";
    case IDS_ReportMemoryUnknown: return "report.memoryUnknown";
    case IDS_ReportChangedSettings: return "report.changedSettings";
    case IDS_ReportEnvironment: return "report.environment";
    case IDS_ReportDefaultSettings: return "report.defaultSettings";
    case IDS_ReportCheckCrashed: return "report.checkCrashed";
    case IDS_ReportCheckTimeout: return "report.checkTimeout";
    case IDS_ReportNotChecked: return "report.notChecked";
    case IDS_ReportNoAdapter: return "report.noAdapter";
    case IDS_ReportNoLoader: return "report.noLoader";
    case IDS_ReportNoVenus: return "report.noVenus";
    case IDS_ReportApiFailed: return "report.apiFailed";
    case IDS_NotInstalled: return "report.notInstalled";
    case IDS_Checking: return "report.checking";
    case IDS_None: return "report.none";
    case IDS_On: return "report.on";
    case IDS_Settings: return "report.settings";
    default: return nullptr;
    }
}

void addMessage(debugjson::Json& object, const char* field, const locale::Message& message)
{
    const char* id = messageId(message.id);
    if (!id)
        return;
    auto args = debugjson::Json::array();
    for (const auto& arg : message.args)
        args.push_back(narrow(arg));
    debugjson::Json metadata = {{"id", id}, {"args", std::move(args)}};
    if (!message.diagnostic.empty())
        metadata["diagnostic"] = narrow(message.diagnostic);
    object[field] = std::move(metadata);
}

InfoRow row(locale::Message key, locale::Message value, bool bad = false, locale::Message warning = {})
{
    return {key.resolve(locale::Language::English), value.resolve(locale::Language::English), bad,
            warning.resolve(locale::Language::English), std::move(key), std::move(value), std::move(warning)};
}

void warn(InfoRow& row, locale::Message warning)
{
    row.warning = warning.resolve(locale::Language::English);
    row.displayWarning = std::move(warning);
}

InfoSection section(const wchar_t* icon, unsigned id)
{
    return {icon, locale::text(id, locale::Language::English), {}, id};
}

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

InfoRow api(const ProbeResult* probe, const wchar_t* name, const char* prefix, const locale::Message& okText)
{
    if (!probe)
        return row(name, {IDS_Checking});
    std::wstring s = probed(probe, (std::string(prefix) + ".status").c_str());
    if (s == L"ok")
        return row(name, okText);
    if (s == L"checking")  // started but never finished
        return row(name, {probed(probe, "timeout").empty() ? IDS_ReportCheckCrashed : IDS_ReportCheckTimeout}, true);
    if (s.empty())
        return row(name, {IDS_ReportNotChecked}, true);
    if (s == L"no adapter")
        return row(name, {IDS_ReportNoAdapter}, true);
    if (s == L"no loader")
        return row(name, {IDS_ReportNoLoader}, true);
    if (s == L"no device")
        return row(name, {IDS_ReportNoVenus}, true);
    return row(name, {IDS_ReportApiFailed, {s}}, true);
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
    const std::wstring status = probed(probe, "venus.status");
    std::wstring name = status.empty() ? probed(probe, "vk.device")
                                     : status == L"ok" ? probed(probe, "venus.device") : L"";
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

    InfoSection driver = section(L"", IDS_ReportDriver);
    driver.rows.push_back(row({IDS_ReportVersion}, info.productVersion.empty() ? locale::Message{IDS_NotInstalled} : locale::Message{info.productVersion},
                           info.productVersion.empty()));
    if (!info.installDate.empty())
        driver.rows.push_back(row({IDS_ReportInstalled}, info.installDate));
    if (!info.deviceFound) {
        driver.rows.push_back(row({IDS_ReportDevice}, {IDS_ReportNoDevice}, true));
    } else {
        if (info.problem)
            driver.rows.push_back(
                row({IDS_ReportDevice}, {IDS_ReportDeviceProblem, {std::to_wstring(info.problem)}}, true));
        InfoRow device = row({IDS_ReportDeviceDriver}, info.driverDesc.empty() ? locale::Message{IDS_None} : locale::Message{info.driverDesc}, !info.ours);
        if (info.needRestart)
            warn(device, {IDS_ReportRestart});
        driver.rows.push_back(device);
        if (!info.driverVersion.empty()) {
            // The package's DriverVer is Venera's version without the commit hash (setup/payload.cmake).
            InfoRow version = row({IDS_ReportDriverVersion},
                            info.driverVersion + (info.driverDate.empty() ? L"" : L", " + info.driverDate));
            if (info.ours && !info.productVersion.empty() &&
                info.productVersion.compare(0, info.driverVersion.size() + 1, info.driverVersion + L".")) {
                warn(version, {IDS_ReportVersionMismatch, {info.productVersion}});
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
                driver.rows.push_back(row({IDS_ReportDriverFiles}, {IDS_ReportFilesMixed}, false,
                                       {IDS_ReportFilesOutside, {stale}}));
            else if (any)
                driver.rows.push_back(row({IDS_ReportDriverFiles}, {IDS_ReportFilesInstalled}));
        }
    }
    driver.rows.push_back(row({IDS_ReportTestSigning}, {info.testSigning ? IDS_On : IDS_ReportSigningOff},
                           !info.testSigning));
    out.push_back(driver);

    InfoSection host = section(L"", IDS_ReportHost);
    std::wstring gpu, vkDriver;
    hostGpu(probe, gpu, vkDriver);
    if (!probe)
        host.rows.push_back(row(L"GPU", {IDS_Checking}));
    else if (!gpu.empty()) {
        host.rows.push_back({L"GPU", gpu});
        // "RADV STRIX_HALO" from the device name, and the driver's own "Mesa 25.2.4" where Venus passes it on
        const bool venus = !probed(probe, "venus.status").empty();
        std::wstring info = probed(probe, venus ? "venus.host_driver_info" : "vk.host_driver_info");
        if (vkDriver.empty())
            vkDriver = probed(probe, venus ? "venus.host_driver" : "vk.host_driver");
        if (!info.empty())
            vkDriver += (vkDriver.empty() ? L"" : L", ") + info;
        if (!vkDriver.empty())
            host.rows.push_back(row({IDS_ReportVulkanDriver}, vkDriver));
    } else if (!probed(probe, "venus.status").empty()) {
        const std::wstring status = probed(probe, "venus.status");
        if (status == L"checking")
            host.rows.push_back(row(L"GPU", {probed(probe, "timeout").empty() ? IDS_ReportCheckCrashed
                                                                           : IDS_ReportCheckTimeout}, true));
        else if (status == L"no device")
            host.rows.push_back(row(L"GPU", {IDS_ReportNoVenus}, true));
        else
            host.rows.push_back(row(L"GPU", {IDS_ReportGpuFailed, {status}}, true));
    } else
        host.rows.push_back(row(L"GPU", {IDS_ReportVenusUnknown}, true));
    // The virtio-gpu device's options on QEMU's command line (docs/qemu.md), which the guest sees only as features.
    if (info.kmdAnswered) {
        auto feature = [&](locale::Message name, bool on, int off) {
            host.rows.push_back(row(name, {on ? IDS_On : off}, !on));
        };
        feature({IDS_ReportAcceleration}, info.supports3d, IDS_ReportAccelerationOff);
        feature(L"Venus", info.venus, IDS_ReportVenusOff);
        feature({IDS_ReportBlob}, info.blob, IDS_ReportBlobOff);
        if (!info.hostVisible)
            host.rows.push_back(row({IDS_ReportHostMemory}, {IDS_ReportHostMemoryOff}, true));
        else
            host.rows.push_back(
                row({IDS_ReportHostMemory}, info.hostVisibleSize ? locale::Message{gigabytes(info.hostVisibleSize)} : locale::Message{IDS_On}));
        feature({IDS_ReportContext}, info.contextInit, IDS_ReportContextOff);
    } else if (info.ours) {
        host.rows.push_back(row({IDS_ReportFeatures}, {IDS_ReportDriverUnknown}, true));
    }
    out.push_back(host);

    InfoSection guest = section(L"", IDS_ReportGuest);
    if (!info.machine.empty())
        guest.rows.push_back(row({IDS_ReportVm}, info.machine));
    if (!info.machineType.empty())
        guest.rows.push_back(row({IDS_ReportMachine}, info.machineType));
    if (!info.windows.empty())
        guest.rows.push_back({L"Windows", info.windows});
    if (!info.cpu.empty())
        guest.rows.push_back(row({IDS_ReportProcessor}, info.cpu));
    if (info.threads) {
        auto count = [](DWORD n, const wchar_t* one) {
            return std::to_wstring(n) + L" " + one + (n == 1 ? L"" : L"s");
        };
        // Two sockets of one core each: likely several sockets configured, of which Windows uses at most two. Fewer
        // than four cores: applications race the drivers' translation threads for them. One socket of at least four
        // cores is what to configure.
        InfoRow topology = row({IDS_ReportTopology}, count(info.sockets, L"socket") + L", " + count(info.cores, L"core") + L", " +
                                          count(info.threads, L"thread"));
        topology.displayValue = {IDS_ReportTopologyValue, {std::to_wstring(info.sockets), std::to_wstring(info.cores), std::to_wstring(info.threads)}};
        if (info.sockets == 2 && info.cores == 2)
            warn(topology, {IDS_ReportSocketsWarning});
        else if (info.cores < 4)
            warn(topology, {IDS_ReportCoresWarning});
        guest.rows.push_back(topology);
    }
    if (info.memory)
        guest.rows.push_back(
            row({IDS_ReportMemory}, {IDS_ReportMemoryValue, {gigabytes(info.memory), gigabytes(info.usableMemory)}}));
    guest.rows.push_back(row({IDS_ReportSwap}, info.swap ? locale::Message{gigabytes(info.swap)} : locale::Message{IDS_None}));
    for (size_t i = 0; i < info.displays.size(); i++) {
        const Display& d = info.displays[i];
        guest.rows.push_back(row(info.displays.size() > 1 ? locale::Message{IDS_ReportDisplayNumber, {std::to_wstring(i + 1)}} : locale::Message{IDS_ReportDisplay},
            {IDS_ReportDisplayValue, {std::to_wstring(d.width), std::to_wstring(d.height), std::to_wstring(d.hz), std::to_wstring(d.scale)}}));
    }
    if (info.displays.empty())
        guest.rows.push_back(row({IDS_ReportDisplay}, {IDS_ReportDisplayMissing}));
    for (const std::wstring& m : info.mice)
        guest.rows.push_back(row({IDS_ReportMouse}, m));
    for (const std::wstring& k : info.keyboards)
        guest.rows.push_back(row({IDS_ReportKeyboard}, k));
    if (info.mice.empty() && info.keyboards.empty())
        guest.rows.push_back(row({IDS_ReportInput}, {IDS_ReportInputMissing}));
    out.push_back(guest);

    InfoSection apis = section(L"", IDS_ReportApis);
    apis.rows.push_back(api(probe, L"Direct3D 12", "d3d12",
                            {IDS_ReportD3d12Features, {probed(probe, "d3d12.level"), probed(probe, "d3d12.shader_model")}}));
    apis.rows.push_back(api(probe, L"Direct3D 11", "d3d11", {IDS_ReportFeatureLevel, {probed(probe, "d3d11.level")}}));
    if (!probe)
        apis.rows.push_back(row(L"Direct3D 9", {IDS_Checking}));
    else
        apis.rows.push_back(row(L"Direct3D 9", {apiOk(probe, "d3d12") ? IDS_ReportD3d9On12 : IDS_ReportD3d12Needed}, !apiOk(probe, "d3d12")));
    apis.rows.push_back(api(probe, L"Vulkan", "vk",
                            probed(probe, "vk.api") + L" (" + probed(probe, "vk.driver") + L", " +
                                probed(probe, "vk.driver_info") + L")"));
    // Another program's driver, which applications may pick over ours or which may break the loader.
    for (const std::wstring& d : info.otherVulkanDrivers)
        apis.rows.push_back(row({IDS_ReportOtherVulkan}, d));
    if (!info.otherVulkanDrivers.empty())
        warn(apis.rows.back(), {info.otherVulkanDrivers.size() > 1 ? IDS_ReportOtherVulkanWarnings : IDS_ReportOtherVulkanWarning});
    // OpenGL: "4.6 (Compatibility Profile) Mesa 26.1.0" from zink; Windows' own renderer is "GDI Generic".
    std::wstring glVersion = probed(probe, "gl.version"), renderer = probed(probe, "gl.renderer");
    InfoRow gl = api(probe, L"OpenGL", "gl", L"");
    if (apiOk(probe, "gl")) {
        if (renderer.find(L"GDI Generic") != std::wstring::npos) {
            gl = row(L"OpenGL", {IDS_ReportSoftwareGl}, true);
        } else {
            std::wstring v = glVersion.substr(0, glVersion.find(L' '));
            size_t mesa = glVersion.find(L"Mesa ");
            std::wstring by = renderer.find(L"zink") != std::wstring::npos ? L"zink" : renderer;
            gl.value = v + L" (" + by + (mesa != std::wstring::npos ? L", " + glVersion.substr(mesa) : L"") + L")";
            gl.displayValue = gl.value;
        }
    }
    apis.rows.push_back(gl);
    out.push_back(apis);

    // What programs may allocate: DXGI's sizes and per-process budgets, and the Vulkan heaps Venus offers.
    InfoSection memory = section(L"", IDS_ReportVideoMemory);
    auto bytes = [&](const char* key) { return gigabytes(std::wcstoull(probed(probe, key).c_str(), nullptr, 10)); };
    if (!probe) {
        memory.rows.push_back(row({IDS_ReportMemory}, {IDS_Checking}));
    } else {
        if (!probed(probe, "dxgi.dedicated").empty()) {
            memory.rows.push_back(row({IDS_ReportDedicated}, bytes("dxgi.dedicated")));
            memory.rows.push_back(row({IDS_ReportShared}, bytes("dxgi.shared")));
        }
        if (!probed(probe, "dxgi.budget_local").empty())
            memory.rows.push_back(row({IDS_ReportBudget}, {IDS_ReportBudgetValue, {bytes("dxgi.budget_local"), bytes("dxgi.budget_non_local")}}));
        for (int h = 0;; h++) {
            const char* prefix = apiOk(probe, "venus") ? "venus.heap." : "vk.heap.";
            std::wstring heap = probed(probe, (prefix + std::to_string(h)).c_str());
            if (heap.empty())
                break;
            size_t space = heap.find(L' ');
            std::wstring flags = space == std::wstring::npos ? L"" : heap.substr(space + 1);
            std::wstring value = gigabytes(std::wcstoull(heap.c_str(), nullptr, 10));
            bool local = flags.find(L'd') != std::wstring::npos, mappable = flags.find(L'm') != std::wstring::npos;
            locale::Message description = value;
            if (local || mappable)
                description = {local && mappable ? IDS_ReportHeapBoth : local ? IDS_ReportHeapLocal : IDS_ReportHeapMappable, {value}};
            memory.rows.push_back(row({IDS_ReportHeap, {std::to_wstring(h)}}, description));
        }
        if (memory.rows.empty())
            memory.rows.push_back(row({IDS_ReportMemory}, {IDS_ReportMemoryUnknown}, true));
    }
    out.push_back(memory);

    // What a forgotten log level or HUD would explain: everything not at its default.
    InfoSection settings = section(L"", IDS_ReportChangedSettings);
    for (const auto& [name, value] : info.settings)
        settings.rows.push_back({name, value});
    for (const auto& [name, value] : info.environment)
        settings.rows.push_back(row(name, {IDS_ReportEnvironment, {value}}));
    if (settings.rows.empty())
        settings.rows.push_back(row({IDS_Settings}, {IDS_ReportDefaultSettings}));
    out.push_back(settings);
    return out;
}

std::wstring reportText(const std::vector<InfoSection>& sections, bool localized)
{
    std::wstring text = std::wstring(locale::text(IDS_ReportTitle, localized ? locale::language() : locale::Language::English)) + L"\r\n";
    for (const InfoSection& s : sections) {
        text += L"\r\n" + (localized ? s.titleText() : s.title) + L"\r\n";
        for (const InfoRow& r : s.rows) {
            text += L"  " + (localized ? r.keyText() : r.key) + L": " + (localized ? r.valueText() : r.value) + L"\r\n";
            if (!r.warning.empty())
                text += L"    " + locale::Message(IDS_ReportWarning, {localized ? r.warningText() : r.warning}).resolve(localized ? locale::language() : locale::Language::English) + L"\r\n";
        }
    }
    return text;
}

std::string reportJson(const std::vector<InfoSection>& sections)
{
    auto entries = debugjson::Json::array();
    for (const InfoSection& s : sections) {
        auto rows = debugjson::Json::array();
        for (const InfoRow& r : s.rows) {
            debugjson::Json entry = {{"key", narrow(r.key)}, {"value", narrow(r.value)},
                                    {"bad", r.bad}, {"warning", narrow(r.warning)}};
            addMessage(entry, "key_message", r.displayKey);
            addMessage(entry, "value_message", r.displayValue);
            addMessage(entry, "warning_message", r.displayWarning);
            rows.push_back(std::move(entry));
        }
        debugjson::Json entry = {{"title", narrow(s.title)}, {"rows", std::move(rows)}};
        addMessage(entry, "title_message", locale::Message(s.titleId));
        entries.push_back(std::move(entry));
    }
    return debugjson::Json({{"sections", std::move(entries)}}).dump();
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
