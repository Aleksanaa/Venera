// SPDX-License-Identifier: MIT
// Direct3D 11 tests, on a device on the VirtIO GPU through the system runtime, each printing what it checks and
// PASS/FAIL. Shaders are compiled in the guest by d3dcompiler_47.dll. The present tests show swapchains in a window
// for a few seconds each (watch them): flip model, then the legacy blt model, then ResizeBuffers; present-blt runs
// the legacy model without a flip swapchain before it, present-check compares the colours on screen with a GDI
// reference window (keep both uncovered).
#include "tests.h"

#include <d3d11_4.h>
#include <d3dcompiler.h>
#include <winternl.h>  // NTSTATUS, for d3dkmthk.h
#include <d3dkmthk.h>
#include <dxgi1_6.h>
#include <dwmapi.h>

#include "../../umd/common/viogpu_abi.h"

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cwchar>
#include <cstring>
#include <utility>
#include <vector>

namespace {

// The runtime's "Removing Device." debug message, with the stack that removes the device (module + offset).
LONG CALLBACK report_device_removal(EXCEPTION_POINTERS* e)
{
    const EXCEPTION_RECORD* r = e->ExceptionRecord;
    if (r->ExceptionCode != DBG_PRINTEXCEPTION_C || r->NumberParameters < 2 ||
        !std::strstr(reinterpret_cast<const char*>(r->ExceptionInformation[1]), "Removing Device"))
        return EXCEPTION_CONTINUE_SEARCH;
    void* frames[32];
    const USHORT n = RtlCaptureStackBackTrace(0, 32, frames, nullptr);
    std::printf("DEVICE REMOVAL on thread %lu:\n", GetCurrentThreadId());
    for (USHORT i = 0; i < n; i++) {
        HMODULE mod = nullptr;
        char name[MAX_PATH] = "?";
        if (GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                               static_cast<LPCSTR>(frames[i]), &mod))
            GetModuleFileNameA(mod, name, sizeof(name));
        const char* base = std::strrchr(name, '\\');
        std::printf("  %s+0x%llx\n", base ? base + 1 : name,
                    static_cast<unsigned long long>(static_cast<char*>(frames[i]) - reinterpret_cast<char*>(mod)));
    }
    return EXCEPTION_CONTINUE_SEARCH;
}

template <class T>
struct Ref {
    T* p = nullptr;
    Ref() = default;
    Ref(const Ref&) = delete;
    ~Ref() { reset(); }
    void reset()
    {
        if (p)
            p->Release();
        p = nullptr;
    }
    T** put() { return &p; }
    T* operator->() const { return p; }
    operator T*() const { return p; }
};

using D3DCompileFn = HRESULT(WINAPI*)(LPCVOID, SIZE_T, LPCSTR, const D3D_SHADER_MACRO*, ID3DInclude*, LPCSTR, LPCSTR,
                                      UINT, UINT, ID3DBlob**, ID3DBlob**);
D3DCompileFn g_compile = nullptr;

bool compile(const char* source, const char* entry, const char* target, Ref<ID3DBlob>* out)
{
    Ref<ID3DBlob> errors;
    HRESULT hr = g_compile(source, std::strlen(source), entry, nullptr, nullptr, entry, target, 0, 0, out->put(),
                           errors.put());
    if (FAILED(hr)) {
        std::printf("    compile %s (%s) failed: 0x%08lx %s\n", entry, target, hr,
                    errors ? static_cast<const char*>(errors->GetBufferPointer()) : "");
        return false;
    }
    return true;
}

bool check(const char* test, const char* what, HRESULT hr)
{
    if (FAILED(hr))
        std::printf("    %s: %s failed: 0x%08lx\n", test, what, hr);
    return SUCCEEDED(hr);
}

bool result(const char* test, bool ok)
{
    std::printf("  %s: %s\n", test, ok ? "PASS" : "FAIL");
    return ok;
}

// Reads `tex` (a 2D texture, subresource 0) back through a staging copy as 32-bit texels.
bool read_back(ID3D11Device* dev, ID3D11DeviceContext* ctx, ID3D11Texture2D* tex, std::vector<uint32_t>* texels,
               UINT* width)
{
    D3D11_TEXTURE2D_DESC desc;
    tex->GetDesc(&desc);
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    desc.MiscFlags = 0;
    desc.SampleDesc = {1, 0};
    Ref<ID3D11Texture2D> staging;
    if (!check("readback", "CreateTexture2D(staging)", dev->CreateTexture2D(&desc, nullptr, staging.put())))
        return false;
    ctx->CopySubresourceRegion(staging, 0, 0, 0, 0, tex, 0, nullptr);
    D3D11_MAPPED_SUBRESOURCE m;
    if (!check("readback", "Map", ctx->Map(staging, 0, D3D11_MAP_READ, 0, &m)))
        return false;
    texels->resize(desc.Width * desc.Height);
    for (UINT y = 0; y < desc.Height; y++)
        std::memcpy(texels->data() + y * desc.Width, static_cast<uint8_t*>(m.pData) + y * m.RowPitch, desc.Width * 4);
    ctx->Unmap(staging, 0);
    *width = desc.Width;
    return true;
}

bool expect_texel(const char* test, const std::vector<uint32_t>& t, UINT width, UINT x, UINT y, uint32_t want)
{
    const uint32_t got = t[y * width + x];
    if (got != want)
        std::printf("    %s: texel (%u, %u) is 0x%08x, expected 0x%08x\n", test, x, y, got, want);
    return got == want;
}

Ref<ID3D11Texture2D>* make_target(ID3D11Device* dev, UINT size, UINT samples, Ref<ID3D11Texture2D>* tex,
                                  Ref<ID3D11RenderTargetView>* rtv)
{
    D3D11_TEXTURE2D_DESC d{size, size, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {samples, 0}, D3D11_USAGE_DEFAULT,
                           D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, 0};
    if (FAILED(dev->CreateTexture2D(&d, nullptr, tex->put())) ||
        FAILED(dev->CreateRenderTargetView(*tex, nullptr, rtv->put())))
        return nullptr;
    return tex;
}

void set_viewport(ID3D11DeviceContext* ctx, UINT size)
{
    D3D11_VIEWPORT vp{0, 0, float(size), float(size), 0, 1};
    ctx->RSSetViewports(1, &vp);
}

// --- Tests -----------------------------------------------------------------------------------------------------

// A clear, read back through a staging copy.
bool clear_test(ID3D11Device* dev, ID3D11DeviceContext* ctx)
{
    const char* t = "clear";
    Ref<ID3D11Texture2D> tex;
    Ref<ID3D11RenderTargetView> rtv;
    if (!make_target(dev, 64, 1, &tex, &rtv))
        return result(t, false);
    const float color[4] = {1.0f, 0.0f, 0.0f, 1.0f};
    ctx->ClearRenderTargetView(rtv, color);
    std::vector<uint32_t> texels;
    UINT w;
    bool ok = read_back(dev, ctx, tex, &texels, &w) && expect_texel(t, texels, w, 0, 0, 0xff0000ff) &&
              expect_texel(t, texels, w, 63, 63, 0xff0000ff);
    return result(t, ok);
}

const char* kTriangleHlsl = R"(
cbuffer Tint : register(b1) { float4 tint; };
struct V { float2 pos : POSITION; float4 color : COLOR; };
struct P { float4 pos : SV_Position; float4 color : COLOR; };
P vs(V v) { P p; p.pos = float4(v.pos, 0, 1); p.color = v.color; return p; }
float4 ps(P p) : SV_Target { return p.color * tint; }
)";

// A triangle from a vertex buffer through an input layout, a constant buffer at slot 1, the left half only.
bool triangle_test(ID3D11Device* dev, ID3D11DeviceContext* ctx)
{
    const char* t = "triangle";
    Ref<ID3DBlob> vsb, psb;
    if (!compile(kTriangleHlsl, "vs", "vs_4_0", &vsb) || !compile(kTriangleHlsl, "ps", "ps_4_0", &psb))
        return result(t, false);
    Ref<ID3D11VertexShader> vs;
    Ref<ID3D11PixelShader> ps;
    Ref<ID3D11InputLayout> layout;
    const D3D11_INPUT_ELEMENT_DESC elements[] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
        {"COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    // Covers x < 0 (the left half) entirely.
    const float verts[] = {-1, -3, 0, 1, 0, 1, -1, 3, 0, 1, 0, 1, 0, 0, 0, 1, 0, 1};
    const float tint[4] = {1, 1, 1, 1};
    Ref<ID3D11Buffer> vb, cb;
    D3D11_BUFFER_DESC vbd{sizeof(verts), D3D11_USAGE_IMMUTABLE, D3D11_BIND_VERTEX_BUFFER, 0, 0, 0};
    D3D11_SUBRESOURCE_DATA vbi{verts, 0, 0};
    D3D11_BUFFER_DESC cbd{16, D3D11_USAGE_DEFAULT, D3D11_BIND_CONSTANT_BUFFER, 0, 0, 0};
    D3D11_SUBRESOURCE_DATA cbi{tint, 0, 0};
    Ref<ID3D11Texture2D> tex;
    Ref<ID3D11RenderTargetView> rtv;
    if (!check(t, "CreateVertexShader", dev->CreateVertexShader(vsb->GetBufferPointer(), vsb->GetBufferSize(), nullptr, vs.put())) ||
        !check(t, "CreatePixelShader", dev->CreatePixelShader(psb->GetBufferPointer(), psb->GetBufferSize(), nullptr, ps.put())) ||
        !check(t, "CreateInputLayout", dev->CreateInputLayout(elements, 2, vsb->GetBufferPointer(), vsb->GetBufferSize(), layout.put())) ||
        !check(t, "CreateBuffer(vertices)", dev->CreateBuffer(&vbd, &vbi, vb.put())) ||
        !check(t, "CreateBuffer(constants)", dev->CreateBuffer(&cbd, &cbi, cb.put())) ||
        !make_target(dev, 64, 1, &tex, &rtv))
        return result(t, false);
    const float black[4] = {0, 0, 0, 1};
    ctx->ClearRenderTargetView(rtv, black);
    ID3D11RenderTargetView* rtvs[] = {rtv};
    ctx->OMSetRenderTargets(1, rtvs, nullptr);
    set_viewport(ctx, 64);
    UINT stride = 24, offset = 0;
    ID3D11Buffer* vbs[] = {vb};
    ctx->IASetVertexBuffers(0, 1, vbs, &stride, &offset);
    ctx->IASetInputLayout(layout);
    ctx->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ctx->VSSetShader(vs, nullptr, 0);
    ctx->PSSetShader(ps, nullptr, 0);
    ID3D11Buffer* cbs[] = {cb};
    ctx->PSSetConstantBuffers(1, 1, cbs);
    ctx->Draw(3, 0);
    std::vector<uint32_t> texels;
    UINT w;
    bool ok = read_back(dev, ctx, tex, &texels, &w) && expect_texel(t, texels, w, 8, 32, 0xff00ff00) &&
              expect_texel(t, texels, w, 56, 32, 0xff000000);
    ctx->ClearState();
    return result(t, ok);
}

const char* kTexturedHlsl = R"(
Texture2D tex : register(t2);
SamplerState smp : register(s1);
struct P { float4 pos : SV_Position; float2 uv : TEXCOORD; };
P vs(uint id : SV_VertexID) {
    P p; p.uv = float2((id << 1) & 2, id & 2); p.pos = float4(p.uv * float2(2, -2) + float2(-1, 1), 0, 1); return p;
}
float4 ps(P p) : SV_Target { return tex.Sample(smp, p.uv); }
)";

