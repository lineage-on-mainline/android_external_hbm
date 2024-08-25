/*
 * Copyright 2024 Google LLC
 * SPDX-License-Identifier: MIT
 */

#include "hbm_minigbm.h"

#include <drm_fourcc.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/sysmacros.h>
#include <unistd.h>

static void
die(const char *msg)
{
    fprintf(stderr, "%s\n", msg);
    abort();
}

static void
test_memory_types(struct hbm_bo *bo)
{
    uint32_t mt_count = hbm_bo_memory_types(bo, 0, NULL);
    uint32_t *mt_flags = malloc(sizeof(*mt_flags) * mt_count);
    if (!mt_flags)
        die("failed to alloc mt flags");
    mt_count = hbm_bo_memory_types(bo, mt_count, mt_flags);

    bool has_mappable = false;
    for (uint32_t i = 0; i < mt_count; i++) {
        if (mt_flags[i] & HBM_MEMORY_FLAG_MAPPABLE) {
            has_mappable = true;
            break;
        }
    }
    if (!has_mappable)
        die("failed mappable mt");

    free(mt_flags);
}

static void
test_image_copy(struct hbm_bo *img_bo, struct hbm_bo *buf_bo, uint32_t width, uint32_t height)
{
    const struct hbm_copy_buffer_image copy = {
        .stride = width,
        .width = width,
        .height = height,
    };
    if (!hbm_bo_copy_buffer_image(buf_bo, img_bo, &copy, -1, NULL))
        die("failed to copy image to buffer");

    void *buf_ptr = hbm_bo_map(buf_bo);
    if (!buf_ptr)
        die("failed to map buffer");

    hbm_bo_invalidate(buf_bo);

    for (uint32_t y = 0; y < height; y++) {
        for (uint32_t x = 0; x < width; x++) {
            if (((const char *)buf_ptr)[width * y + x] != (char)(x * y))
                die("image-to-buffer copy has wrong values");
        }
    }

    hbm_bo_unmap(buf_bo);

    if (!hbm_bo_copy_buffer_image(img_bo, buf_bo, &copy, -1, NULL))
        die("failed to copy buffer to image");

    void *img_ptr = hbm_bo_map(img_bo);
    if (!img_ptr)
        die("failed to map image");

    hbm_bo_invalidate(img_bo);

    for (uint32_t y = 0; y < height; y++) {
        for (uint32_t x = 0; x < width; x++) {
            if (((const char *)img_ptr)[width * y + x] != (char)(x * y))
                die("buffer-to-image copy has wrong values");
        }
    }

    hbm_bo_unmap(img_bo);
}

static void
test_image_map(struct hbm_bo *img_bo, uint32_t width, uint32_t height, uint64_t stride, bool write)
{
    void *img_ptr = hbm_bo_map(img_bo);
    if (!img_ptr)
        die("failed to map image");

    if (write) {
        for (uint32_t y = 0; y < height; y++) {
            for (uint32_t x = 0; x < width; x++) {
                ((char *)img_ptr)[stride * y + x] = (char)(x * y);
            }
        }
    } else {
        for (uint32_t y = 0; y < height; y++) {
            for (uint32_t x = 0; x < width; x++) {
                if (((const char *)img_ptr)[stride * y + x] != (char)(x * y))
                    die("image readback has wrong values");
            }
        }
    }

    hbm_bo_flush(img_bo);
    hbm_bo_invalidate(img_bo);

    hbm_bo_unmap(img_bo);
}

