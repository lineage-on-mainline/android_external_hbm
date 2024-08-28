# HBM

HBM is a hardware buffer allocator.

`hbm` crate provides a Rust library to allocate, export/import, and access
hardware buffers.

`hbm-minigbm` crate provides an unstable C API for
[minigbm](https://chromium.googlesource.com/chromiumos/platform/minigbm/)'s
internal use.

`hbm-gralloc` crate provides a HAL service for [Graphics
Allocator](https://android.googlesource.com/platform/hardware/interfaces/+/refs/heads/main/graphics/allocator/aidl/)
interface and an SP-HAL for [Graphics
Mapper](https://android.googlesource.com/platform/hardware/interfaces/+/refs/heads/main/graphics/mapper/stable-c)
interface on Android.  It is mainly built via the Android build system rather
than via cargo.

## TODOs

`hbm`

  - quirk device and quirks
  - require modifiers
  - `no_std`
  - docs

`hbm-gralloc`

  - aidl codegen
  - impl
