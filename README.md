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

  - multi-backend
    - e.g., drm-kms adds alignment and modifier constraints, vulkan decides
      the layout, dma-heap allocates dma-buf, and vulkan again maps/copies
  - more backends, such as vaapi, v4l2, libcamera, etc.
  - policy backend
    - collect constraints and generate a policy offline
    - load constraints from the policy at runtime
    - this is useful when sandboxed or for non-queryable constraints
  - require modifiers
  - `no_std`
  - docs

`hbm-gralloc`

  - aidl codegen
  - impl
