# Firefox

Out of the box, Firefox on Windows doesn't use our GPU. It blocks every graphics feature for GPU vendors it doesn't
know (`FEATURE_FAILURE_UNKNOWN_DEVICE_VENDOR` in about:support), and the VirtIO GPU's vendor ID, 0x1AF4, isn't on its
list. So WebRender draws in software, WebGL runs on ANGLE over Microsoft's software renderer (WARP, "Microsoft Basic
Render Driver"), WebGL 2 is off, and `navigator.gpu.requestAdapter()` returns null. Ticking "Use hardware acceleration
when available" in the settings doesn't change any of this.

Tested with Firefox 159.0.1 and Venera 0.2.2.

## Enabling hardware acceleration

Set these in `about:config`, or put them in `user.js` in the profile folder, then restart Firefox:

```
user_pref("layers.d3d11.enable-blacklist", false);  // D3D11 compositing on our driver
user_pref("gfx.webrender.all", true);               // hardware WebRender
user_pref("gfx.webgpu.ignore-blocklist", true);     // WebGPU on our Direct3D 12
user_pref("webgl.disable-angle", true);             // WebGL through OpenGL instead of ANGLE
user_pref("webgl.force-enabled", true);             // ignore the blocklist for WebGL
```

about:support should then show:

- Compositing: WebRender
- WebGL 1 and WebGL 2 Driver Renderer: `Mesa -- zink Vulkan 1.4(Virtio-GPU Venus (...))`
- WebGPU Default Adapter: backend `Dx12`, name `VirtIO GPU (Venus) with Venera D3D10/11/12`

WebGL then runs on OpenGL, which our driver provides through Mesa's zink on Venus. Web pages see a generic renderer
name ("Radeon R9 200 Series, or similar"): this is Firefox's fingerprinting protection, not a wrong GPU.

## WebGL through Direct3D 11

Firefox's usual WebGL path, ANGLE on Direct3D 11, can't be enabled with prefs. `webgl.force-enabled`,
`webgl.ignore-blocklist`, `webgl.angle.force-d3d11` and `gfx.blocklist.all = -1` all leave it on WARP. Firefox
decides this from the vendor check above, and nothing in the prefs overrides it: it doesn't even open a D3D11 device
on our adapter for WebGL.

The only way to get it is for Firefox to believe it is running on another vendor's GPU. Firefox has test environment
variables for this, and with them ANGLE, WebGL and WebGPU all run on our driver:

```
MOZ_GFX_SPOOF_VENDOR_ID=0x1002
MOZ_GFX_SPOOF_DEVICE_ID=0x1586
MOZ_GFX_SPOOF_DRIVER_VERSION=32.0.21025.10016
```

We don't recommend this, as it may cause compatibility issues:

- Firefox then applies that vendor's blocklist entries and driver workarounds, which were written for a different
  driver.
- Firefox updates its blocklist online, so a future entry for the faked driver version can silently turn
  acceleration off again.
- Crash reports from our stack would be filed under the other vendor.
- These variables are a testing aid. Mozilla doesn't support them and may remove them.

The OpenGL path above needs none of this.

## Known messages

- `DoesRecreatingCreateTexture2DFail` in about:support's failure log: one of Firefox's startup checks creates a shared
  texture with a full mip chain, and our driver only shares single-level textures. Firefox notes it and carries on.
- `Failed to create a D3D11 content device: 0x887a0004` from the RDD process, and `Failed to get D3D11VideoDevice`:
  our driver has no video decode (the content device most likely asks for video support), so Firefox decodes video
  in software.
