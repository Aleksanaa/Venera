// SPDX-License-Identifier: MIT
// Overview: the adapter at a glance, as Settings > System > About shows the PC. A card with the host GPU and a button
// that copies the whole page as text, then expanders with the system report (report.cpp). What needs a graphics driver
// comes from the probe (probe.cpp), run in the background when the page is made and on "Check again".
#include "panel.h"

#include <algorithm>

namespace panel {

using namespace theme;

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

float valueHeight(Ui& ui, const InfoRow& r, float w)
{
    float width = w - 16 - kValueX - (r.bad ? kGlyphW : 0);
    return std::max(32.f, ui.textHeight(r.value, Ui::kBody, width) + 12);
}

float warningHeight(Ui& ui, const InfoRow& r, float w)
{
    if (r.warning.empty())
        return 0;
    float width = w - 16 - 56 - kWarningIndent - kGlyphW;
    return std::max(28.f, ui.textHeight(r.warning, Ui::kBody, width) + 8);
}

float rowHeight(Ui& ui, const InfoRow& r, float w) { return valueHeight(ui, r, w) + warningHeight(ui, r, w); }

class Overview : public Page {
public:
    explicit Overview(HWND window) : window_(window), info_(readSystemInfo()) { check(); }

    const wchar_t* title() const override { return L"Overview"; }
    const wchar_t* icon() const override { return L""; }
    void draw(Ui& ui, float x, float w, float& y) override;
    void click(int id) override;
    void refresh() override { info_.displays = readDisplays(info_.ours ? info_.driverDesc : std::wstring()); }
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
        if (open) {
            h += 1 + 12 + 12;
            for (const InfoRow& r : s.rows)
                h += rowHeight(ui, r, w);
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
            for (const InfoRow& r : s.rows) {
                ui.text(r.key, Ui::kBody, {x + 56, ry, x + kValueX, ry + 32}, kWhite, kTextSecondary);
                float vx = x + kValueX;
                if (r.bad) {
                    ui.text(kErrorGlyph, Ui::kIcon, {vx, ry, vx + 16, ry + 32}, kCritical, 1);
                    vx += kGlyphW;
                }
                float vh = valueHeight(ui, r, w);
                ui.selectable(text++, r.value, Ui::kBody, {vx, ry, x + w - 16, ry + vh}, kWhite, kTextPrimary, 0, true);
                ry += vh;
                if (!r.warning.empty()) {
                    // indented under the key, its icon on the first line
                    float wx = x + 56 + kWarningIndent;
                    float wh = warningHeight(ui, r, w);
                    ui.text(kWarningGlyph, Ui::kIcon, {wx, ry, wx + 16, ry + 28}, kCaution, 1);
                    ui.selectable(text++, r.warning, Ui::kBody, {wx + kGlyphW, ry, x + w - 16, ry + wh}, kWhite,
                                  kTextSecondary, 0, true);
                    ry += wh;
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
    } else if (id == kCheckId) {
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
