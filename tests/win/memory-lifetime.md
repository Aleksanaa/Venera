# Memory lifetime checks

Build the probes in the project dev shell:

```sh
nix develop --command bash -c 'cmake --build build/win --target memorytest engineprobe'
```

Run `memorytest.exe` in Windows. It uses fault injection and submits no GPU work.
It checks 100,000 resource open/close operations, hash collisions, final detach,
resource reuse, and rollback after a failed attach. It also checks external memory
type selection, failed imports, handle ownership, and the extension guard.

Put `engineprobe.exe` beside the built `venera_vulkan_virtio.dll`. Run it in the
logged-in Windows desktop session. A guest-agent service session can lack access
to the display adapter.

The probe creates a private device and submits 256 empty command buffers. It must
allocate only one command buffer. It then checks recovery after fence allocation
and command recording failures, and rejects reuse after a failed completion wait.
The wait test completes real GPU work before it injects the error.

Use `engineprobe.exe --reuse-only` to check command buffer reuse without error
injection. Both probes return zero on success and print each completed check.

## Shared image integration

Install the KMD and UMD from the same build before a shared image test. The new
`supports_resource_refs` capability uses one reserved bit in the existing adapter
info structure. Older KMDs cannot safely support the temporary import used by the
memory-type query. The UMD rejects shared image imports on those KMDs.

For an integration run, keep a device alive while repeatedly creating, opening,
and closing shared textures. Include host-visible exports on a discrete GPU.
Open the same resource more than once, close one handle, then use the remaining
handle and check its pixels. Only the final close must detach the host resource.
Also test repeated swapchain resizing and recovery after allocation failure.

The fault-injection tests do not replace this installed-driver integration check.
