# NVIDIA host patches

NVIDIA can expose linear DMA-BUFs as external-only EGL imports.
QEMU needs the external-texture path for GTK and SPICE to display these buffers.
See [issue #7](https://github.com/Aleksanaa/Venera/issues/7).

## QEMU

Apply all commits in [arehnman/yttrium-qemu](https://github.com/arehnman/yttrium-qemu) from
[`cf3bba5c26`](https://github.com/arehnman/yttrium-qemu/commit/cf3bba5c26df3ad912c29b71dd0bf5d3f6d6b652) ("ui: decouple GL presentation from renderer blocking") to
[`3001a0746a`](https://github.com/arehnman/yttrium-qemu/commit/3001a0746a7fe78e03c611fddf09705e718117b0) ("ui/opengl: keep external-copy VAOs local to each blit").
The series was checked against [QEMU 11.1.2](https://download.qemu.org/qemu-11.1.2.tar.xz) with zero fuzz.
Other QEMU versions can need changes to the patch context.

## Renderer and configuration

Use [arehnman/virglrenderer at `3d681c0a1a25694d20f655e95a2f95e70b62e7c6`](https://github.com/arehnman/virglrenderer/commit/3d681c0a1a25694d20f655e95a2f95e70b62e7c6)
with Venus support and its matching `virgl_render_server`.
Build QEMU with OpenGL, virglrenderer, and the GTK or SPICE frontend.

See [QEMU configuration](../../qemu.md) for shared memory, the virtio GPU, and Intel CPU PAT settings.
See [libvirt configuration](../../libvirt.md) for the `seccomp_sandbox = 0` setting.
Remove any explicit `-sandbox` argument that enables the filter in the VM command line.

The display patches do not establish a fix for
[the fragment shading rate assertion in issue #8](https://github.com/Aleksanaa/Venera/issues/8).