// A 2x2 texture with initial data sampled (point filtering) over the whole target: four quadrants.
bool textured_test(ID3D11Device* dev, ID3D11DeviceContext* ctx)
{
    const char* t = "textured";
    Ref<ID3DBlob> vsb, psb;
    if (!compile(kTexturedHlsl, "vs", "vs_4_0", &vsb) || !compile(kTexturedHlsl, "ps", "ps_4_0", &psb))
        return result(t, false);
    Ref<ID3D11VertexShader> vs;
    Ref<ID3D11PixelShader> ps;
    const uint32_t texels_in[4] = {0xff8040bf, 0xff80bf40, 0xff4080bf, 0xffbf8040};
    D3D11_TEXTURE2D_DESC td{2, 2, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0}, D3D11_USAGE_IMMUTABLE,
                            D3D11_BIND_SHADER_RESOURCE, 0, 0};
    D3D11_SUBRESOURCE_DATA ti{texels_in, 8, 0};
    Ref<ID3D11Texture2D> src, tex;
    Ref<ID3D11ShaderResourceView> srv;
    Ref<ID3D11SamplerState> smp;
    Ref<ID3D11RenderTargetView> rtv;
    D3D11_SAMPLER_DESC sd{D3D11_FILTER_MIN_MAG_MIP_POINT, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE_ADDRESS_CLAMP,
                          D3D11_TEXTURE_ADDRESS_CLAMP, 0, 1, D3D11_COMPARISON_NEVER, {}, 0, D3D11_FLOAT32_MAX};
    if (!check(t, "CreateVertexShader", dev->CreateVertexShader(vsb->GetBufferPointer(), vsb->GetBufferSize(), nullptr, vs.put())) ||
        !check(t, "CreatePixelShader", dev->CreatePixelShader(psb->GetBufferPointer(), psb->GetBufferSize(), nullptr, ps.put())) ||
        !check(t, "CreateTexture2D", dev->CreateTexture2D(&td, &ti, src.put())) ||
        !check(t, "CreateShaderResourceView", dev->CreateShaderResourceView(src, nullptr, srv.put())) ||
        !check(t, "CreateSamplerState", dev->CreateSamplerState(&sd, smp.put())) || !make_target(dev, 64, 1, &tex, &rtv))
        return result(t, false);
    ID3D11RenderTargetView* rtvs[] = {rtv};
    ctx->OMSetRenderTargets(1, rtvs, nullptr);
    set_viewport(ctx, 64);
    ctx->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ctx->VSSetShader(vs, nullptr, 0);
    ctx->PSSetShader(ps, nullptr, 0);
    ID3D11ShaderResourceView* srvs[] = {srv};
    ctx->PSSetShaderResources(2, 1, srvs);
    ID3D11SamplerState* smps[] = {smp};
    ctx->PSSetSamplers(1, 1, smps);
    ctx->Draw(3, 0);
    std::vector<uint32_t> texels;
    UINT w;
    bool ok = read_back(dev, ctx, tex, &texels, &w) && expect_texel(t, texels, w, 8, 8, texels_in[0]) &&
              expect_texel(t, texels, w, 56, 8, texels_in[1]) && expect_texel(t, texels, w, 8, 56, texels_in[2]) &&
              expect_texel(t, texels, w, 56, 56, texels_in[3]);
    ctx->ClearState();
    return result(t, ok);
}

const char* kDepthHlsl = R"(
cbuffer C : register(b0) { float depth; float3 pad; float4 color; };
float4 vs(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2); return float4(uv * float2(2, -2) + float2(-1, 1), depth, 1);
}
float4 ps() : SV_Target { return color; }
)";

// Two full-screen draws with a less-than depth test (the far one, drawn second, loses) into a 4x MSAA target with a
// depth buffer, resolved.
bool depth_test(ID3D11Device* dev, ID3D11DeviceContext* ctx)
{
    const char* t = "depth+msaa";
    Ref<ID3DBlob> vsb, psb;
    if (!compile(kDepthHlsl, "vs", "vs_4_0", &vsb) || !compile(kDepthHlsl, "ps", "ps_4_0", &psb))
        return result(t, false);
    Ref<ID3D11VertexShader> vs;
    Ref<ID3D11PixelShader> ps;
    Ref<ID3D11Texture2D> ms, resolved, depth;
    Ref<ID3D11RenderTargetView> rtv, unused;
    Ref<ID3D11DepthStencilView> dsv;
    Ref<ID3D11DepthStencilState> dss;
    Ref<ID3D11Buffer> cb;
    UINT levels = 0;
    dev->CheckMultisampleQualityLevels(DXGI_FORMAT_R8G8B8A8_UNORM, 4, &levels);
    D3D11_TEXTURE2D_DESC dd{64, 64, 1, 1, DXGI_FORMAT_D32_FLOAT, {4, 0}, D3D11_USAGE_DEFAULT,
                            D3D11_BIND_DEPTH_STENCIL, 0, 0};
    D3D11_DEPTH_STENCIL_DESC dsd{TRUE, D3D11_DEPTH_WRITE_MASK_ALL, D3D11_COMPARISON_LESS, FALSE, 0xff, 0xff, {}, {}};
    D3D11_BUFFER_DESC cbd{32, D3D11_USAGE_DYNAMIC, D3D11_BIND_CONSTANT_BUFFER, D3D11_CPU_ACCESS_WRITE, 0, 0};
    if (levels == 0 || !make_target(dev, 64, 4, &ms, &rtv) || !make_target(dev, 64, 1, &resolved, &unused) ||
        !check(t, "CreateVertexShader", dev->CreateVertexShader(vsb->GetBufferPointer(), vsb->GetBufferSize(), nullptr, vs.put())) ||
        !check(t, "CreatePixelShader", dev->CreatePixelShader(psb->GetBufferPointer(), psb->GetBufferSize(), nullptr, ps.put())) ||
        !check(t, "CreateTexture2D(depth)", dev->CreateTexture2D(&dd, nullptr, depth.put())) ||
        !check(t, "CreateDepthStencilView", dev->CreateDepthStencilView(depth, nullptr, dsv.put())) ||
        !check(t, "CreateDepthStencilState", dev->CreateDepthStencilState(&dsd, dss.put())) ||
        !check(t, "CreateBuffer", dev->CreateBuffer(&cbd, nullptr, cb.put()))) {
        std::printf("    4x MSAA quality levels: %u\n", levels);
        return result(t, false);
    }
    const float black[4] = {0, 0, 0, 1};
    ctx->ClearRenderTargetView(rtv, black);
    ctx->ClearDepthStencilView(dsv, D3D11_CLEAR_DEPTH, 1.0f, 0);
    ID3D11RenderTargetView* rtvs[] = {rtv};
    ctx->OMSetRenderTargets(1, rtvs, dsv);
    ctx->OMSetDepthStencilState(dss, 0);
    set_viewport(ctx, 64);
    ctx->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ctx->VSSetShader(vs, nullptr, 0);
    ctx->PSSetShader(ps, nullptr, 0);
    ID3D11Buffer* cbs[] = {cb};
    ctx->VSSetConstantBuffers(0, 1, cbs);
    ctx->PSSetConstantBuffers(0, 1, cbs);
    const float draws[2][8] = {{0.25f, 0, 0, 0, 0, 0, 1, 1}, {0.75f, 0, 0, 0, 1, 0, 0, 1}};   // near blue, far red
    for (const auto& c : draws) {
        D3D11_MAPPED_SUBRESOURCE m;
        if (!check(t, "Map(WRITE_DISCARD)", ctx->Map(cb, 0, D3D11_MAP_WRITE_DISCARD, 0, &m)))
            return result(t, false);
        std::memcpy(m.pData, c, sizeof(c));
        ctx->Unmap(cb, 0);
        ctx->Draw(3, 0);
    }
    ctx->ResolveSubresource(resolved, 0, ms, 0, DXGI_FORMAT_R8G8B8A8_UNORM);
    std::vector<uint32_t> texels;
    UINT w;
    bool ok = read_back(dev, ctx, resolved, &texels, &w) && expect_texel(t, texels, w, 32, 32, 0xffff0000);
    ctx->ClearState();
    return result(t, ok);
}

const char* kComputeHlsl = R"(
RWBuffer<uint> outv : register(u1);
[numthreads(64, 1, 1)] void cs(uint3 id : SV_DispatchThreadID) { outv[id.x] = id.x * 3 + 1; }
)";

