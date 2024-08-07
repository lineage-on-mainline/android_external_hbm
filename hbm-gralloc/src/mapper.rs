// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

// TODO implement
// https://android.googlesource.com/platform/hardware/interfaces/+/refs/heads/main/graphics/mapper/stable-c/include/android/hardware/graphics/mapper/IMapper.h
//
// To generate the type definitions,
//
//   $ bindgen --no-doc-comments --no-layout-tests \
//       --default-enum-style rust \
//       --ctypes-prefix std::ffi \
//       --allowlist-type AIMapper \
//       <path-to-IMapper.h> -- \
//       -x c++ -include stddef.h -I<path-to-mesa-include-android_stub>

#[cfg(feature = "builtin-imapper-stablec-bindgen")]
mod builtin_imapper_stablec_bindgen;
#[cfg(feature = "builtin-imapper-stablec-bindgen")]
use builtin_imapper_stablec_bindgen as imapper_stablec_bindgen;

use imapper_stablec_bindgen::{
    buffer_handle_t, native_handle_t, AIMapper, AIMapperV5, AIMapper_BeginDumpBufferCallback,
    AIMapper_DumpBufferCallback, AIMapper_Error, AIMapper_MetadataType,
    AIMapper_MetadataTypeDescription, AIMapper_Version, ARect,
};

unsafe extern "C" fn import_buffer(
    _handle: *const native_handle_t,
    _out_buffer_handle: *mut buffer_handle_t,
) -> AIMapper_Error {
    // validate(handle);
    // buf = native_handle_clone(handle);
    // import(buf); // validate and setup buf->bo mapping
    // return buf;
    AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED
}

unsafe extern "C" fn free_buffer(_buffer: buffer_handle_t) -> AIMapper_Error {
    // bo = lookup(buf);
    // delete(bo);
    // native_handle_close(buf);
    // native_handle_delete(buf);
    AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED
}

unsafe extern "C" fn get_transport_size(
    buffer: buffer_handle_t,
    out_num_fds: *mut u32,
    out_num_ints: *mut u32,
) -> AIMapper_Error {
    let buf = &*buffer;
    *out_num_fds = buf.numFds as u32;
    *out_num_ints = buf.numInts as u32;
    AIMapper_Error::AIMAPPER_ERROR_NONE
}

unsafe extern "C" fn lock(
    _buffer: buffer_handle_t,
    _cpu_usage: u64,
    _access_region: ARect,
    _acquire_fence: std::ffi::c_int,
    _out_data: *mut *mut std::ffi::c_void,
) -> AIMapper_Error {
    // bo = lookup(buf);
    // wait(acquire_fence);
    // map(bo);
    // sync(bo, start);
    AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED
}

unsafe extern "C" fn unlock(
    _buffer: buffer_handle_t,
    release_fence: *mut std::ffi::c_int,
) -> AIMapper_Error {
    // bo = lookup(buf);
    // sync(bo, end);
    // unmap(bo);
    *release_fence = -1;
    AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED
}

unsafe extern "C" fn flush_locked_buffer(_buffer: buffer_handle_t) -> AIMapper_Error {
    // bo = lookup(buf);
    // flush(bo);
    AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED
}

unsafe extern "C" fn reread_locked_buffer(_buffer: buffer_handle_t) -> AIMapper_Error {
    // bo = lookup(buf);
    // invalidate(bo);
    AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED
}

unsafe extern "C" fn get_metadata(
    buffer: buffer_handle_t,
    metadata_type: AIMapper_MetadataType,
    dest_buffer: *mut std::ffi::c_void,
    dest_buffer_size: usize,
) -> i32 {
    let c_name = std::ffi::CStr::from_ptr(metadata_type.name);
    let name = c_name.to_str().unwrap();
    if name != "android.hardware.graphics.common.StandardMetadataType" {
        return AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED as i32;
    }

    get_standard_metadata(buffer, metadata_type.value, dest_buffer, dest_buffer_size)
}