static void
test_image(struct hbm_device *dev)
{
    const struct hbm_description img_desc = {
        .flags = HBM_RESOURCE_FLAG_MAP | HBM_RESOURCE_FLAG_COPY,
        .format = DRM_FORMAT_R8,
        .modifier = DRM_FORMAT_MOD_LINEAR,
    };

    const int mod_count = hbm_device_get_modifiers(dev, &img_desc, 0, NULL);
    if (mod_count < 0)
        die("failed to get image modifiers");
    if (mod_count > 0) {
        uint64_t *mods = malloc(sizeof(*mods) * mod_count);
        if (!mods)
            die("failed to allocate modifiers");

        if (hbm_device_get_modifiers(dev, &img_desc, mod_count, mods) != mod_count)
            die("unexpected image modifier count change");

        if (img_desc.modifier != DRM_FORMAT_MOD_INVALID) {
            if (mod_count != 1 || mods[0] != img_desc.modifier)
                die("unexpected image modifier");

            /* R8 / MOD_LINEAR has 1 plane */
            if (hbm_device_get_plane_count(dev, img_desc.format, img_desc.modifier) != 1)
                die("unexpected plane count");
        }

        free(mods);
    }
    if (!hbm_device_supports_modifier(dev, &img_desc, img_desc.modifier))
        die("unexpected missing modifier support");

    const union hbm_extent img_extent = {
        .image = {
            .width = 13,
            .height = 31,
        },
    };
    struct hbm_bo *img_bo = hbm_bo_create_with_constraint(dev, &img_desc, &img_extent, NULL);
    if (!img_bo)
        die("failed to create image bo");
    test_memory_types(img_bo);
    if (!hbm_bo_bind_memory(img_bo, HBM_MEMORY_FLAG_MAPPABLE, -1))
        die("failed to bind image bo");

    int img_dmabuf = hbm_bo_export_dma_buf(img_bo, "test image");
    if (img_dmabuf < 0)
        die("failed to export image dma-buf");

    struct hbm_layout img_layout;
    if (!hbm_bo_layout(img_bo, &img_layout))
        die("failed to get image layout");

    test_image_map(img_bo, img_extent.image.width, img_extent.image.height, img_layout.strides[0],
                   true);

    hbm_bo_destroy(img_bo);

    img_bo = hbm_bo_create_with_layout(dev, &img_desc, &img_extent, &img_layout, img_dmabuf);
    if (!img_bo)
        die("failed to create image bo with layout");
    test_memory_types(img_bo);
    if (!hbm_bo_bind_memory(img_bo, HBM_MEMORY_FLAG_MAPPABLE, img_dmabuf))
        die("failed to import image dma-buf");

    test_image_map(img_bo, img_extent.image.width, img_extent.image.height, img_layout.strides[0],
                   false);

    {
        const struct hbm_description tmp_desc = {
            .flags = HBM_RESOURCE_FLAG_MAP | HBM_RESOURCE_FLAG_COPY,
            .format = DRM_FORMAT_INVALID,
            .modifier = DRM_FORMAT_MOD_INVALID,
        };
        const union hbm_extent tmp_extent = {
            .buffer = {
                .size = img_extent.image.width * img_extent.image.height,
            },
        };
        struct hbm_bo *tmp_bo = hbm_bo_create_with_constraint(dev, &tmp_desc, &tmp_extent, NULL);
        if (!tmp_bo)
            die("failed to create temp bo");
        test_memory_types(tmp_bo);
        if (!hbm_bo_bind_memory(tmp_bo, HBM_MEMORY_FLAG_MAPPABLE, -1))
            die("failed to bind temp bo");

        test_image_copy(img_bo, tmp_bo, img_extent.image.width, img_extent.image.height);
        hbm_bo_destroy(tmp_bo);
    }

    hbm_bo_destroy(img_bo);
}

static void
test_buffer_copy(struct hbm_bo *buf_bo, struct hbm_bo *buf_dst, uint64_t buf_size)
{
    const struct hbm_copy_buffer copy = {
        .size = buf_size,
    };
    if (!hbm_bo_copy_buffer(buf_dst, buf_bo, &copy, -1, NULL))
        die("failed to copy buffer");

    void *buf_ptr = hbm_bo_map(buf_bo);
    if (!buf_ptr)
        die("failed to map buffer");

    hbm_bo_invalidate(buf_bo);

    for (uint64_t i = 0; i < buf_size; i++) {
        if (((const char *)buf_ptr)[i] != (char)i) {
            die("buffer copy has wrong values");
        }
    }

    hbm_bo_unmap(buf_bo);
}