// A typed UAV buffer cleared, then written by a dispatch of 4 groups of 64.
bool compute_test(ID3D11Device* dev, ID3D11DeviceContext* ctx)
{
    const char* t = "compute";
    Ref<ID3DBlob> csb;
    if (!compile(kComputeHlsl, "cs", "cs_5_0", &csb))
        return result(t, false);
    constexpr UINT n = 256;
    Ref<ID3D11ComputeShader> cs;
    Ref<ID3D11Buffer> buf, staging;
    Ref<ID3D11UnorderedAccessView> uav;
    D3D11_BUFFER_DESC bd{n * 4, D3D11_USAGE_DEFAULT, D3D11_BIND_UNORDERED_ACCESS, 0, 0, 0};
    D3D11_BUFFER_DESC sd{n * 4, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, 0, 0};
    D3D11_UNORDERED_ACCESS_VIEW_DESC ud{};
    ud.Format = DXGI_FORMAT_R32_UINT;
    ud.ViewDimension = D3D11_UAV_DIMENSION_BUFFER;
    ud.Buffer = {0, n, 0};
    if (!check(t, "CreateComputeShader", dev->CreateComputeShader(csb->GetBufferPointer(), csb->GetBufferSize(), nullptr, cs.put())) ||
        !check(t, "CreateBuffer", dev->CreateBuffer(&bd, nullptr, buf.put())) ||
        !check(t, "CreateBuffer(staging)", dev->CreateBuffer(&sd, nullptr, staging.put())) ||
        !check(t, "CreateUnorderedAccessView", dev->CreateUnorderedAccessView(buf, &ud, uav.put())))
        return result(t, false);
    const UINT zero[4] = {7, 7, 7, 7};
    ctx->ClearUnorderedAccessViewUint(uav, zero);
    ctx->CSSetShader(cs, nullptr, 0);
    ID3D11UnorderedAccessView* uavs[] = {uav};
    ctx->CSSetUnorderedAccessViews(1, 1, uavs, nullptr);
    ctx->Dispatch(n / 64, 1, 1);
    ctx->CopyResource(staging, buf);
    D3D11_MAPPED_SUBRESOURCE m;
    bool ok = check(t, "Map", ctx->Map(staging, 0, D3D11_MAP_READ, 0, &m));
    if (ok) {
        const auto* v = static_cast<const uint32_t*>(m.pData);
        for (UINT i = 0; i < n && ok; i++)
            if (v[i] != i * 3 + 1) {
                std::printf("    %s: value %u is %u, expected %u\n", t, i, v[i], i * 3 + 1);
                ok = false;
            }
        ctx->Unmap(staging, 0);
    }
    ctx->ClearState();
    return result(t, ok);
}

const char* kStreamOutputHlsl = R"(
struct O { float4 pos : SV_Position; float4 skinned : TEXCOORD0; uint2 tags : TEXCOORD1; };
O vs(uint id : SV_VertexID) {
    O o;
    o.pos = float4(0, 0, 0, 1);
    o.skinned = float4(id * 2 + 1, id, -0.5, 1);
    o.tags = uint2(id + 5, 0xfffffff0u + id);
    return o;
}
)";

// Stream output of a vertex shader with no geometry shader, as games skin meshes: 4 points written to a buffer with
// rasterization off. The integers (small ones are float denormals) must arrive bit for bit.
bool stream_output_test(ID3D11Device* dev, ID3D11DeviceContext* ctx)
{
    const char* t = "stream-output";
    Ref<ID3DBlob> vsb;
    if (!compile(kStreamOutputHlsl, "vs", "vs_5_0", &vsb))
        return result(t, false);
    constexpr UINT n = 4, stride = 24;
    const D3D11_SO_DECLARATION_ENTRY decl[] = {{0, "TEXCOORD", 0, 0, 4, 0}, {0, "TEXCOORD", 1, 0, 2, 0}};
    const UINT strides[] = {stride};
    Ref<ID3D11VertexShader> vs;
    Ref<ID3D11GeometryShader> gs;
    Ref<ID3D11Buffer> buf, staging;
    D3D11_BUFFER_DESC bd{n * stride, D3D11_USAGE_DEFAULT, D3D11_BIND_STREAM_OUTPUT, 0, 0, 0};
    D3D11_BUFFER_DESC sd{n * stride, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, 0, 0};
    if (!check(t, "CreateVertexShader", dev->CreateVertexShader(vsb->GetBufferPointer(), vsb->GetBufferSize(), nullptr, vs.put())) ||
        !check(t, "CreateGeometryShaderWithStreamOutput",
               dev->CreateGeometryShaderWithStreamOutput(vsb->GetBufferPointer(), vsb->GetBufferSize(), decl, 2, strides,
                                                         1, D3D11_SO_NO_RASTERIZED_STREAM, nullptr, gs.put())) ||
        !check(t, "CreateBuffer", dev->CreateBuffer(&bd, nullptr, buf.put())) ||
        !check(t, "CreateBuffer(staging)", dev->CreateBuffer(&sd, nullptr, staging.put())))
        return result(t, false);
    ctx->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_POINTLIST);
    ctx->VSSetShader(vs, nullptr, 0);
    ctx->GSSetShader(gs, nullptr, 0);
    ID3D11Buffer* targets[] = {buf};
    const UINT offsets[] = {0};
    ctx->SOSetTargets(1, targets, offsets);
    ctx->Draw(n, 0);
    ctx->SOSetTargets(0, nullptr, nullptr);
    ctx->CopyResource(staging, buf);
    D3D11_MAPPED_SUBRESOURCE m;
    bool ok = check(t, "Map", ctx->Map(staging, 0, D3D11_MAP_READ, 0, &m));
    if (ok) {
        const auto* v = static_cast<const uint32_t*>(m.pData);
        for (UINT i = 0; i < n && ok; i++) {
            const float f[4] = {float(i * 2 + 1), float(i), -0.5f, 1.0f};
            uint32_t want[6] = {0, 0, 0, 0, i + 5, 0xfffffff0u + i};
            std::memcpy(want, f, sizeof(f));
            for (UINT c = 0; c < 6 && ok; c++)
                if (v[i * 6 + c] != want[c]) {
                    std::printf("    %s: vertex %u word %u is 0x%08x, expected 0x%08x\n", t, i, c, v[i * 6 + c],
                                want[c]);
                    ok = false;
                }
        }
        ctx->Unmap(staging, 0);
    }
    ctx->ClearState();
    return result(t, ok);
}

// An event, an occlusion query around a full-screen draw of 64x64 pixels, and timestamps.
bool queries_test(ID3D11Device* dev, ID3D11DeviceContext* ctx)
{
    const char* t = "queries";
    Ref<ID3DBlob> vsb, psb;
    if (!compile(kTexturedHlsl, "vs", "vs_4_0", &vsb) || !compile(kDepthHlsl, "ps", "ps_4_0", &psb))
        return result(t, false);
    Ref<ID3D11VertexShader> vs;
    Ref<ID3D11PixelShader> ps;
    Ref<ID3D11Texture2D> tex;
    Ref<ID3D11RenderTargetView> rtv;
    Ref<ID3D11Query> event, occlusion, disjoint, ts0, ts1;
    D3D11_QUERY_DESC qd[] = {{D3D11_QUERY_EVENT, 0}, {D3D11_QUERY_OCCLUSION, 0}, {D3D11_QUERY_TIMESTAMP_DISJOINT, 0},
                             {D3D11_QUERY_TIMESTAMP, 0}};
    if (!check(t, "CreateVertexShader", dev->CreateVertexShader(vsb->GetBufferPointer(), vsb->GetBufferSize(), nullptr, vs.put())) ||
        !check(t, "CreatePixelShader", dev->CreatePixelShader(psb->GetBufferPointer(), psb->GetBufferSize(), nullptr, ps.put())) ||
        !make_target(dev, 64, 1, &tex, &rtv) || !check(t, "CreateQuery(event)", dev->CreateQuery(&qd[0], event.put())) ||
        !check(t, "CreateQuery(occlusion)", dev->CreateQuery(&qd[1], occlusion.put())) ||
        !check(t, "CreateQuery(disjoint)", dev->CreateQuery(&qd[2], disjoint.put())) ||
        !check(t, "CreateQuery(timestamp)", dev->CreateQuery(&qd[3], ts0.put())) ||
        !check(t, "CreateQuery(timestamp)", dev->CreateQuery(&qd[3], ts1.put())))
        return result(t, false);
    ID3D11RenderTargetView* rtvs[] = {rtv};
    ctx->OMSetRenderTargets(1, rtvs, nullptr);
    set_viewport(ctx, 64);
    ctx->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ctx->VSSetShader(vs, nullptr, 0);
    ctx->PSSetShader(ps, nullptr, 0);
    ctx->Begin(disjoint);
    ctx->End(ts0);
    ctx->Begin(occlusion);
    ctx->Draw(3, 0);
    ctx->End(occlusion);
    ctx->End(ts1);
    ctx->End(disjoint);
    ctx->End(event);
    auto wait = [&](ID3D11Query* q, void* data, UINT size) {
        for (int i = 0; i < 5000; i++) {
            HRESULT hr = ctx->GetData(q, data, size, 0);
            if (hr == S_OK)
                return true;
            if (FAILED(hr)) {
                std::printf("    %s: GetData failed: 0x%08lx\n", t, hr);
                return false;
            }
            Sleep(1);
        }
        std::printf("    %s: GetData timed out\n", t);
        return false;
    };
    BOOL done = FALSE;
    UINT64 samples = 0, t0 = 0, t1 = 0;
    D3D11_QUERY_DATA_TIMESTAMP_DISJOINT dj{};
    bool ok = wait(event, &done, sizeof(done)) && done && wait(occlusion, &samples, sizeof(samples)) &&
              wait(disjoint, &dj, sizeof(dj)) && wait(ts0, &t0, sizeof(t0)) && wait(ts1, &t1, sizeof(t1));
    std::printf("    %s: %llu samples passed, timestamps %llu..%llu at %llu Hz%s\n", t,
                static_cast<unsigned long long>(samples), static_cast<unsigned long long>(t0),
                static_cast<unsigned long long>(t1), static_cast<unsigned long long>(dj.Frequency),
                dj.Disjoint ? " (disjoint)" : "");
    ok = ok && samples == 64 * 64 && t1 >= t0 && dj.Frequency;
    ctx->ClearState();
    return result(t, ok);
}