unsafe extern "C" fn get_standard_metadata(
    _buffer: buffer_handle_t,
    _standard_metadata_type: i64,
    _dest_buffer: *mut std::ffi::c_void,
    _dest_buffer_size: usize,
) -> i32 {
    // bo = lookup(buf);
    // val = get_metadata(bo); // ro metadata are embedded and rw metadata are on shmem
    // encode(val, dest_buffer, dest_buffer_size);
    AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED as i32
}

unsafe extern "C" fn set_metadata(
    buffer: buffer_handle_t,
    metadata_type: AIMapper_MetadataType,
    metadata: *const std::ffi::c_void,
    metadata_size: usize,
) -> AIMapper_Error {
    let c_name = std::ffi::CStr::from_ptr(metadata_type.name);
    let name = c_name.to_str().unwrap();
    if name != "android.hardware.graphics.common.StandardMetadataType" {
        return AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED;
    }

    set_standard_metadata(buffer, metadata_type.value, metadata, metadata_size)
}

unsafe extern "C" fn set_standard_metadata(
    _buffer: buffer_handle_t,
    _standard_metadata_type: i64,
    _metadata: *const std::ffi::c_void,
    _metadata_size: usize,
) -> AIMapper_Error {
    // bo = lookup(buf);
    // val = decode(metadata, metadata_size);
    // set_metadata(bo, val); // ro metadata are embedded and rw metadata are on shmem
    AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED
}

unsafe extern "C" fn list_supported_metadata_types(
    out_description_list: *mut *const AIMapper_MetadataTypeDescription,
    out_number_of_descriptions: *mut usize,
) -> AIMapper_Error {
    // list std metadata
    *out_description_list = std::ptr::null();
    *out_number_of_descriptions = 0;
    AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED
}

unsafe extern "C" fn dump_buffer(
    _buffer: buffer_handle_t,
    _dump_buffer_callback: AIMapper_DumpBufferCallback,
    _context: *mut std::ffi::c_void,
) -> AIMapper_Error {
    // bo = lookup(buf);
    // for each metadata: dump_bufferCallback()
    AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED
}

unsafe extern "C" fn dump_all_buffers(
    _begin_dump_callback: AIMapper_BeginDumpBufferCallback,
    _dump_buffer_callback: AIMapper_DumpBufferCallback,
    _context: *mut std::ffi::c_void,
) -> AIMapper_Error {
    // for each buffer: dump_buffer(buffer);
    AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED
}

unsafe extern "C" fn get_reserved_region(
    _buffer: buffer_handle_t,
    _out_reserved_region: *mut *mut std::ffi::c_void,
    _out_reserved_size: *mut u64,
) -> AIMapper_Error {
    // bo = lookup(buf);
    // // shmem holds rw metadata as well as a region reserved for client
    // return mmap_shmem(bo);
    AIMapper_Error::AIMAPPER_ERROR_UNSUPPORTED
}

#[no_mangle]
pub unsafe extern "C" fn AIMapper_loadIMapper(
    out_implementation: *mut *mut AIMapper,
) -> AIMapper_Error {
    let mapper = Box::new(AIMapper {
        version: AIMapper_Version::AIMAPPER_VERSION_5,
        v5: AIMapperV5 {
            importBuffer: Some(import_buffer),
            freeBuffer: Some(free_buffer),
            getTransportSize: Some(get_transport_size),
            lock: Some(lock),
            unlock: Some(unlock),
            flushLockedBuffer: Some(flush_locked_buffer),
            rereadLockedBuffer: Some(reread_locked_buffer),
            getMetadata: Some(get_metadata),
            getStandardMetadata: Some(get_standard_metadata),
            setMetadata: Some(set_metadata),
            setStandardMetadata: Some(set_standard_metadata),
            listSupportedMetadataTypes: Some(list_supported_metadata_types),
            dumpBuffer: Some(dump_buffer),
            dumpAllBuffers: Some(dump_all_buffers),
            getReservedRegion: Some(get_reserved_region),
        },
    });

    *out_implementation = Box::into_raw(mapper);
    AIMapper_Error::AIMAPPER_ERROR_NONE
}
