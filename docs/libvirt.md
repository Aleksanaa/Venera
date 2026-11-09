# libvirt (virt-manager)

The display is SPICE with GL. Stock QEMU's SPICE GL holds all of the guest's GPU commands after every frame until
the SPICE client has drawn it, which makes the desktop feel laggy. Apply yttrium's QEMU patch
[cf3bba5c26](https://github.com/arehnman/yttrium-qemu/commit/cf3bba5c26) ("ui: decouple GL presentation from
renderer blocking"), from the `yttrium` branch of [arehnman/yttrium-qemu](https://github.com/arehnman/yttrium-qemu).
It applies cleanly to QEMU 11.1.

The guest needs a virtio-gpu device with `venus`, `blob` and a `hostmem` window, and its memory must be a shared
memfd. libvirt has no attribute for `venus`, so set it through an override on the video device. The `<graphics>`
element is for lower SPICE latency, by disabling image, audio, video compression, and streaming, over a local socket
(`listen type='none'`), with GL on the host's render node.

```xml
<memoryBacking>
  <source type='memfd'/>
  <access mode='shared'/>
</memoryBacking>
...
<graphics type='spice'>
  <listen type='none'/>
  <image compression='off'/>
  <jpeg compression='never'/>
  <zlib compression='never'/>
  <playback compression='off'/>
  <streaming mode='off'/>
  <mouse mode='server'/>
  <gl enable='yes' rendernode='/dev/dri/renderD128'/>
</graphics>
<video>
  <model type='virtio' heads='1' primary='yes' blob='on'>
    <acceleration accel3d='yes'/>
  </model>
</video>
...
<qemu:override>
  <qemu:device alias='video0'>
    <qemu:frontend>
      <qemu:property name='venus' type='bool' value='true'/>
      <qemu:property name='hostmem' type='unsigned' value='4294967296'/>
    </qemu:frontend>
  </qemu:device>
</qemu:override>
```

Give the guest at least 4 vCPUs, as cores of one socket, as the guest does all the translation on its own CPUs.
With fewer CPUs, applications fall behind their own GPU work, and VRAM leak may be observed.
Without a `<topology>`, libvirt gives each vCPU its own socket, and Windows can only utilize 1 or 2. Task Manager
shows how many it uses ("Logical processors"). Add config like this:

```xml
<vcpu placement='static'>4</vcpu>
...
<cpu mode='host-passthrough' check='none' migratable='on'>
  <topology sockets='1' dies='1' clusters='1' cores='4' threads='1'/>
</cpu>
```

libvirt starts QEMU with a `-sandbox` whose seccomp filter kills virglrenderer's render server (see
[qemu.md](qemu.md)), so set `seccomp_sandbox = 0` in libvirt's `qemu.conf` and restart libvirtd.

For lower input latency, add virtio mouse and keyboard in place of the emulated PS/2 ones (the guest needs
virtio-win's `vioinput` driver).
