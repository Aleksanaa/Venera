# QEMU

For NVIDIA hosts, see the [host patch set](nvidia_patchset/README.md) for pinned sources, patches, and application order.

QEMU's own SDL or GTK window draws without SPICE's blocking, so stock QEMU is smooth here. With GTK, yttrium's QEMU patch
[cf3bba5c26](https://github.com/arehnman/yttrium-qemu/commit/cf3bba5c26) ("ui: decouple GL presentation from
renderer blocking") still helps, so a hidden or minimized GTK window doesn't stall the guest. SDL2 doesn't need it.

The guest needs a virtio-gpu device with `venus`, `blob` and a `hostmem` window, and its memory must be a shared
memfd:

```
-machine q35,memory-backend=mem
-accel kvm,honor-guest-pat=on  # Intel CPU hosts, see below
-object memory-backend-memfd,id=mem,size=8G,share=on
-device virtio-vga-gl,venus=on,blob=on,hostmem=4G
-display sdl,gl=on            # or gtk,gl=on
```

Give the guest at least 4 logical CPUs, as cores of one socket:

```
-smp 4,sockets=1,cores=4,threads=1
```

The guest does all the translation on its own CPUs. With fewer CPUs, applications fall behind their own
GPU work, and VRAM leak may be observed. Windows client editions also use only some sockets
(Home 1, Pro 2). Task Manager shows how many it uses ("Logical processors").

On an Intel CPU host, `-accel kvm,honor-guest-pat=on` is needed. Without it, KVM on Intel ignores the guest's page
attributes and maps the `hostmem` window write-back, so the guest's write-combined views of host GPU memory are not
coherent with the GPU. KVM on AMD always honors them, so AMD hosts don't need it.

Don't pass `-sandbox` with `elevateprivileges=deny`. Its seccomp filter kills virglrenderer's render server as it
starts (the forked child calls `setpgid`), so the guest's Venus context never exists and the host logs
`virgl blob create error: Invalid argument` and `virtio_gpu_virgl_process_cmd: ctrl 0x10c, error 0x1200` for every
blob while the guest stays black.

- **SDL** is the most recommended. Set `SDL_VIDEODRIVER=wayland` and `SDL_VIDEO_WAYLAND_SCALE_TO_DISPLAY=1` (makes
  SDL 3 measure the window in physical pixels instead of logical ones) if you are using wayland. It's very smooth,
  clear, and bug-free.
- **GTK** has menus, but GTK 3 draws only at integer scales, so on a fractionally scaled display the compositor
  resamples the window and text looks slightly pixelated.

For lower input latency, use virtio input devices (`-device virtio-keyboard-pci -device virtio-tablet-pci`) in place
of the emulated PS/2 and USB ones (the guest needs virtio-win's `vioinput` driver).

[run-qemu.sh](run-qemu.sh) is a reference script that I rely on. You may find helpful pieces there.