static void
test_buffer_map(struct hbm_bo *buf_bo, uint64_t buf_size, bool write)
{
    void *buf_ptr = hbm_bo_map(buf_bo);
    if (!buf_ptr)
        die("failed to map buffer");

    if (write) {
        for (uint64_t i = 0; i < buf_size; i++) {
            ((char *)buf_ptr)[i] = (char)i;
        }
    } else {
        for (uint64_t i = 0; i < buf_size; i++) {
            if (((const char *)buf_ptr)[i] != (char)i) {
                die("buffer readback has wrong values");
            }
        }
    }

    hbm_bo_flush(buf_bo);
    hbm_bo_invalidate(buf_bo);

    hbm_bo_unmap(buf_bo);
}

static void
test_buffer(struct hbm_device *dev)
{
    const struct hbm_description buf_desc = {
        .flags = HBM_RESOURCE_FLAG_MAP | HBM_RESOURCE_FLAG_COPY,
        .format = DRM_FORMAT_INVALID,
        .modifier = DRM_FORMAT_MOD_INVALID,
    };

    if (hbm_device_get_modifiers(dev, &buf_desc, 0, NULL) != 0)
        die("unexpeted buffer modifiers");

    const union hbm_extent buf_extent = {
        .buffer = {
            .size = 13,
        },
    };
    struct hbm_bo *buf_bo = hbm_bo_create_with_constraint(dev, &buf_desc, &buf_extent, NULL);
    if (!buf_bo)
        die("failed to create buffer bo");
    test_memory_types(buf_bo);
    if (!hbm_bo_bind_memory(buf_bo, HBM_MEMORY_FLAG_MAPPABLE, -1))
        die("failed to bind buffer bo");

    int buf_dmabuf = hbm_bo_export_dma_buf(buf_bo, "test buffer");
    if (buf_dmabuf < 0)
        die("failed to export buffer dma-buf");

    struct hbm_layout buf_layout;
    if (!hbm_bo_layout(buf_bo, &buf_layout))
        die("failed to get buffer layout");

    test_buffer_map(buf_bo, buf_extent.buffer.size, true);

    hbm_bo_destroy(buf_bo);

    buf_bo = hbm_bo_create_with_layout(dev, &buf_desc, &buf_extent, &buf_layout, buf_dmabuf);
    if (!buf_bo)
        die("failed to create buffer bo with layout");
    test_memory_types(buf_bo);
    if (!hbm_bo_bind_memory(buf_bo, HBM_MEMORY_FLAG_MAPPABLE, buf_dmabuf))
        die("failed to import buffer dma-buf");

    test_buffer_map(buf_bo, buf_extent.buffer.size, false);

    {
        struct hbm_bo *tmp_bo = hbm_bo_create_with_constraint(dev, &buf_desc, &buf_extent, NULL);
        if (!tmp_bo)
            die("failed to create temp bo");
        test_memory_types(tmp_bo);
        if (!hbm_bo_bind_memory(tmp_bo, HBM_MEMORY_FLAG_MAPPABLE, -1))
            die("failed to bind temp bo");

        test_buffer_copy(buf_bo, tmp_bo, buf_extent.buffer.size);
        hbm_bo_destroy(tmp_bo);
    }

    hbm_bo_destroy(buf_bo);
}

static void
test_log(enum hbm_log_level lv, const char *msg, void *data)
{
    printf("hbm: %s\n", msg);
}

int
main(void)
{
    const dev_t dev_id = makedev(226, 128);

    hbm_log_init(HBM_LOG_LEVEL_DEBUG, test_log, NULL);

    struct hbm_device *dev = hbm_device_create(dev_id, false);
    if (!dev)
        die("failed to create device");

    test_buffer(dev);
    test_image(dev);

    hbm_device_destroy(dev);

    return 0;
}