// Dynamic buffer mapping with discard and no-overwrite, and UpdateSubresource into part of a texture.
bool update_test(ID3D11Device* dev, ID3D11DeviceContext* ctx)
{
    const char* t = "map+update";
    Ref<ID3D11Buffer> dyn, staging;
    D3D11_BUFFER_DESC bd{256, D3D11_USAGE_DYNAMIC, D3D11_BIND_VERTEX_BUFFER, D3D11_CPU_ACCESS_WRITE, 0, 0};
    D3D11_BUFFER_DESC sd{256, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, 0, 0};
    if (!check(t, "CreateBuffer(dynamic)", dev->CreateBuffer(&bd, nullptr, dyn.put())) ||
        !check(t, "CreateBuffer(staging)", dev->CreateBuffer(&sd, nullptr, staging.put())))
        return result(t, false);
    D3D11_MAPPED_SUBRESOURCE m;
    bool ok = check(t, "Map(WRITE_DISCARD)", ctx->Map(dyn, 0, D3D11_MAP_WRITE_DISCARD, 0, &m));
    if (ok) {
        std::memset(m.pData, 0x11, 256);
        ctx->Unmap(dyn, 0);
    }
    ok = ok && check(t, "Map(WRITE_NO_OVERWRITE)", ctx->Map(dyn, 0, D3D11_MAP_WRITE_NO_OVERWRITE, 0, &m));
    if (ok) {
        std::memset(static_cast<uint8_t*>(m.pData) + 128, 0x22, 128);
        ctx->Unmap(dyn, 0);
    }
    ctx->CopyResource(staging, dyn);
    ok = ok && check(t, "Map(READ)", ctx->Map(staging, 0, D3D11_MAP_READ, 0, &m));
    if (ok) {
        const auto* b = static_cast<const uint8_t*>(m.pData);
        if (b[0] != 0x11 || b[127] != 0x11 || b[128] != 0x22 || b[255] != 0x22) {
            std::printf("    %s: buffer bytes %02x %02x %02x %02x\n", t, b[0], b[127], b[128], b[255]);
            ok = false;
        }
        ctx->Unmap(staging, 0);
    }
    Ref<ID3D11Texture2D> tex;
    Ref<ID3D11RenderTargetView> rtv;
    ok = ok && make_target(dev, 64, 1, &tex, &rtv);
    if (ok) {
        const float black[4] = {0, 0, 0, 0};
        ctx->ClearRenderTargetView(rtv, black);
        std::vector<uint32_t> block(16 * 16, 0xff123456);
        const D3D11_BOX box{16, 16, 0, 32, 32, 1};
        ctx->UpdateSubresource(tex, 0, &box, block.data(), 16 * 4, 0);
        std::vector<uint32_t> texels;
        UINT w;
        ok = read_back(dev, ctx, tex, &texels, &w) && expect_texel(t, texels, w, 20, 20, 0xff123456) &&
             expect_texel(t, texels, w, 40, 40, 0);
    }
    return result(t, ok);
}

// --- D3D11.3 (the WDDM 2.0 DDI) ------------------------------------------------------------------------------

// The D3D11.3 options as the runtime reports them from the driver's WDDM 2.0 caps, and what the level promises.
bool features_test(ID3D11Device* dev, ID3D11DeviceContext*)
{
    const char* t = "d3d11.3 features";
    D3D11_FEATURE_DATA_D3D11_OPTIONS2 o2{};
    D3D11_FEATURE_DATA_D3D11_OPTIONS3 o3{};
    D3D11_FEATURE_DATA_FORMAT_SUPPORT2 rg32f{DXGI_FORMAT_R32G32_FLOAT, 0};
    if (!check(t, "CheckFeatureSupport(OPTIONS2)", dev->CheckFeatureSupport(D3D11_FEATURE_D3D11_OPTIONS2, &o2, sizeof(o2))) ||
        !check(t, "CheckFeatureSupport(OPTIONS3)", dev->CheckFeatureSupport(D3D11_FEATURE_D3D11_OPTIONS3, &o3, sizeof(o3))) ||
        !check(t, "CheckFeatureSupport(FORMAT_SUPPORT2)",
               dev->CheckFeatureSupport(D3D11_FEATURE_FORMAT_SUPPORT2, &rg32f, sizeof(rg32f))))
        return result(t, false);
    const D3D_FEATURE_LEVEL level = dev->GetFeatureLevel();
    std::printf("    feature level 0x%x: typed UAV loads %d, ROVs %d, stencil ref %d, conservative rasterization tier %d, "
                "tiled resources tier %d, VP/RT index from any shader %d; R32G32_FLOAT support2 0x%x\n",
                level, o2.TypedUAVLoadAdditionalFormats, o2.ROVsSupported, o2.PSSpecifiedStencilRefSupported,
                o2.ConservativeRasterizationTier, o2.TiledResourcesTier,
                o3.VPAndRTArrayIndexFromAnyShaderFeedingRasterizer, rg32f.OutFormatSupport2);
    // GravityMark's need: typed UAV loads, of RG32F among others.
    bool ok = o2.TypedUAVLoadAdditionalFormats && (rg32f.OutFormatSupport2 & D3D11_FORMAT_SUPPORT2_UAV_TYPED_LOAD);
    if (level < D3D_FEATURE_LEVEL_12_0)
        std::printf("    %s: below 12_0 (DXVK's level on this GPU, or Direct2D in the process)\n", t);
    // 12_0 promises tiled resources tier 2 and typed UAV loads; 12_1 adds ROVs and conservative rasterization.
    if (level >= D3D_FEATURE_LEVEL_12_0 && o2.TiledResourcesTier < D3D11_TILED_RESOURCES_TIER_2) {
        std::printf("    %s: 12_0 without tiled resources tier 2\n", t);
        ok = false;
    }
    if (level >= D3D_FEATURE_LEVEL_12_1 && (!o2.ROVsSupported || !o2.ConservativeRasterizationTier)) {
        std::printf("    %s: 12_1 without ROVs or conservative rasterization\n", t);
        ok = false;
    }
    return result(t, ok);
}

// GravityMark's occluder pass, which the runtime rejects without typed UAV loads: a compute shader reading and
// writing an R32G32_FLOAT texture through one UAV.
const char* kTypedLoadHlsl = R"(
RWTexture2D<float2> tex : register(u0);
[numthreads(8, 8, 1)] void cs(uint3 id : SV_DispatchThreadID) { tex[id.xy] = tex[id.xy] * 2 + float2(1, id.x); }
)";

bool typed_uav_load_test(ID3D11Device* dev, ID3D11DeviceContext* ctx)
{
    const char* t = "typed uav load";
    constexpr UINT n = 16;
    std::vector<float> in(n * n * 2);
    for (UINT i = 0; i < n * n; i++) {
        in[i * 2] = float(i);
        in[i * 2 + 1] = 0.5f;
    }
    Ref<ID3DBlob> csb;
    Ref<ID3D11ComputeShader> cs;
    Ref<ID3D11Texture2D> tex, staging;
    Ref<ID3D11UnorderedAccessView> uav;
    D3D11_TEXTURE2D_DESC td{n, n, 1, 1, DXGI_FORMAT_R32G32_FLOAT, {1, 0}, D3D11_USAGE_DEFAULT,
                            D3D11_BIND_UNORDERED_ACCESS, 0, 0};
    D3D11_SUBRESOURCE_DATA ti{in.data(), n * 8, 0};
    D3D11_TEXTURE2D_DESC sd = td;
    sd.Usage = D3D11_USAGE_STAGING;
    sd.BindFlags = 0;
    sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    if (!compile(kTypedLoadHlsl, "cs", "cs_5_0", &csb) ||
        !check(t, "CreateComputeShader", dev->CreateComputeShader(csb->GetBufferPointer(), csb->GetBufferSize(), nullptr, cs.put())) ||
        !check(t, "CreateTexture2D", dev->CreateTexture2D(&td, &ti, tex.put())) ||
        !check(t, "CreateTexture2D(staging)", dev->CreateTexture2D(&sd, nullptr, staging.put())) ||
        !check(t, "CreateUnorderedAccessView", dev->CreateUnorderedAccessView(tex, nullptr, uav.put())))
        return result(t, false);
    ctx->CSSetShader(cs, nullptr, 0);
    ID3D11UnorderedAccessView* uavs[] = {uav};
    ctx->CSSetUnorderedAccessViews(0, 1, uavs, nullptr);
    ctx->Dispatch(n / 8, n / 8, 1);
    ctx->CopyResource(staging, tex);
    D3D11_MAPPED_SUBRESOURCE m{};
    bool ok = check(t, "Map", ctx->Map(staging, 0, D3D11_MAP_READ, 0, &m));
    for (UINT y = 0; ok && y < n; y++)
        for (UINT x = 0; ok && x < n; x++) {
            const float* v = reinterpret_cast<const float*>(static_cast<const uint8_t*>(m.pData) + y * m.RowPitch) + x * 2;
            const float want0 = float(y * n + x) * 2 + 1, want1 = 0.5f * 2 + float(x);
            if (v[0] != want0 || v[1] != want1) {
                std::printf("    %s: texel (%u, %u) is (%g, %g), expected (%g, %g)\n", t, x, y, v[0], v[1], want0,
                            want1);
                ok = false;
            }
        }
    if (m.pData)
        ctx->Unmap(staging, 0);
    ctx->ClearState();
    return result(t, ok);
}

