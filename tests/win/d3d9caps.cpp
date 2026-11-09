// SPDX-License-Identifier: MIT
// Prints what D3D9 (D3D9On12 on venera_d3d12, or venera_d3d12_32 in a 32-bit process) reports: the adapter, the
// device caps, and per format whether it is a texture, a render target, filterable and blendable as a render
// target (D3DUSAGE_QUERY_POSTPIXELSHADER_BLENDING: Unreal Engine 3 refuses to start without it for
// D3DFMT_A16B16G16R16F). Then draws for real: blends a half-transparent quad into an A16B16G16R16F render target and
// reads the result back, and lists the driver DLLs the process loaded. Built for x86_64 (d3d9caps) and i686
// (d3d9caps_32, in the 32-bit build tree: build\win\umd32\tests\win).
//   d3d9caps [output file]
#include <windows.h>
#include <d3d9.h>
#include <tlhelp32.h>

#include <cmath>
#include <cstdio>
#include <cstring>

namespace {

FILE* out = stdout;

struct Format {
    D3DFORMAT format;
    const char* name;
};

const Format kFormats[] = {
    {D3DFMT_A8R8G8B8, "A8R8G8B8"},           {D3DFMT_X8R8G8B8, "X8R8G8B8"},
    {D3DFMT_A2R10G10B10, "A2R10G10B10"},     {D3DFMT_A16B16G16R16, "A16B16G16R16"},
    {D3DFMT_R16F, "R16F"},                   {D3DFMT_G16R16F, "G16R16F"},
    {D3DFMT_A16B16G16R16F, "A16B16G16R16F"}, {D3DFMT_R32F, "R32F"},
    {D3DFMT_G32R32F, "G32R32F"},             {D3DFMT_A32B32G32R32F, "A32B32G32R32F"},
};

bool check(IDirect3D9* d3d, D3DFORMAT format, DWORD usage)
{
    return SUCCEEDED(d3d->CheckDeviceFormat(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, usage,
                                            D3DRTYPE_TEXTURE, format));
}

float half_to_float(WORD h)
{
    const int exponent = (h >> 10) & 0x1f;
    const int mantissa = h & 0x3ff;
    float v = exponent == 0 ? std::ldexp(float(mantissa), -24) : std::ldexp(float(mantissa | 0x400), exponent - 25);
    return (h & 0x8000) ? -v : v;
}

// Clears an A16B16G16R16F render target to 0.25 and blends a quad of color 1.0 at alpha 0.5 over it
// (SRCALPHA, INVSRCALPHA): every channel but alpha should read 0.625.
bool blend_test(IDirect3DDevice9* dev)
{
    constexpr UINT kSize = 16;
    IDirect3DTexture9* rt = nullptr;
    IDirect3DSurface9* rt_surface = nullptr;
    IDirect3DSurface9* readback = nullptr;
    bool ok = false;
    HRESULT hr = dev->CreateTexture(kSize, kSize, 1, D3DUSAGE_RENDERTARGET, D3DFMT_A16B16G16R16F, D3DPOOL_DEFAULT,
                                    &rt, nullptr);
    std::fprintf(out, "blend: CreateTexture(A16B16G16R16F, RENDERTARGET): 0x%08lx\n", hr);
    if (SUCCEEDED(hr)) {
        rt->GetSurfaceLevel(0, &rt_surface);
        hr = dev->CreateOffscreenPlainSurface(kSize, kSize, D3DFMT_A16B16G16R16F, D3DPOOL_SYSTEMMEM, &readback,
                                              nullptr);
        std::fprintf(out, "blend: CreateOffscreenPlainSurface(SYSTEMMEM): 0x%08lx\n", hr);
    }
    if (readback) {
        struct Vertex {
            float x, y, z, rhw;
            D3DCOLOR color;
        };
        const float s = float(kSize) - 0.5f;
        const D3DCOLOR color = D3DCOLOR_ARGB(0x80, 0xff, 0xff, 0xff);
        const Vertex quad[] = {
            {-0.5f, -0.5f, 0, 1, color}, {s, -0.5f, 0, 1, color}, {-0.5f, s, 0, 1, color}, {s, s, 0, 1, color}};
        dev->SetRenderTarget(0, rt_surface);
        dev->Clear(0, nullptr, D3DCLEAR_TARGET, D3DCOLOR_COLORVALUE(0.25f, 0.25f, 0.25f, 0.25f), 1.0f, 0);
        dev->BeginScene();
        dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
        dev->SetRenderState(D3DRS_LIGHTING, FALSE);
        dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
        dev->SetRenderState(D3DRS_ZENABLE, D3DZB_FALSE);
        dev->SetRenderState(D3DRS_ALPHABLENDENABLE, TRUE);
        dev->SetRenderState(D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
        dev->SetRenderState(D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
        dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
        dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
        dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
        dev->SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_DIFFUSE);
        hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLESTRIP, 2, quad, sizeof(Vertex));
        dev->EndScene();
        std::fprintf(out, "blend: DrawPrimitiveUP: 0x%08lx\n", hr);
        hr = dev->GetRenderTargetData(rt_surface, readback);
        std::fprintf(out, "blend: GetRenderTargetData: 0x%08lx\n", hr);
        D3DLOCKED_RECT lr{};
        if (SUCCEEDED(hr) && SUCCEEDED(readback->LockRect(&lr, nullptr, D3DLOCK_READONLY))) {
            const WORD* px = reinterpret_cast<const WORD*>(static_cast<const BYTE*>(lr.pBits) + lr.Pitch * (kSize / 2)) +
                             4 * (kSize / 2);
            const float r = half_to_float(px[0]), g = half_to_float(px[1]), b = half_to_float(px[2]),
                        a = half_to_float(px[3]);
            readback->UnlockRect();
            // 0.5 alpha is 0x80/255: allow for it.
            ok = std::fabs(r - 0.625f) < 0.01f && std::fabs(g - 0.625f) < 0.01f && std::fabs(b - 0.625f) < 0.01f;
            std::fprintf(out, "blend: center pixel r=%.4f g=%.4f b=%.4f a=%.4f (expected rgb 0.625): %s\n", r, g, b, a,
                         ok ? "PASS" : "FAIL");
        }
    }
    if (readback)
        readback->Release();
    if (rt_surface)
        rt_surface->Release();
    if (rt)
        rt->Release();
    return ok;
}

// The driver DLLs in this process: which D3D9 implementation runs, and on which UMD.
void list_driver_modules()
{
    HANDLE snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, GetCurrentProcessId());
    if (snap == INVALID_HANDLE_VALUE)
        return;
    MODULEENTRY32W me{sizeof(me)};
    for (BOOL ok = Module32FirstW(snap, &me); ok; ok = Module32NextW(snap, &me)) {
        const wchar_t* n = me.szModule;
        if (!_wcsnicmp(n, L"venera", 6) || !_wcsnicmp(n, L"d3d9", 4) || !_wcsnicmp(n, L"d3d12", 5) ||
            !_wcsnicmp(n, L"d3d11", 5) || !_wcsnicmp(n, L"viogpu", 6))
            std::fprintf(out, "module: %ls\n", me.szExePath);
    }
    CloseHandle(snap);
}

} // namespace

