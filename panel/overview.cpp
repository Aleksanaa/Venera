// SPDX-License-Identifier: MIT
// Overview: the adapter at a glance, as Settings > System > About shows the PC. A card with the host GPU and a button
// that copies the whole page as text, then expanders with the system report (report.cpp). What needs a graphics driver
// comes from the probe (probe.cpp), run in the background when the page is made and on "Check again".
#include "panel.h"

#include <algorithm>

namespace panel {

using namespace theme;

bool g_mockOverview = false;

namespace {

constexpr UINT kProbeDone = kPageMessage;
constexpr UINT_PTR kCopiedTimer = kPageTimer;
constexpr DWORD kProbeTimeoutMs = 20000;
enum : int { kCopyId = kPageId, kCheckId, kExpanderId, kNameText = kPageId + 100, kValueText };

// A row: its key, then its value in a column of its own, after an error icon when the value is a problem; a warning
// on a line of its own below, indented, after a warning icon. Values and warnings wrap.
constexpr float kValueX = 56 + 180;  // the values' column, from the card's left
constexpr float kGlyphW = 24;        // an icon and its gap
constexpr float kWarningIndent = 16;
constexpr wchar_t kErrorGlyph[] = L"", kWarningGlyph[] = L"";  // StatusErrorFull, Warning (filled)

// A row laid out in a card w wide: its value and warning wrapped to their columns, and the lines' heights, which centre
// them.
struct Row {
    const InfoRow* info;
    Ui::Paragraph value, warning;
    float valueH, warningH = 0;
};

Row layoutRow(Ui& ui, const InfoRow& r, float w)
{
    Row row{&r, ui.wrap(r.value, Ui::kBody, w - 16 - kValueX - (r.bad ? kGlyphW : 0))};
    row.valueH = std::max(32.f, row.value.height + 12);
    if (!r.warning.empty()) {
        row.warning = ui.wrap(r.warning, Ui::kBody, w - 16 - 56 - kWarningIndent - kGlyphW);
        row.warningH = std::max(28.f, row.warning.height + 8);
    }
    return row;
}

// Every warning and problem the report knows, with long values (/mock).
SystemInfo mockInfo()
{
    SystemInfo s;
    s.deviceFound = true;
    s.driverDesc = L"VirtIO GPU (Venus) with Venera D3D10/11/12";
    s.provider = L"Venera project";
    s.ours = true;
    s.problem = 43;
    s.driverVersion = L"0.2.1.140";
    s.driverDate = L"Monday, November 3, 2026";
    s.needRestart = true;
    s.driverFolder = L"C:\\Windows\\System32\\DriverStore\\FileRepository\\venera.inf_amd64_0123456789abcdef";
    s.productVersion = L"0.2.2.159.dbdbd77";
    s.installDate = L"Tuesday, November 10, 2026";
    s.testSigning = false;
    s.kmdAnswered = true;
    s.machine = L"QEMU Standard PC (Q35 + ICH9, 2009)";
    s.machineType = L"pc-q35-11.1";
    s.windows = L"Windows 11 Pro 25H2, build 26200.9711";
    s.cpu = L"AMD RYZEN AI MAX+ 395 w/ Radeon 8060S";
    s.sockets = 2;
    s.cores = 2;
    s.threads = 2;
    s.memory = 8ull << 30;
    s.usableMemory = s.memory - (100ull << 20);
    s.displays = {{2464, 1570, 60, 150}, {1920, 1080, 144, 100}};
    s.otherVulkanDrivers = {L"C:\\Program Files\\Some Vendor\\Graphics Runtime\\vulkan\\some_vendor_icd64.json",
                            L"C:\\Windows\\SysWOW64\\leftover_yttrium_icd32.json"};
    s.settings = {{L"DXVK_HUD", L"fps,frametimes,gpuload,memory,version,api,compiler"},
                  {L"MESA_LOG_LEVEL", L"debug"},
                  {L"VENERA_LOG", L"C:\\venera\\venera.log"}};
    s.environment = {{L"VK_INSTANCE_LAYERS", L"VK_LAYER_KHRONOS_validation"},
                     {L"VKD3D_CONFIG", L"dxr,force_static_cbv,no_upload_hvv,single_queue"}};
    return s;
}

ProbeResult mockProbe()
{
    const std::wstring folder =
        L"C:\\Windows\\System32\\DriverStore\\FileRepository\\venera.inf_amd64_0123456789abcdef";
    return {{"vk.status", L"ok"},
            {"vk.device", L"Virtio-GPU Venus (AMD Radeon 8060S Graphics (RADV STRIX_HALO))"},
            {"vk.host_driver_info", L"Mesa 26.2.5"},
            {"vk.api", L"1.4.334"},
            {"vk.driver", L"venus"},
            {"vk.driver_info", L"Mesa 26.1.0"},
            {"vk.heap.0", L"6227702784 m"},
            {"vk.heap.1", L"12562141184 dm"},
            {"d3d12.status", L"error 0x887a0004"},
            {"d3d11.status", L"ok"},
            {"d3d11.level", L"12_1"},
            {"gl.status", L"ok"},
            {"gl.version", L"1.1.0"},
            {"gl.renderer", L"GDI Generic"},
            {"dxgi.dedicated", L"0"},
            {"dxgi.shared", L"4294967296"},
            {"dxgi.budget_local", L"3865470566"},
            {"dxgi.budget_non_local", L"0"},
            {"module.venera_d3d11.dll", L"C:\\Windows\\System32\\venera_d3d11.dll"},
            {"module.venera_vulkan_virtio.dll", folder + L"\\venera_vulkan_virtio.dll"},
            {"done", L"1"}};
}

class Overview : public Page {
public:
    explicit Overview(HWND window) : window_(window), info_(g_mockOverview ? mockInfo() : readSystemInfo())
    {
        if (g_mockOverview)
            probe_ = mockProbe();
        else
            check();
    }