// A 256x256 RGBA8 tiled texture (2x2 tiles of 128x128) on a tile pool: mapped, cleared, one tile written with
// UpdateTiles and read with CopyTiles, the packed mip count asked for, a tile unmapped (tier 2: it reads as zero),
// and the pool grown.
bool tiled_test(ID3D11Device* dev, ID3D11DeviceContext* ctx)
{
    const char* t = "tiled resources";
    D3D11_FEATURE_DATA_D3D11_OPTIONS2 o2{};
    dev->CheckFeatureSupport(D3D11_FEATURE_D3D11_OPTIONS2, &o2, sizeof(o2));
    if (!o2.TiledResourcesTier) {
        std::printf("    %s: no tiled resources at feature level 0x%x\n", t, dev->GetFeatureLevel());
        return result(t, dev->GetFeatureLevel() < D3D_FEATURE_LEVEL_12_0);
    }
    Ref<ID3D11Device2> dev2;
    Ref<ID3D11DeviceContext2> ctx2;
    constexpr UINT tile = 64 * 1024, size = 256;
    Ref<ID3D11Buffer> pool, linear, staging;
    Ref<ID3D11Texture2D> tex;
    Ref<ID3D11RenderTargetView> rtv;
    D3D11_BUFFER_DESC pd{4 * tile, D3D11_USAGE_DEFAULT, 0, 0, D3D11_RESOURCE_MISC_TILE_POOL, 0};
    D3D11_BUFFER_DESC ld{tile, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0, 0, 0};
    D3D11_BUFFER_DESC sd{tile, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, 0, 0};
    D3D11_TEXTURE2D_DESC td{size, size, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0}, D3D11_USAGE_DEFAULT,
                            D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, D3D11_RESOURCE_MISC_TILED};
    if (!check(t, "QueryInterface(ID3D11Device2)", dev->QueryInterface(IID_PPV_ARGS(dev2.put()))) ||
        !check(t, "QueryInterface(ID3D11DeviceContext2)", ctx->QueryInterface(IID_PPV_ARGS(ctx2.put()))) ||
        !check(t, "CreateBuffer(tile pool)", dev->CreateBuffer(&pd, nullptr, pool.put())) ||
        !check(t, "CreateBuffer(linear)", dev->CreateBuffer(&ld, nullptr, linear.put())) ||
        !check(t, "CreateBuffer(staging)", dev->CreateBuffer(&sd, nullptr, staging.put())) ||
        !check(t, "CreateTexture2D(tiled)", dev->CreateTexture2D(&td, nullptr, tex.put())) ||
        !check(t, "CreateRenderTargetView", dev->CreateRenderTargetView(tex, nullptr, rtv.put())))
        return result(t, false);
    UINT tiles = 0, subresources = 1;
    D3D11_PACKED_MIP_DESC packed{};
    D3D11_TILE_SHAPE shape{};
    D3D11_SUBRESOURCE_TILING tiling{};
    dev2->GetResourceTiling(tex, &tiles, &packed, &shape, &subresources, 0, &tiling);
    std::printf("    %s: %u tiles of %ux%u, %u packed mips in %u tiles\n", t, tiles, shape.WidthInTexels,
                shape.HeightInTexels, packed.NumPackedMips, packed.NumTilesForPackedMips);
    bool ok = tiles == 4 && shape.WidthInTexels == 128 && shape.HeightInTexels == 128 && !packed.NumPackedMips;

    const D3D11_TILED_RESOURCE_COORDINATE origin{0, 0, 0, 0}, right{1, 0, 0, 0}, below{0, 1, 0, 0};
    const D3D11_TILE_REGION_SIZE all{4, FALSE, 0, 0, 0}, one{1, FALSE, 0, 0, 0};
    const UINT start = 0, count = 4;
    ok = ok && check(t, "UpdateTileMappings", ctx2->UpdateTileMappings(tex, 1, &origin, &all, pool, 1, nullptr, &start,
                                                                         &count, 0));
    const float red[4] = {1, 0, 0, 1};
    ctx->ClearRenderTargetView(rtv, red);
    const std::vector<uint32_t> green(tile / 4, 0xff00ff00);
    ctx2->UpdateTiles(tex, &right, &one, green.data(), 0);
    ctx2->TiledResourceBarrier(nullptr, tex);
    std::vector<uint32_t> texels;
    UINT w;
    ok = ok && read_back(dev, ctx, tex, &texels, &w) && expect_texel(t, texels, w, 10, 10, 0xff0000ff) &&
         expect_texel(t, texels, w, 200, 10, 0xff00ff00) && expect_texel(t, texels, w, 10, 200, 0xff0000ff) &&
         expect_texel(t, texels, w, 200, 200, 0xff0000ff);

    // The written tile back into a buffer, texels in rows.
    ctx2->CopyTiles(tex, &right, &one, linear, 0, D3D11_TILE_COPY_SWIZZLED_TILED_RESOURCE_TO_LINEAR_BUFFER);
    ctx->CopyResource(staging, linear);
    D3D11_MAPPED_SUBRESOURCE m{};
    if (ok && check(t, "Map", ctx->Map(staging, 0, D3D11_MAP_READ, 0, &m))) {
        const auto* v = static_cast<const uint32_t*>(m.pData);
        for (UINT i = 0; i < tile / 4 && ok; i++)
            if (v[i] != 0xff00ff00) {
                std::printf("    %s: CopyTiles texel %u is 0x%08x, expected 0xff00ff00\n", t, i, v[i]);
                ok = false;
            }
        ctx->Unmap(staging, 0);
    }

    if (o2.TiledResourcesTier >= D3D11_TILED_RESOURCES_TIER_2) {
        const UINT null_range = D3D11_TILE_RANGE_NULL;
        ok = ok && check(t, "UpdateTileMappings(null)", ctx2->UpdateTileMappings(tex, 1, &below, &one, nullptr, 1,
                                                                                   &null_range, nullptr, nullptr, 0));
        ok = ok && read_back(dev, ctx, tex, &texels, &w) && expect_texel(t, texels, w, 10, 200, 0) &&
             expect_texel(t, texels, w, 200, 10, 0xff00ff00);
    }
    ok = ok && check(t, "ResizeTilePool", ctx2->ResizeTilePool(pool, 8 * tile));
    ctx->ClearState();
    return result(t, ok);
}

const char* kPointHlsl = R"(
float4 vs(float2 pos : POSITION) : SV_Position { return float4(pos, 0, 1); }
float4 ps() : SV_Target { return 1; }
)";

// A triangle inside pixel (2, 2) of an 8x8 target that misses the pixel's centre: only conservative rasterization
// (ID3D11RasterizerState2, the WDDM 2.0 rasterizer DDI) draws it.
bool conservative_test(ID3D11Device* dev, ID3D11DeviceContext* ctx)
{
    const char* t = "conservative rasterization";
    D3D11_FEATURE_DATA_D3D11_OPTIONS2 o2{};
    dev->CheckFeatureSupport(D3D11_FEATURE_D3D11_OPTIONS2, &o2, sizeof(o2));
    if (!o2.ConservativeRasterizationTier) {
        std::printf("    %s: not supported at feature level 0x%x\n", t, dev->GetFeatureLevel());
        return result(t, dev->GetFeatureLevel() < D3D_FEATURE_LEVEL_12_1);
    }
    Ref<ID3D11Device3> dev3;
    Ref<ID3DBlob> vsb, psb;
    Ref<ID3D11VertexShader> vs;
    Ref<ID3D11PixelShader> ps;
    Ref<ID3D11InputLayout> layout;
    Ref<ID3D11Buffer> vb;
    Ref<ID3D11Texture2D> tex;
    Ref<ID3D11RenderTargetView> rtv;
    Ref<ID3D11RasterizerState2> plain, conservative;
    // Pixel (x, y) spans [x, x + 1]: NDC x = px / 4 - 1, y = 1 - py / 4 on 8 pixels.
    auto ndc = [](float px, float py) { return std::pair<float, float>{px / 4 - 1, 1 - py / 4}; };
    const auto a = ndc(2.1f, 2.1f), b = ndc(2.4f, 2.1f), c = ndc(2.1f, 2.4f);
    const float verts[] = {a.first, a.second, b.first, b.second, c.first, c.second};
    const D3D11_INPUT_ELEMENT_DESC element{"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0};
    D3D11_BUFFER_DESC vbd{sizeof(verts), D3D11_USAGE_IMMUTABLE, D3D11_BIND_VERTEX_BUFFER, 0, 0, 0};
    D3D11_SUBRESOURCE_DATA vbi{verts, 0, 0};
    D3D11_RASTERIZER_DESC2 rd{D3D11_FILL_SOLID, D3D11_CULL_NONE, FALSE, 0, 0, 0, TRUE, FALSE, FALSE, FALSE, 0,
                              D3D11_CONSERVATIVE_RASTERIZATION_MODE_OFF};
    D3D11_RASTERIZER_DESC2 cd = rd;
    cd.ConservativeRaster = D3D11_CONSERVATIVE_RASTERIZATION_MODE_ON;
    if (!compile(kPointHlsl, "vs", "vs_5_0", &vsb) || !compile(kPointHlsl, "ps", "ps_5_0", &psb) ||
        !check(t, "QueryInterface(ID3D11Device3)", dev->QueryInterface(IID_PPV_ARGS(dev3.put()))) ||
        !check(t, "CreateVertexShader", dev->CreateVertexShader(vsb->GetBufferPointer(), vsb->GetBufferSize(), nullptr, vs.put())) ||
        !check(t, "CreatePixelShader", dev->CreatePixelShader(psb->GetBufferPointer(), psb->GetBufferSize(), nullptr, ps.put())) ||
        !check(t, "CreateInputLayout", dev->CreateInputLayout(&element, 1, vsb->GetBufferPointer(), vsb->GetBufferSize(), layout.put())) ||
        !check(t, "CreateBuffer", dev->CreateBuffer(&vbd, &vbi, vb.put())) ||
        !check(t, "CreateRasterizerState2", dev3->CreateRasterizerState2(&rd, plain.put())) ||
        !check(t, "CreateRasterizerState2(conservative)", dev3->CreateRasterizerState2(&cd, conservative.put())) ||
        !make_target(dev, 8, 1, &tex, &rtv))
        return result(t, false);
    ID3D11RenderTargetView* rtvs[] = {rtv};
    UINT stride = 8, offset = 0;
    ID3D11Buffer* vbs[] = {vb};
    const float black[4] = {0, 0, 0, 0};
    bool ok = true;
    for (ID3D11RasterizerState2* state : {plain.p, conservative.p}) {
        ctx->ClearRenderTargetView(rtv, black);
        ctx->OMSetRenderTargets(1, rtvs, nullptr);
        set_viewport(ctx, 8);
        ctx->IASetVertexBuffers(0, 1, vbs, &stride, &offset);
        ctx->IASetInputLayout(layout);
        ctx->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ctx->VSSetShader(vs, nullptr, 0);
        ctx->PSSetShader(ps, nullptr, 0);
        ctx->RSSetState(state);
        ctx->Draw(3, 0);
        std::vector<uint32_t> texels;
        UINT w;
        const uint32_t want = state == conservative.p ? 0xffffffff : 0;
        ok = ok && read_back(dev, ctx, tex, &texels, &w) && expect_texel(t, texels, w, 2, 2, want) &&
             expect_texel(t, texels, w, 5, 5, 0);
    }
    ctx->ClearState();
    return result(t, ok);
}

// IDXGIDevice3::Trim (a WDDM 2.0 flush with the trim flag), then rendering goes on.
bool trim_test(ID3D11Device* dev, ID3D11DeviceContext* ctx)
{
    const char* t = "trim";
    Ref<IDXGIDevice3> dxgi;
    if (!check(t, "QueryInterface(IDXGIDevice3)", dev->QueryInterface(IID_PPV_ARGS(dxgi.put()))))
        return result(t, false);
    ctx->ClearState();
    dxgi->Trim();
    return result(t, clear_test(dev, ctx));
}

bool shared_child(ID3D11Device* dev, ID3D11DeviceContext* ctx, HANDLE handle)
{
    Ref<ID3D11Texture2D> opened;
    if (!check("shared child", "OpenSharedResource", dev->OpenSharedResource(handle, IID_PPV_ARGS(opened.put()))))
        return false;
    std::vector<uint32_t> texels;
    UINT width;
    return read_back(dev, ctx, opened, &texels, &width) &&
           expect_texel("shared child", texels, width, 0, 0, 0xff00ffff) &&
           expect_texel("shared child", texels, width, 95, 63, 0xff00ffff);
}

bool run_shared_child(HANDLE handle)
{
    wchar_t path[MAX_PATH], command[MAX_PATH + 80];
    const DWORD length = GetModuleFileNameW(nullptr, path, MAX_PATH);
    if (!length || length >= MAX_PATH)
        return false;
    std::swprintf(command, std::size(command), L"\"%ls\" /shared-child %llu", path,
                  static_cast<unsigned long long>(reinterpret_cast<uintptr_t>(handle)));
    STARTUPINFOW startup{};
    startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
    startup.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
    startup.hStdError = GetStdHandle(STD_ERROR_HANDLE);
    PROCESS_INFORMATION process{};
    if (!CreateProcessW(path, command, nullptr, nullptr, TRUE, CREATE_NO_WINDOW, nullptr, nullptr, &startup, &process))
        return false;
    const DWORD wait = WaitForSingleObject(process.hProcess, 15000);
    DWORD code = 1;
    if (wait == WAIT_OBJECT_0)
        GetExitCodeProcess(process.hProcess, &code);
    else {
        TerminateProcess(process.hProcess, 1);
        WaitForSingleObject(process.hProcess, 5000);
    }
    CloseHandle(process.hThread);
    CloseHandle(process.hProcess);
    std::printf("shared child pid %lu: wait 0x%08lx, exit 0x%08lx\n", process.dwProcessId, wait, code);
    return wait == WAIT_OBJECT_0 && code == 0;
}

// A texture shared across devices and processes, as DWM opens compositor surfaces.
bool shared_test(ID3D11Device* dev, ID3D11DeviceContext* ctx, IDXGIAdapter* adapter)
{
    const char* t = "shared";
    Ref<ID3D11Device> dev2;
    Ref<ID3D11DeviceContext> ctx2;
    HRESULT hr = D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, nullptr, 0, D3D11_SDK_VERSION,
                                   dev2.put(), nullptr, ctx2.put());
    if (!check(t, "D3D11CreateDevice(second)", hr))
        return result(t, false);
    D3D11_TEXTURE2D_DESC d{96, 64, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, {1, 0}, D3D11_USAGE_DEFAULT,
                           D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, D3D11_RESOURCE_MISC_SHARED};
    Ref<ID3D11Texture2D> tex;
    Ref<ID3D11RenderTargetView> rtv;
    Ref<IDXGIResource> dxgi;
    HANDLE shared = nullptr;
    if (!check(t, "CreateTexture2D(shared)", dev->CreateTexture2D(&d, nullptr, tex.put())) ||
        !check(t, "CreateRenderTargetView", dev->CreateRenderTargetView(tex, nullptr, rtv.put())) ||
        !check(t, "QueryInterface(IDXGIResource)", tex->QueryInterface(IID_PPV_ARGS(dxgi.put()))) ||
        !check(t, "GetSharedHandle", dxgi->GetSharedHandle(&shared)))
        return result(t, false);
    const float color[4] = {0.0f, 1.0f, 1.0f, 1.0f};
    ctx->ClearRenderTargetView(rtv, color);
    ctx->Flush();
    const bool cross_process = result("shared cross-process", run_shared_child(shared));
    Ref<ID3D11Texture2D> opened;
    if (!check(t, "OpenSharedResource", dev2->OpenSharedResource(shared, IID_PPV_ARGS(opened.put()))))
        return result(t, false);
    // Reopening on the creator exercises a different runtime allocation handle.
    Ref<ID3D11Texture2D> same_device;
    if (!check(t, "OpenSharedResource(same device)", dev->OpenSharedResource(shared, IID_PPV_ARGS(same_device.put()))))
        return result(t, false);
    std::vector<uint32_t> texels;
    UINT w;
    // B8G8R8A8 in little-endian memory: blue and green 0xff, red 0.
    bool ok = read_back(dev2, ctx2, opened, &texels, &w) && expect_texel(t, texels, w, 0, 0, 0xff00ffff) &&
              expect_texel(t, texels, w, 95, 63, 0xff00ffff);
    if (!ok)
        return result(t, false);
    ok = read_back(dev, ctx, same_device, &texels, &w) && expect_texel(t, texels, w, 0, 0, 0xff00ffff);
    // An imported texture must retain the blob after the exporting resource dies.
    rtv.reset();
    dxgi.reset();
    tex.reset();
    same_device.reset();
    ctx->ClearState();
    ctx->Flush();
    ok = ok && read_back(dev2, ctx2, opened, &texels, &w) && expect_texel(t, texels, w, 95, 63, 0xff00ffff);
    return result(t, ok && cross_process);
}

