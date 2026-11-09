// SPDX-License-Identifier: MIT
// Tiled resources (D3D11.2, in the WDDM 2.0 DDI): DXVK's, on sparse Vulkan resources. The runtime has validated the
// arguments, and its tile structures and flags are the API's.
#include "device.h"

#include <cstddef>

namespace venera::umd {

namespace {

static_assert(sizeof(D3DWDDM1_3DDI_TILED_RESOURCE_COORDINATE) == sizeof(D3D11_TILED_RESOURCE_COORDINATE) &&
              offsetof(D3DWDDM1_3DDI_TILED_RESOURCE_COORDINATE, Subresource) ==
                  offsetof(D3D11_TILED_RESOURCE_COORDINATE, Subresource));
static_assert(sizeof(D3DWDDM1_3DDI_TILE_REGION_SIZE) == sizeof(D3D11_TILE_REGION_SIZE) &&
              offsetof(D3DWDDM1_3DDI_TILE_REGION_SIZE, Width) == offsetof(D3D11_TILE_REGION_SIZE, Width) &&
              offsetof(D3DWDDM1_3DDI_TILE_REGION_SIZE, Depth) == offsetof(D3D11_TILE_REGION_SIZE, Depth));
static_assert(UINT(D3DWDDM1_3DDI_TILE_MAPPING_NO_OVERWRITE) == UINT(D3D11_TILE_MAPPING_NO_OVERWRITE));
static_assert(UINT(D3DWDDM_1_3DDI_TILE_RANGE_NULL) == UINT(D3D11_TILE_RANGE_NULL) &&
              UINT(D3DWDDM_1_3DDI_TILE_RANGE_SKIP) == UINT(D3D11_TILE_RANGE_SKIP) &&
              UINT(D3DWDDM_1_3DDI_TILE_RANGE_REUSE_SINGLE_TILE) == UINT(D3D11_TILE_RANGE_REUSE_SINGLE_TILE));
static_assert(UINT(D3DWDDM1_3DDI_TILE_COPY_NO_OVERWRITE) == UINT(D3D11_TILE_COPY_NO_OVERWRITE) &&
              UINT(D3DWDDM1_3DDI_TILE_COPY_LINEAR_BUFFER_TO_SWIZZLED_TILED_RESOURCE) ==
                  UINT(D3D11_TILE_COPY_LINEAR_BUFFER_TO_SWIZZLED_TILED_RESOURCE) &&
              UINT(D3DWDDM1_3DDI_TILE_COPY_SWIZZLED_TILED_RESOURCE_TO_LINEAR_BUFFER) ==
                  UINT(D3D11_TILE_COPY_SWIZZLED_TILED_RESOURCE_TO_LINEAR_BUFFER));

const D3D11_TILED_RESOURCE_COORDINATE* api(const D3DWDDM1_3DDI_TILED_RESOURCE_COORDINATE* c)
{
    return reinterpret_cast<const D3D11_TILED_RESOURCE_COORDINATE*>(c);
}

const D3D11_TILE_REGION_SIZE* api(const D3DWDDM1_3DDI_TILE_REGION_SIZE* s)
{
    return reinterpret_cast<const D3D11_TILE_REGION_SIZE*>(s);
}

void APIENTRY update_tile_mappings(D3D10DDI_HDEVICE h, D3D10DDI_HRESOURCE tiled, UINT regions,
                                   const D3DWDDM1_3DDI_TILED_RESOURCE_COORDINATE* starts,
                                   const D3DWDDM1_3DDI_TILE_REGION_SIZE* sizes, D3D10DDI_HRESOURCE pool, UINT ranges,
                                   const UINT* range_flags, const UINT* pool_starts, const UINT* range_counts,
                                   UINT flags)
{
    Device* d = Device::from(h);
    HRESULT hr = d->ctx->UpdateTileMappings(resource_of(tiled), regions, api(starts), api(sizes), buffer_of(pool),
                                            ranges, range_flags, pool_starts, range_counts, flags);
    if (FAILED(hr)) {
        VENERA_WARN("UpdateTileMappings: %u regions, %u ranges: 0x%08lx", regions, ranges, hr);
        d->set_error(hr == E_OUTOFMEMORY ? hr : E_INVALIDARG);
    }
}

void APIENTRY copy_tile_mappings(D3D10DDI_HDEVICE h, D3D10DDI_HRESOURCE dst,
                                 const D3DWDDM1_3DDI_TILED_RESOURCE_COORDINATE* dst_start, D3D10DDI_HRESOURCE src,
                                 const D3DWDDM1_3DDI_TILED_RESOURCE_COORDINATE* src_start,
                                 const D3DWDDM1_3DDI_TILE_REGION_SIZE* size, UINT flags)
{
    Device* d = Device::from(h);
    HRESULT hr = d->ctx->CopyTileMappings(resource_of(dst), api(dst_start), resource_of(src), api(src_start),
                                          api(size), flags);
    if (FAILED(hr)) {
        VENERA_WARN("CopyTileMappings: 0x%08lx", hr);
        d->set_error(hr == E_OUTOFMEMORY ? hr : E_INVALIDARG);
    }
}

void APIENTRY copy_tiles(D3D10DDI_HDEVICE h, D3D10DDI_HRESOURCE tiled,
                         const D3DWDDM1_3DDI_TILED_RESOURCE_COORDINATE* start,
                         const D3DWDDM1_3DDI_TILE_REGION_SIZE* size, D3D10DDI_HRESOURCE buffer, UINT64 offset,
                         UINT flags)
{
    Device::from(h)->ctx->CopyTiles(resource_of(tiled), api(start), api(size), buffer_of(buffer), offset, flags);
}

void APIENTRY update_tiles(D3D10DDI_HDEVICE h, D3D10DDI_HRESOURCE tiled,
                           const D3DWDDM1_3DDI_TILED_RESOURCE_COORDINATE* start,
                           const D3DWDDM1_3DDI_TILE_REGION_SIZE* size, const void* data, UINT flags)
{
    Device::from(h)->ctx->UpdateTiles(resource_of(tiled), api(start), api(size), data, flags);
}

// A resource or view by its driver handle; null for none.
ID3D11DeviceChild* barrier_object(D3D11DDI_HANDLETYPE type, void* handle)
{
    if (!handle)
        return nullptr;
    switch (type) {
    case D3D10DDI_HT_RESOURCE: return static_cast<ResourceObject*>(handle)->resource.get();
    case D3D10DDI_HT_SHADERRESOURCEVIEW:
    case D3D10DDI_HT_RENDERTARGETVIEW:
    case D3D10DDI_HT_DEPTHSTENCILVIEW:
    case D3D11DDI_HT_UNORDEREDACCESSVIEW: return static_cast<ViewObject*>(handle)->view.get();
    default:
        VENERA_WARN("TiledResourceBarrier: handle type %d", static_cast<int>(type));
        return nullptr;
    }
}

void APIENTRY tiled_resource_barrier(D3D10DDI_HDEVICE h, D3D11DDI_HANDLETYPE before_type, void* before,
                                     D3D11DDI_HANDLETYPE after_type, void* after)
{
    Device::from(h)->ctx->TiledResourceBarrier(barrier_object(before_type, before),
                                               barrier_object(after_type, after));
}

// The runtime works out the rest of GetResourceTiling (tile counts and shapes) from the standard tile shapes.
void APIENTRY get_mip_packing(D3D10DDI_HDEVICE h, D3D10DDI_HRESOURCE tiled, UINT* packed_mips,
                              UINT* tiles_for_packed_mips)
{
    D3D11_PACKED_MIP_DESC packed{};
    Device::from(h)->d3d->GetResourceTiling(resource_of(tiled), nullptr, &packed, nullptr, nullptr, 0, nullptr);
    *packed_mips = packed.NumPackedMips;
    *tiles_for_packed_mips = packed.NumTilesForPackedMips;
}

void APIENTRY resize_tile_pool(D3D10DDI_HDEVICE h, D3D10DDI_HRESOURCE pool, UINT64 size)
{
    Device* d = Device::from(h);
    HRESULT hr = d->ctx->ResizeTilePool(buffer_of(pool), size);
    if (FAILED(hr)) {
        VENERA_WARN("ResizeTilePool: %llu bytes: 0x%08lx", static_cast<unsigned long long>(size), hr);
        d->set_error(hr == E_OUTOFMEMORY ? hr : E_INVALIDARG);
    }
}

} // namespace

void fill_tile_ddi(D3DWDDM2_0DDI_DEVICEFUNCS& t)
{
    t.pfnUpdateTileMappings = update_tile_mappings;
    t.pfnCopyTileMappings = copy_tile_mappings;
    t.pfnCopyTiles = copy_tiles;
    t.pfnUpdateTiles = update_tiles;
    t.pfnTiledResourceBarrier = tiled_resource_barrier;
    t.pfnGetMipPacking = get_mip_packing;
    t.pfnResizeTilePool = resize_tile_pool;
}

} // namespace venera::umd