int main(int argc, char** argv)
{
    if (argc > 1 && !(out = std::fopen(argv[1], "w"))) {
        std::fprintf(stderr, "cannot write %s\n", argv[1]);
        return 1;
    }
    std::fprintf(out, "process: %u-bit\n", unsigned(sizeof(void*) * 8));
    IDirect3D9* d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) {
        std::fprintf(out, "Direct3DCreate9 failed\n");
        return 1;
    }
    D3DADAPTER_IDENTIFIER9 id{};
    d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &id);
    std::fprintf(out, "adapter: %s (driver %s, %04lx:%04lx)\n", id.Description, id.Driver, id.VendorId, id.DeviceId);

    D3DCAPS9 caps{};
    HRESULT hr = d3d->GetDeviceCaps(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, &caps);
    std::fprintf(out, "caps: 0x%08lx vs %lu.%lu ps %lu.%lu max texture %lux%lu simultaneous RTs %lu\n", hr,
                 D3DSHADER_VERSION_MAJOR(caps.VertexShaderVersion), D3DSHADER_VERSION_MINOR(caps.VertexShaderVersion),
                 D3DSHADER_VERSION_MAJOR(caps.PixelShaderVersion), D3DSHADER_VERSION_MINOR(caps.PixelShaderVersion),
                 caps.MaxTextureWidth, caps.MaxTextureHeight, caps.NumSimultaneousRTs);

    std::fprintf(out, "%-16s %-8s %-6s %-7s %-6s\n", "format", "texture", "rt", "filter", "blend");
    for (const Format& f : kFormats)
        std::fprintf(out, "%-16s %-8s %-6s %-7s %-6s\n", f.name, check(d3d, f.format, 0) ? "yes" : "-",
                     check(d3d, f.format, D3DUSAGE_RENDERTARGET) ? "yes" : "-",
                     check(d3d, f.format, D3DUSAGE_QUERY_FILTER) ? "yes" : "-",
                     check(d3d, f.format, D3DUSAGE_QUERY_POSTPIXELSHADER_BLENDING) ? "yes" : "-");

    // A hidden window for the device; nothing is presented.
    HWND wnd = CreateWindowExW(0, L"STATIC", L"d3d9caps", WS_OVERLAPPEDWINDOW, 0, 0, 64, 64, nullptr, nullptr,
                               GetModuleHandleW(nullptr), nullptr);
    D3DPRESENT_PARAMETERS pp{};
    pp.Windowed = TRUE;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.BackBufferFormat = D3DFMT_UNKNOWN;
    pp.hDeviceWindow = wnd;
    IDirect3DDevice9* dev = nullptr;
    hr = d3d->CreateDevice(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, wnd, D3DCREATE_HARDWARE_VERTEXPROCESSING, &pp, &dev);
    std::fprintf(out, "CreateDevice: 0x%08lx\n", hr);
    bool ok = false;
    if (dev) {
        ok = blend_test(dev);
        list_driver_modules();
        dev->Release();
    }
    d3d->Release();
    DestroyWindow(wnd);
    return ok ? 0 : 2;
}