bool shared_nt_test(ID3D11Device* dev, ID3D11DeviceContext* ctx, IDXGIAdapter* adapter)
{
    const char* t = "shared NT";
    D3D11_TEXTURE2D_DESC desc{96, 64, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, {1, 0}, D3D11_USAGE_DEFAULT,
                              D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0,
                              D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX};
    Ref<ID3D11Texture2D> tex;
    Ref<ID3D11RenderTargetView> rtv;
    Ref<IDXGIResource1> resource;
    Ref<IDXGIKeyedMutex> producer;
    if (!check(t, "CreateTexture2D", dev->CreateTexture2D(&desc, nullptr, tex.put())) ||
        !check(t, "CreateRenderTargetView", dev->CreateRenderTargetView(tex, nullptr, rtv.put())) ||
        !check(t, "IDXGIResource1", tex->QueryInterface(IID_PPV_ARGS(resource.put()))) ||
        !check(t, "IDXGIKeyedMutex", tex->QueryInterface(IID_PPV_ARGS(producer.put()))))
        return result(t, false);
    HANDLE handle = nullptr;
    if (!check(t, "CreateSharedHandle", resource->CreateSharedHandle(nullptr,
               DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE, nullptr, &handle)))
        return result(t, false);
    Ref<ID3D11Device> dev2;
    Ref<ID3D11Device1> dev21;
    Ref<ID3D11DeviceContext> ctx2;
    Ref<ID3D11Texture2D> opened;
    Ref<IDXGIKeyedMutex> consumer;
    bool ok = check(t, "D3D11CreateDevice", D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                         0, nullptr, 0, D3D11_SDK_VERSION, dev2.put(), nullptr, ctx2.put())) &&
              check(t, "ID3D11Device1", dev2->QueryInterface(IID_PPV_ARGS(dev21.put()))) &&
              check(t, "OpenSharedResource1", dev21->OpenSharedResource1(handle, IID_PPV_ARGS(opened.put())));
    CloseHandle(handle);
    if (!ok || !check(t, "consumer mutex", opened->QueryInterface(IID_PPV_ARGS(consumer.put()))))
        return result(t, false);
    // AcquireSync can return WAIT_TIMEOUT, which is not a failed HRESULT.
    if (producer->AcquireSync(0, 5000) != S_OK)
        return result(t, false);
    const float color[4] = {0.0f, 1.0f, 1.0f, 1.0f};
    ctx->ClearRenderTargetView(rtv, color);
    std::vector<uint32_t> texels;
    UINT width;
    const HRESULT release = producer->ReleaseSync(1);
    if (!ok || !check(t, "ReleaseSync(producer)", release) || consumer->AcquireSync(1, 5000) != S_OK)
        return result(t, false);
    ok = read_back(dev2, ctx2, opened, &texels, &width) &&
         expect_texel(t, texels, width, 95, 63, 0xff00ffff);
    return result(t, check(t, "ReleaseSync(consumer)", consumer->ReleaseSync(2)) && ok);
}

// The UMDs open their own kernel device by the LUID the KMD reports for the runtime's adapter (umd/common/kmt.cpp),
// so the KMD must report the DXGI adapter's own LUID. Also lists the kernel adapters: an indirect display (remote
// control tools' virtual screens) adds one, and picking a kernel adapter by enumeration once broke every shared
// texture while one was attached.
bool kmt_adapter_test(ID3D11Device*, ID3D11DeviceContext*, IDXGIAdapter* adapter)
{
    const char* t = "kmt adapter";
    DXGI_ADAPTER_DESC desc{};
    if (!check(t, "GetDesc", adapter->GetDesc(&desc)))
        return result(t, false);
    auto query = [](D3DKMT_HANDLE handle, venera::viogpu::AdapterInfo* info) {
        D3DKMT_QUERYADAPTERINFO q{};
        q.hAdapter = handle;
        q.Type = KMTQAITYPE_UMDRIVERPRIVATE;
        q.pPrivateDriverData = info;
        q.PrivateDriverDataSize = sizeof(*info);
        return D3DKMTQueryAdapterInfo(&q) >= 0 && info->IamVioGPU == venera::viogpu::kIam;
    };
    auto close = [](D3DKMT_HANDLE handle) {
        D3DKMT_CLOSEADAPTER ca{handle};
        if (D3DKMTCloseAdapter(&ca) < 0)
            std::printf("D3DKMTCloseAdapter(0x%x) failed\n", handle);
    };
    D3DKMT_ENUMADAPTERS2 e{};
    std::vector<D3DKMT_ADAPTERINFO> adapters;
    if (D3DKMTEnumAdapters2(&e) >= 0 && e.NumAdapters) {
        adapters.resize(e.NumAdapters);
        e.pAdapters = adapters.data();
        if (D3DKMTEnumAdapters2(&e) < 0)
            e.NumAdapters = 0;
    }
    for (ULONG i = 0; i < e.NumAdapters; i++) {
        venera::viogpu::AdapterInfo info{};
        const bool ours = query(adapters[i].hAdapter, &info);
        std::printf("kernel adapter %08lx:%08lx: %lu sources%s\n", static_cast<ULONG>(adapters[i].AdapterLuid.HighPart),
                    adapters[i].AdapterLuid.LowPart, adapters[i].NumOfSources, ours ? ", viogpu" : "");
        close(adapters[i].hAdapter);
    }
    D3DKMT_OPENADAPTERFROMLUID oa{};
    oa.AdapterLuid = desc.AdapterLuid;
    const NTSTATUS s = D3DKMTOpenAdapterFromLuid(&oa);
    std::printf("DXGI adapter %08lx:%08lx: D3DKMTOpenAdapterFromLuid 0x%08lx\n",
                static_cast<ULONG>(desc.AdapterLuid.HighPart), desc.AdapterLuid.LowPart, s);
    if (s < 0)
        return result(t, false);
    venera::viogpu::AdapterInfo info{};
    const bool ours = query(oa.hAdapter, &info);
    close(oa.hAdapter);
    std::printf("KMD: %s, reports adapter %08lx:%08lx\n", ours ? "viogpu" : "NOT viogpu",
                static_cast<ULONG>(info.AdapterLuid.HighPart), info.AdapterLuid.LowPart);
    return result(t, ours && info.AdapterLuid.HighPart == desc.AdapterLuid.HighPart &&
                     info.AdapterLuid.LowPart == desc.AdapterLuid.LowPart);
}

