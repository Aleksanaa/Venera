# NVIDIA host patch set

This directory records the host patches used with Venera on an NVIDIA RTX 4060 host and a Windows 11 guest.
The host uses an Intel Core i5-13600KF, QEMU/KVM, libvirt, and SPICE OpenGL. GTK uses a separate renderer build.

NVIDIA can expose linear DMA-BUFs as external-only EGL imports.
QEMU must sample these buffers through `GL_TEXTURE_EXTERNAL_OES` instead of attaching them directly to a framebuffer.
The QEMU series adds that path for GTK and SPICE, with its mailbox dependencies and later fixes.
See [the host requirements in issue #7](https://github.com/Aleksanaa/Venera/issues/7).

## Source versions

| Component | Base | Local changes |
| --- | --- | --- |
| QEMU | [11.1.2 release](https://download.qemu.org/qemu-11.1.2.tar.xz) | All patches in `qemu/series`, in order |
| virglrenderer | [arehnman/virglrenderer at `3d681c0a1a25694d20f655e95a2f95e70b62e7c6`](https://github.com/arehnman/virglrenderer/commit/3d681c0a1a25694d20f655e95a2f95e70b62e7c6) | `01-libvirt-seccomp.patch`, plus `02-gtk-cursor-rgba.patch` for GTK |

Use the pinned virglrenderer fork as the base. The two local patches do not include the fork's Venus changes.
Build the matching `virgl_render_server` with Venus support, and make sure QEMU loads this renderer library.
Keep the render server available in the runtime environment.

## QEMU series

The files preserve the original authors and commit IDs from
[arehnman/yttrium-qemu](https://github.com/arehnman/yttrium-qemu).
Patch 05 has a corrected mail subject. Its code is unchanged.

| Patches | Purpose |
| --- | --- |
| 01–03 | Decouple GTK/SPICE presentation from renderer progress, then fix SPICE orientation and wakeups |
| 04 | Reset the console display state when virtio scanouts reset |
| 05 | Work around input cleanup with older SPICE servers |
| 06 | Remove fixed-function texture calls from shader paths |
| 07–08 | Import external-only DMA-BUFs and add a shader copy path |
| 09 | Mark udmabuf framebuffers as linear |
| 10–11 | Enable external sampling in GTK and SPICE |
| 12 | Keep global and context async fence completions separate |
| 13 | Create the external-copy VAO per blit after GL context recreation |

Apply the complete series to a clean QEMU 11.1.2 source tree.
Set `patchset` to the absolute path of this directory:

```sh
patchset=/absolute/path/to/Venera/docs/nvidia_patchset
cd /absolute/path/to/qemu-11.1.2
while IFS= read -r file; do
    patch --batch --fuzz=0 -p1 < "$patchset/qemu/$file" || exit 1
done < "$patchset/qemu/series"
```

Build QEMU with OpenGL, virglrenderer, and the GTK or SPICE frontend.
Other QEMU versions can need changes to the patch context.

## virglrenderer patches

`01-libvirt-seccomp.patch` converts the local build's `setpgid` substitution into a patch.
QEMU's `elevateprivileges=deny` sandbox blocks this call when the render server starts.
The workaround skips the call and leaves the child in its existing process group.
It is specific to this sandbox setup. Review that process-group change for your host.
See [QEMU sandbox requirements](../qemu.md) for the alternative sandbox configuration.

From the pinned virglrenderer source tree, apply:

```sh
patch --batch --fuzz=0 -p1 < "$patchset/virglrenderer/01-libvirt-seccomp.patch"
```

For GTK, also apply `02-gtk-cursor-rgba.patch` before the build:

```sh
patch --batch --fuzz=0 -p1 < "$patchset/virglrenderer/02-gtk-cursor-rgba.patch"
```

The cursor patch requests `GL_RGBA`, `GL_UNSIGNED_BYTE`, and four bytes per pixel for QEMU GTK.
The local SPICE setup uses the renderer without this cursor patch.
Check cursor colors and transparency separately for each frontend.

## Runtime configuration and checks

Use shared memfd guest memory and a virtio GPU with Venus, blob resources, and a hostmem window.
Intel CPU hosts also need `-accel kvm,honor-guest-pat=on`.
See [QEMU configuration](../qemu.md) and [libvirt configuration](../libvirt.md).
Select the NVIDIA Vulkan ICD if the host has more than one Vulkan driver.

This submission checks patch application against both pinned sources with zero fuzz.
It also checks that the resulting files match the local patch set.
It does not repeat a build, driver installation, or guest graphics test.
The local libvirt configuration selects the patched QEMU and the SPICE renderer described above.
The GTK cursor patch remains a separate configuration.

The patches do not establish a fix for
[the fragment shading rate assertion in issue #8](https://github.com/Aleksanaa/Venera/issues/8).
Test desktop display, cursor behavior, driver reinstall, and graphics workloads after installation.