    const wchar_t* title() const override { return L"Overview"; }
    const wchar_t* icon() const override { return L""; }
    void draw(Ui& ui, float x, float w, float& y) override;
    void click(int id) override;
    void refresh() override
    {
        if (!g_mockOverview)
            info_.displays = readDisplays(info_.ours ? info_.driverDesc : std::wstring());
    }
    bool message(UINT msg, WPARAM wp, LPARAM lp) override;

private:
    void check()
    {
        probing_ = true;
        startProbe(window_, kProbeDone, kProbeTimeoutMs);
    }
    const ProbeResult* probe() const { return probing_ ? nullptr : &probe_; }

    HWND window_;
    SystemInfo info_;
    ProbeResult probe_;
    bool probing_ = false, copied_ = false;
    bool collapsed_[8] = {};
};

void Overview::draw(Ui& ui, float x, float w, float& y)
{
    // the adapter
    ui.card({x, y, x + w, y + 104});
    std::wstring gpu, vkDriver;
    hostGpu(probe(), gpu, vkDriver);
    std::wstring name = !gpu.empty() ? gpu : info_.ours ? L"VirtIO GPU" : info_.driverDesc;
    std::wstring sub = info_.ours ? L"VirtIO GPU through Venus" : info_.deviceFound ? L"VirtIO GPU" : L"";
    ui.text(L"", Ui::kGlyph, {x + 24, y, x + 72, y + 104}, accent, 1, DWRITE_TEXT_ALIGNMENT_CENTER);
    const wchar_t* label = copied_ ? L"Copied" : L"Copy";
    float bw = ui.buttonWidth(label);
    ui.selectable(kNameText, name, Ui::kSubtitle, {x + 96, y + 24, x + w - bw - 40, y + 54});
    ui.text(sub, Ui::kBody, {x + 96, y + 56, x + w - bw - 40, y + 78}, kWhite, kTextSecondary);
    ui.button(label, x + w - 24 - bw, y + 36, kCopyId, false);
    y += 104 + 24;

    // expanders: header row, then (unless collapsed) a divider and the rows
    std::vector<InfoSection> all = describe(info_, probe());
    int text = kValueText;  // the values are selectable, numbered in order
    for (size_t i = 0; i < all.size(); i++) {
        const InfoSection& s = all[i];
        bool open = !collapsed_[i];
        int headerId = kExpanderId + int(i);
        float h = 64;
        std::vector<Row> rows;
        if (open) {
            h += 1 + 12 + 12;
            for (const InfoRow& r : s.rows) {
                rows.push_back(layoutRow(ui, r, w));
                h += rows.back().valueH + rows.back().warningH;
            }
        }
        ui.card({x, y, x + w, y + h});
        if (ui.hover == headerId)
            ui.fill({x + 1, y + 1, x + w - 1, y + 64}, 4, kWhite, kCardHover - kCardFill);
        ui.hot({x, y, x + w, y + 64}, headerId);
        ui.text(s.icon, Ui::kIconLarge, {x + 16, y, x + 36, y + 64}, kWhite, 1, DWRITE_TEXT_ALIGNMENT_CENTER);
        ui.text(s.title, Ui::kBody, {x + 56, y, x + w - 260, y + 64});
        if (s.title == L"Graphics APIs" && !probing_)
            ui.button(L"Check again", x + w - 52 - ui.buttonWidth(L"Check again"), y + 16, kCheckId, false);
        ui.text(open ? L"" : L"", Ui::kIconSmall, {x + w - 44, y, x + w - 16, y + 64}, kWhite,
                kTextPrimary, DWRITE_TEXT_ALIGNMENT_CENTER);
        if (open) {
            ui.fill({x + 1, y + 64, x + w - 1, y + 65}, 0, kBlack, 0.25f);
            float ry = y + 65 + 12;
            for (const Row& r : rows) {
                ui.text(r.info->key, Ui::kBody, {x + 56, ry, x + kValueX, ry + 32}, kWhite, kTextSecondary);
                float vx = x + kValueX;
                if (r.info->bad) {
                    ui.text(kErrorGlyph, Ui::kIcon, {vx, ry, vx + 16, ry + 32}, kCritical, 1);
                    vx += kGlyphW;
                }
                ui.selectable(text++, r.value, vx, ry + (r.valueH - r.value.height) / 2);
                ry += r.valueH;
                if (r.warningH) {
                    // indented under the key, its icon on the first line
                    float wx = x + 56 + kWarningIndent;
                    ui.text(kWarningGlyph, Ui::kIcon, {wx, ry, wx + 16, ry + 28}, kCaution, 1);
                    ui.selectable(text++, r.warning, wx + kGlyphW, ry + (r.warningH - r.warning.height) / 2, kWhite,
                                  kTextSecondary);
                    ry += r.warningH;
                }
            }
        }
        y += h + 4;
    }
}

void Overview::click(int id)
{
    if (id == kCopyId) {
        setClipboard(window_, reportText(describe(info_, probe())));
        copied_ = true;
        SetTimer(window_, kCopiedTimer, 2000, nullptr);
    } else if (id == kCheckId && !g_mockOverview) {
        info_ = readSystemInfo();
        check();
    } else if (id >= kExpanderId && id < kExpanderId + 8) {
        collapsed_[id - kExpanderId] = !collapsed_[id - kExpanderId];
    }
}

bool Overview::message(UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == kProbeDone) {
        std::unique_ptr<ProbeResult> result(reinterpret_cast<ProbeResult*>(lp));
        probe_ = std::move(*result);
        probing_ = false;
        return true;
    }
    if (msg == WM_TIMER && wp == kCopiedTimer) {
        KillTimer(window_, kCopiedTimer);
        copied_ = false;
        return true;
    }
    return false;
}

}  // namespace

std::unique_ptr<Page> makeOverview(HWND window) { return std::make_unique<Overview>(window); }

}  // namespace panel
