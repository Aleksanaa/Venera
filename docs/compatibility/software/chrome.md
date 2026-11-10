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

The check is the `MITIGATION_FORCE_MS_SIGNED_BINS` mitigation, which Chromium applies once the process has started
(`sandbox/policy/win/sandbox_win.cc`). One switch turns off only this check:

```
chrome.exe --allow-third-party-modules
```

Chrome passes the switch on to the GPU process, which then leaves out the mitigation. The rest of the sandbox stays in
place: chrome://gpu still shows "Sandboxed: true", and the GPU process loads `venera_d3d12.dll`. Renderers don't get
the switch and keep their own signature checks. Chrome treats it as an unsupported flag, so it shows a warning bar
when it starts.

Running the GPU process without any sandbox, with `--disable-gpu-sandbox`, also works, but it weakens Chrome's
security far more: a bug in the GPU process can then be exploited with the user's full rights.

## Enabling WebGPU

Both are needed:

1. `chrome://flags/#enable-unsafe-webgpu` set to Enabled.
2. Chrome started with `--allow-third-party-modules`, for example from a shortcut. Close every Chrome window first,
   or the switch is ignored.

`requestAdapter()` then returns our adapter (backend D3D12, "VirtIO GPU (Venus) with Venera D3D10/11/12"), and
devices can be created on it. Without the second switch, every `requestAdapter()` call returns null, including
`forceFallbackAdapter`, even with the first one set.

## Keeping the switch

Chrome has no setting or policy for its command line, so `--allow-third-party-modules` has to go everywhere Chrome is
started from:

- The shortcuts: the Start menu's (`C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Google Chrome.lnk`), the
  taskbar's (`%APPDATA%\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar\Google Chrome.lnk`) and the
  desktop's (`C:\Users\Public\Desktop\Google Chrome.lnk`). Add the switch in Properties → Target, after the closing
  quote of the path.
- The commands Windows runs to open links and PDFs with Chrome, which start Chrome when it isn't running. They're the
  default values of `HKEY_LOCAL_MACHINE\SOFTWARE\Classes\ChromeHTML\shell\open\command` and of
  `...\ChromePDF\shell\open\command`:

  ```
  "C:\Program Files\Google\Chrome\Application\chrome.exe" --allow-third-party-modules --single-argument %1
  ```

Chrome reads its switches only when the first window starts. If Chrome is already running, launching it again with
the switch only opens a new window, and the switch is ignored, so close every window first, including any Chrome left
running in the background. chrome://gpu → Command Line shows whether the running browser has the switch.

Chrome's installer may rewrite the shortcuts and the registry values when Chrome updates, so check them again
after an update.