// Keep both imports open while full updates and dirty rectangles cross the mutex.
// Odd dimensions expose row-pitch errors. Every pixel is checked on every frame.
bool shared_updates_test(ID3D11Device* dev, ID3D11DeviceContext* ctx, IDXGIAdapter* adapter)
{
    const char* t = "shared NT updates";
    constexpr UINT w = 257, h = 131, frames = 16;
    D3D11_TEXTURE2D_DESC desc{w, h, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, {1, 0}, D3D11_USAGE_DEFAULT,
                              D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0,
                              D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX};
    Ref<ID3D11Texture2D> tex, opened;
    Ref<IDXGIResource1> resource;
    Ref<IDXGIKeyedMutex> producer, consumer;
    Ref<ID3D11Device> dev2;
    Ref<ID3D11Device1> dev21;
    Ref<ID3D11DeviceContext> ctx2;
    Ref<ID3D11DeviceContext1> ctx1, ctx21;
    Ref<ID3D11RenderTargetView> rtv, rtv2;
    if (!check(t, "CreateTexture2D", dev->CreateTexture2D(&desc, nullptr, tex.put())) ||
        !check(t, "producer resource", tex->QueryInterface(IID_PPV_ARGS(resource.put()))) ||
        !check(t, "producer mutex", tex->QueryInterface(IID_PPV_ARGS(producer.put()))) ||
        !check(t, "producer context", ctx->QueryInterface(IID_PPV_ARGS(ctx1.put()))) ||
        !check(t, "producer RTV", dev->CreateRenderTargetView(tex, nullptr, rtv.put())) ||
        !check(t, "second device", D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                   0, nullptr, 0, D3D11_SDK_VERSION, dev2.put(), nullptr, ctx2.put())) ||
        !check(t, "second device1", dev2->QueryInterface(IID_PPV_ARGS(dev21.put()))) ||
        !check(t, "consumer context", ctx2->QueryInterface(IID_PPV_ARGS(ctx21.put()))))
        return result(t, false);
    HANDLE handle = nullptr;
    if (!check(t, "CreateSharedHandle", resource->CreateSharedHandle(nullptr,
               DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE, nullptr, &handle)))
        return result(t, false);
    const HRESULT hr = dev21->OpenSharedResource1(handle, IID_PPV_ARGS(opened.put()));
    CloseHandle(handle);
    if (!check(t, "OpenSharedResource1", hr) ||
        !check(t, "consumer mutex", opened->QueryInterface(IID_PPV_ARGS(consumer.put()))) ||
        !check(t, "consumer RTV", dev2->CreateRenderTargetView(opened, nullptr, rtv2.put())))
        return result(t, false);
    std::vector<uint32_t> expected(w * h), actual;
    auto pixels_match = [&](ID3D11Device* device, ID3D11DeviceContext* context, ID3D11Texture2D* image,
                            UINT frame, const char* direction) {
        UINT width = 0;
        if (!read_back(device, context, image, &actual, &width) || width != w || actual.size() != expected.size())
            return false;
        for (UINT y = 0; y < h; y++)
            for (UINT x = 0; x < w; x++)
                if (actual[y * w + x] != expected[y * w + x]) {
                    std::printf("    %s frame %u %s: ", t, frame, direction);
                    expect_texel(t, actual, w, x, y, expected[y * w + x]);
                    return false;
                }
        return true;
    };
    for (UINT frame = 0; frame < frames; frame++) {
        if (producer->AcquireSync(2 * frame, 5000) != S_OK)
            return result(t, false);
        for (UINT y = 0; y < h; y++)
            for (UINT x = 0; x < w; x++)
                expected[y * w + x] = 0xff000000u | ((x + frame * 17) & 255) |
                    (((y * 3 + frame * 11) & 255) << 8) | (((x ^ y ^ (frame * 23)) & 255) << 16);
        ctx->UpdateSubresource(tex, 0, nullptr, expected.data(), w * 4, 0);
        const D3D11_RECT dirty{LONG(frame * 7), 19, LONG(frame * 7 + 67), 83};
        const float cyan[] = {0, 1, 1, 1};
        ctx1->ClearView(rtv, cyan, &dirty, 1);
        for (LONG y = dirty.top; y < dirty.bottom; y++)
            for (LONG x = dirty.left; x < dirty.right; x++)
                expected[y * w + x] = 0xff00ffff;
        // ReleaseSync must submit the producer's commands without an explicit Flush.
        if (!check(t, "producer ReleaseSync", producer->ReleaseSync(2 * frame + 1)) ||
            consumer->AcquireSync(2 * frame + 1, 5000) != S_OK)
            return result(t, false);
        const bool forward = pixels_match(dev2, ctx2, opened, frame, "producer to consumer");
        const D3D11_RECT reply{193, 97, 251, 129};
        const float yellow[] = {1, 1, 0, 1};
        ctx21->ClearView(rtv2, yellow, &reply, 1);
        for (LONG y = reply.top; y < reply.bottom; y++)
            for (LONG x = reply.left; x < reply.right; x++)
                expected[y * w + x] = 0xffffff00;
        if (!check(t, "consumer ReleaseSync", consumer->ReleaseSync(2 * frame + 2)) || !forward ||
            producer->AcquireSync(2 * frame + 2, 5000) != S_OK)
            return result(t, false);
        const bool reverse = pixels_match(dev, ctx, tex, frame, "consumer to producer");
        if (!check(t, "producer return key", producer->ReleaseSync(2 * frame + 2)) || !reverse)
            return result(t, false);
    }
    std::printf("    %u frames, %ux%u, full updates and bidirectional dirty rectangles\n", frames, w, h);
    return result(t, true);
}

void pump_messages()
{
    MSG m;
    while (PeekMessageW(&m, nullptr, 0, 0, PM_REMOVE)) {
        TranslateMessage(&m);
        DispatchMessageW(&m);
    }
}

COLORREF reference_color = RGB(0, 0, 0);

