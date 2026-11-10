# Chrome

Out of the box, chrome://gpu shows compositing, WebGL and video decode as hardware accelerated, on our Direct3D 11.
WebGPU doesn't work. `navigator.gpu.requestAdapter()` returns null, and the WebGPU samples say "requestAdapter returned
null - this sample can't run on this system", although chrome://gpu lists WebGPU as hardware accelerated: that line
doesn't take Chrome's per-adapter checks into account. There are two such checks, and each needs its own switch.

Tested with Chrome 157. Edge is built on the same Chromium code and should behave the same, but it hasn't been tested.

## Vendor allowlist

On Windows, Chromium offers a Direct3D 12 adapter to web pages only if its PCI vendor is AMD, Intel, Microsoft,
NVIDIA or Qualcomm (`gpu/config/webgpu_blocklist_impl.cc`). The VirtIO GPU's vendor ID, 0x1AF4, isn't one of them, so
the Dawn Info section of chrome://gpu shows our adapter as:

```
Blocklisted - crbug.com/446254136: Limited support / testing currently available on Windows.
```

The check looks at the vendor ID only, not at the driver or its version. `chrome://flags/#enable-unsafe-webgpu`
switches it off, together with the rest of the WebGPU adapter blocklist, and turns on WebGPU features that are still
experimental.

## Signature check in the GPU process

Chrome's GPU process starts with a looser sandbox and tightens it once it has set up the GPU. From then on, it loads
only DLLs signed by Microsoft. GPU vendors' drivers pass, because Microsoft signs them through WHQL; ours are
test-signed. What this means for each of our drivers depends on when Chrome first needs it:

- Direct3D 11 is what Chrome composites with, so the GPU process loads `venera_d3d11.dll` while it sets up, before
  the sandbox tightens. The DLL is already in memory by then and never checked, so compositing and WebGL work.
- Direct3D 12 is first needed when a page asks for WebGPU, after the sandbox has tightened. Windows then refuses to
  load `venera_d3d12.dll`, and Dawn never sees our adapter. `#enable-unsafe-webgpu` has nothing left to unblock.

Windows logs each refusal in Event Viewer, under Applications and Services Logs → Microsoft → Windows →
CodeIntegrity → Operational (event 3033). Here they only ever name `venera_d3d12.dll`, never `venera_d3d11.dll`:

```
Code Integrity determined that a process (...\chrome.exe) attempted to load
...\venera.inf_amd64_...\venera_d3d12.dll that did not meet the Microsoft signing level requirements.
```

Chrome's own `vulkan-1.dll` and `vk_swiftshader.dll` are refused in the same way, which is why chrome://gpu lists no
Vulkan adapters either.

Chrome has no switch, feature or policy that turns off only this check. The only way past it is to run the GPU process
without its sandbox:

```
chrome.exe --disable-gpu-sandbox
```

This weakens Chrome's security: a bug in the GPU process can then be exploited with the user's full rights. Use it
for testing, not for everyday browsing. Microsoft-signed drivers would remove the need for it, but that takes
Microsoft's hardware signing program, which this project doesn't have.

## Enabling WebGPU

Both are needed:

1. `chrome://flags/#enable-unsafe-webgpu` set to Enabled.
2. Chrome started with `--disable-gpu-sandbox`, for example from a shortcut. Close every Chrome window first, or the
   switch is ignored.

`requestAdapter()` then returns our adapter (backend D3D12, "VirtIO GPU (Venus) with Venera D3D10/11/12"), and
devices can be created on it.