LRESULT CALLBACK reference_window_proc(HWND window, UINT message, WPARAM wparam, LPARAM lparam)
{
    if (message == WM_PAINT) {
        PAINTSTRUCT paint{};
        HDC dc = BeginPaint(window, &paint);
        HBRUSH brush = CreateSolidBrush(reference_color);
        FillRect(dc, &paint.rcPaint, brush);
        DeleteObject(brush);
        EndPaint(window, &paint);
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

// Each model uses a fresh window. After flip presentation, Windows ignores
// legacy blt updates to that HWND even after the flip swapchain is destroyed.
bool present_test(ID3D11Device* dev, ID3D11DeviceContext* ctx, IDXGIAdapter* adapter, bool blt_only,
                  bool check_screen)
{
    const char* t = "present";
    Ref<IDXGIFactory2> factory;
    if (!check(t, "GetParent(factory)", adapter->GetParent(IID_PPV_ARGS(factory.put()))))
        return result(t, false);
    WNDCLASSW wc{};
    wc.lpfnWndProc = DefWindowProcW;
    wc.hInstance = GetModuleHandleW(nullptr);
    wc.lpszClassName = L"d3d11probe";
    RegisterClassW(&wc);
    HWND reference = nullptr;
    if (check_screen) {
        WNDCLASSW reference_class{};
        reference_class.lpfnWndProc = reference_window_proc;
        reference_class.hInstance = wc.hInstance;
        reference_class.lpszClassName = L"d3d11probe-reference";
        RegisterClassW(&reference_class);
        reference = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                                     reference_class.lpszClassName, L"GDI colour reference", WS_POPUP | WS_VISIBLE,
                                     800, 100, 160, 160, nullptr, nullptr, wc.hInstance, nullptr);
        if (!reference)
            return result(t, false);
    }
    struct Phase {
        const char* name;
        DXGI_SWAP_EFFECT effect;
        UINT buffers, width, height;
    };
    const Phase phases[] = {{"flip discard 480x270", DXGI_SWAP_EFFECT_FLIP_DISCARD, 2, 480, 270},
                            {"blt discard 480x270", DXGI_SWAP_EFFECT_DISCARD, 1, 480, 270},
                            {"flip sequential, ResizeBuffers 640x360", DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, 2, 320, 180}};
    bool ok = true;
    for (const Phase& p : phases) {
        if (blt_only && p.effect != DXGI_SWAP_EFFECT_DISCARD)
            continue;
        struct Window {
            HWND handle;
            ~Window() { if (handle) DestroyWindow(handle); }
        } window{CreateWindowW(wc.lpszClassName, L"d3d11probe", WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                                100, 100, 640, 400, nullptr, nullptr, wc.hInstance, nullptr)};
        const HWND wnd = window.handle;
        if (!wnd) {
            ok = false;
            continue;
        }
        SetForegroundWindow(wnd);
        if (check_screen)
            SetWindowPos(wnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        DXGI_SWAP_CHAIN_DESC1 sd{};
        sd.Width = p.width;
        sd.Height = p.height;
        sd.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
        sd.SampleDesc = {1, 0};
        sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
        sd.BufferCount = p.buffers;
        sd.SwapEffect = p.effect;
        Ref<IDXGISwapChain1> sc;
        HRESULT hr = factory->CreateSwapChainForHwnd(dev, wnd, &sd, nullptr, nullptr, sc.put());
        if (!check(t, p.name, hr)) {
            ok = false;
            continue;
        }
        if (p.effect == DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL) {
            hr = sc->ResizeBuffers(0, 640, 360, DXGI_FORMAT_UNKNOWN, 0);
            if (!check(t, "ResizeBuffers", hr)) {
                ok = false;
                continue;
            }
        }
        UINT presented = 0;
        for (UINT frame = 0; frame < 120; frame++) {
            Ref<ID3D11Texture2D> back;
            Ref<ID3D11RenderTargetView> rtv;
            if (FAILED(sc->GetBuffer(0, IID_PPV_ARGS(back.put()))) ||
                FAILED(dev->CreateRenderTargetView(back, nullptr, rtv.put())))
                break;
            const float f = frame / 120.0f;
            const float color[4] = {f, 1.0f - f, 0.5f, 1.0f};
            ctx->ClearRenderTargetView(rtv, color);
            hr = sc->Present(1, 0);
            if (FAILED(hr)) {
                std::printf("    %s: %s: Present failed at frame %u: 0x%08lx\n", t, p.name, frame, hr);
                std::printf("    GetDeviceRemovedReason: 0x%08lx\n", dev->GetDeviceRemovedReason());
                break;
            }
            // ANGLE (old Chromium) treats a failing GetDeviceRemovedReason as a lost context.
            hr = dev->GetDeviceRemovedReason();
            if (hr != S_OK) {
                std::printf("    %s: %s: device removed after frame %u: 0x%08lx\n", t, p.name, frame, hr);
                break;
            }
            presented++;
            pump_messages();
            if (check_screen && (frame == 30 || frame == 90)) {
                const int red = static_cast<int>(std::lround(color[0] * 255));
                const int green = static_cast<int>(std::lround(color[1] * 255));
                const int blue = static_cast<int>(std::lround(color[2] * 255));
                reference_color = RGB(red, green, blue);
                InvalidateRect(reference, nullptr, FALSE);
                UpdateWindow(reference);
                DwmFlush();
                RECT client{};
                GetClientRect(wnd, &client);
                POINT sample{client.right / 2, client.bottom / 2};
                ClientToScreen(wnd, &sample);
                const bool uncovered = GetAncestor(WindowFromPoint(sample), GA_ROOT) == wnd;
                POINT reference_sample{80, 80};
                ClientToScreen(reference, &reference_sample);
                const bool reference_uncovered = GetAncestor(WindowFromPoint(reference_sample), GA_ROOT) == reference;
                HDC screen = GetDC(nullptr);
                const COLORREF pixel = GetPixel(screen, sample.x, sample.y);
                const COLORREF expected = GetPixel(screen, reference_sample.x, reference_sample.y);
                ReleaseDC(nullptr, screen);
                // Both samples include the user's desktop colour filter.
                const bool match = uncovered && reference_uncovered && pixel != CLR_INVALID && expected != CLR_INVALID &&
                                   std::abs(int(GetRValue(pixel)) - int(GetRValue(expected))) <= 3 &&
                                   std::abs(int(GetGValue(pixel)) - int(GetGValue(expected))) <= 3 &&
                                   std::abs(int(GetBValue(pixel)) - int(GetBValue(expected))) <= 3;
                std::printf("    %s: frame %u screen RGB %u,%u,%u GDI reference %u,%u,%u: %s\n", p.name, frame,
                            GetRValue(pixel), GetGValue(pixel), GetBValue(pixel), GetRValue(expected),
                            GetGValue(expected), GetBValue(expected),
                            match ? "PASS" : "FAIL");
                ok = ok && match;
                if (!uncovered || !reference_uncovered)
                    std::printf("    screen sample is covered by another window\n");
            }
        }
        std::printf("    %s: %s: %u of 120 frames presented\n", t, p.name, presented);
        ok = ok && presented == 120;
        ctx->ClearState();
        ctx->Flush();
    }
    if (reference)
        DestroyWindow(reference);
    return result(t, ok);
}

} // namespace

namespace panel {

namespace {

HANDLE g_child_handle = nullptr;  // runD3D11SharedChild's

// Runs `test` on a D3D11 device on the VirtIO GPU.
bool on_device(bool (*test)(ID3D11Device*, ID3D11DeviceContext*, IDXGIAdapter*))
{
    AddVectoredExceptionHandler(1, report_device_removal);
    HMODULE compiler = LoadLibraryW(L"d3dcompiler_47.dll");
    g_compile = compiler ? reinterpret_cast<D3DCompileFn>(reinterpret_cast<void*>(GetProcAddress(compiler, "D3DCompile")))
                         : nullptr;
    if (!g_compile) {
        std::printf("d3dcompiler_47.dll not found: shaders cannot be compiled\n");
        return false;
    }
    Ref<IDXGIAdapter1> adapter;
    adapter.p = findAdapter();
    if (!adapter) {
        std::printf("no VirtIO GPU adapter\n");
        return false;
    }
    DXGI_ADAPTER_DESC1 desc{};
    adapter->GetDesc1(&desc);
    std::printf("adapter: %ls (vendor 0x%04x device 0x%04x flags 0x%x)\n", desc.Description, desc.VendorId,
                desc.DeviceId, desc.Flags);
    Ref<ID3D11Device> dev;
    Ref<ID3D11DeviceContext> ctx;
    D3D_FEATURE_LEVEL level{};
    // Up to 12_1, which a list must name: the default list stops at 11_1.
    const D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_12_1, D3D_FEATURE_LEVEL_12_0, D3D_FEATURE_LEVEL_11_1,
                                        D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0};
    HRESULT hr = D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, levels, ARRAYSIZE(levels),
                                   D3D11_SDK_VERSION, dev.put(), &level, ctx.put());
    std::printf("D3D11CreateDevice: 0x%08lx, feature level 0x%x, venera_d3d11.dll %s: %s\n", hr, level,
                GetModuleHandleW(L"venera_d3d11.dll") ? "loaded" : "NOT loaded", SUCCEEDED(hr) ? "ok" : "FAIL");
    return SUCCEEDED(hr) && test(dev, ctx, adapter);
}

template <bool (*F)(ID3D11Device*, ID3D11DeviceContext*)>
bool run()
{
    return on_device([](ID3D11Device* dev, ID3D11DeviceContext* ctx, IDXGIAdapter*) { return F(dev, ctx); });
}

template <bool (*F)(ID3D11Device*, ID3D11DeviceContext*, IDXGIAdapter*)>  // the tests that need the adapter
bool runAdapter()
{
    return on_device(F);
}

bool device_test(ID3D11Device*, ID3D11DeviceContext*) { return true; }

template <bool BltOnly, bool CheckScreen>
bool present()
{
    if (CheckScreen)
        SetProcessDPIAware();
    return on_device([](ID3D11Device* dev, ID3D11DeviceContext* ctx, IDXGIAdapter* adapter) {
        return present_test(dev, ctx, adapter, BltOnly, CheckScreen);
    });
}

}  // namespace

TestGroup d3d11Tests()
{
    return {"d3d11", L"Direct3D 11", L"Venera's D3D11 driver on DXVK", {
        {"device", L"Device", L"Creates a device; venera_d3d11.dll must load", run<device_test>},
        {"clear", L"Clear", L"Clears a render target and reads it back", run<clear_test>},
        {"triangle", L"Triangle", L"Shaders, a constant buffer and a vertex buffer", run<triangle_test>},
        {"textured", L"Textures", L"Sampling a texture through a shader resource view", run<textured_test>},
        {"depth", L"Depth and MSAA", L"Depth tests, MSAA and a resolve", run<depth_test>},
        {"compute", L"Compute", L"A dispatch writing a UAV", run<compute_test>},
        {"stream-output", L"Stream output", L"A vertex shader's outputs streamed to a buffer, no geometry shader",
         run<stream_output_test>},
        {"queries", L"Queries", L"An event, an occlusion query and timestamps", run<queries_test>},
        {"update", L"Updates", L"Dynamic buffers mapped with discard and no-overwrite, UpdateSubresource",
         run<update_test>},
        {"features", L"D3D11.3 features", L"Feature level and the D3D11.3 options (typed UAV loads and the rest)",
         run<features_test>},
        {"typed-uav-load", L"Typed UAV loads", L"A compute shader reading and writing RG32F through a UAV, as GravityMark",
         run<typed_uav_load_test>},
        {"tiled", L"Tiled resources", L"Tile mappings on a tile pool, UpdateTiles, CopyTiles, an unmapped tile",
         run<tiled_test>},
        {"conservative", L"Conservative rasterization", L"A triangle smaller than a pixel, missing its centre",
         run<conservative_test>},
        {"trim", L"Trim", L"IDXGIDevice3::Trim, then a clear", run<trim_test>},
        {"kmt-adapter", L"Kernel adapter",
         L"The KMD reports the DXGI adapter's LUID, where the drivers open shared memory", runAdapter<kmt_adapter_test>},
        {"shared", L"Shared texture", L"A texture opened by another device and process, as DWM does",
         runAdapter<shared_test>},
        {"shared-nt", L"Shared NT handle", L"The same through an NT handle", runAdapter<shared_nt_test>},
        {"shared-updates", L"Shared updates", L"Updates and dirty rectangles across a keyed mutex, every pixel checked",
         runAdapter<shared_updates_test>},
        {"present", L"Swapchains", L"Flip, legacy blt and resized swapchains in a window", present<false, false>},
        {"present-blt", L"Legacy swapchain", L"The legacy blt model alone", present<true, false>},
        {"present-check", L"Swapchain colours", L"Screen colours against a GDI reference; keep the windows uncovered",
         present<false, true>},
    }};
}

int runD3D11SharedChild(const wchar_t* handle)
{
    g_child_handle = reinterpret_cast<HANDLE>(static_cast<uintptr_t>(std::wcstoull(handle, nullptr, 10)));
    return on_device([](ID3D11Device* dev, ID3D11DeviceContext* ctx, IDXGIAdapter*) {
        return result("shared child pixels", shared_child(dev, ctx, g_child_handle));
    }) ? 0 : 1;
}

}  // namespace panel
