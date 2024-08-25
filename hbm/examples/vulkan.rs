use drm_fourcc::DrmFourcc;
use hbm::{Flags, Format, MemoryType, Usage};
use std::slice;
use std::sync::Arc;

fn test_image(dev: Arc<hbm::Device>) {
    let img_desc = hbm::Description::new()
        .flags(Flags::EXTERNAL | Flags::MAP | Flags::COPY)
        .format(Format::new(DrmFourcc::Argb8888 as u32));
    let img_usage = Usage::Vulkan(hbm::vulkan::Usage::empty());
    let img_class = dev.classify(img_desc, slice::from_ref(&img_usage)).unwrap();

    let img_width = 63;
    let img_height = 63;
    let mut img_bo = hbm::Bo::with_constraint(
        dev.clone(),
        &img_class,
        hbm::Extent::Image(img_width, img_height),
        None,
    )
    .unwrap();
    img_bo.bind_memory(MemoryType::MAPPABLE, None).unwrap();

    let img_dmabuf = img_bo.export_dma_buf(Some("img")).unwrap();
    let img_layout = img_bo.layout();
    println!(
        "img size {}x{} alloc {} format {} modifier 0x{:x}",
        img_width, img_height, img_layout.size, img_desc.format, img_layout.modifier.0,
    );
    for plane in 0..(img_layout.plane_count as usize) {
        println!(
            "  plane {}: offset {} stride {}",
            plane, img_layout.offsets[plane], img_layout.strides[plane]
        );
    }

    let mut img_bo2 = hbm::Bo::with_layout(
        dev.clone(),
        &img_class,
        hbm::Extent::Image(img_width, img_height),
        img_layout,
        None,
    )
    .unwrap();
    img_bo2
        .bind_memory(MemoryType::MAPPABLE, Some(img_dmabuf))
        .unwrap();

    img_bo.map().unwrap();
    img_bo.flush();
    img_bo.invalidate();
    img_bo.unmap();

    let img_copy = hbm::CopyBufferImage {
        offset: 0,
        stride: (img_width * 4) as _,
        plane: 0,
        x: 0,
        y: 0,
        width: img_width,
        height: img_height,
    };

    let buf_desc = hbm::Description::new().flags(Flags::MAP | Flags::COPY);
    let buf_usage = Usage::Vulkan(hbm::vulkan::Usage::empty());
    let buf_class = dev.classify(buf_desc, slice::from_ref(&buf_usage)).unwrap();
    let buf_size = (img_width * img_height * 4) as u64;
    let mut buf_bo =
        hbm::Bo::with_constraint(dev.clone(), &buf_class, hbm::Extent::Buffer(buf_size), None)
            .unwrap();
    buf_bo.bind_memory(MemoryType::MAPPABLE, None).unwrap();

    buf_bo
        .copy_buffer_image(&img_bo, img_copy, None, true)
        .unwrap();
    img_bo
        .copy_buffer_image(&buf_bo, img_copy, None, true)
        .unwrap();
}

fn test_buffer(dev: Arc<hbm::Device>) {
    let buf_desc = hbm::Description::new().flags(Flags::EXTERNAL | Flags::MAP | Flags::COPY);
    let buf_usage = Usage::Vulkan(hbm::vulkan::Usage::empty());
    let buf_class = dev.classify(buf_desc, slice::from_ref(&buf_usage)).unwrap();

    let buf_size = 13;
    let mut buf_bo =
        hbm::Bo::with_constraint(dev.clone(), &buf_class, hbm::Extent::Buffer(buf_size), None)
            .unwrap();
    buf_bo.bind_memory(MemoryType::MAPPABLE, None).unwrap();

    let buf_dmabuf = buf_bo.export_dma_buf(Some("buf")).unwrap();
    let buf_layout = buf_bo.layout();
    println!("buf size {} alloc {}", buf_size, buf_layout.size);

    let mut buf_bo2 = hbm::Bo::with_layout(
        dev.clone(),
        &buf_class,
        hbm::Extent::Buffer(buf_size),
        buf_layout,
        None,
    )
    .unwrap();
    buf_bo2
        .bind_memory(MemoryType::MAPPABLE, Some(buf_dmabuf))
        .unwrap();

    buf_bo.map().unwrap();
    buf_bo.flush();
    buf_bo.invalidate();
    buf_bo.unmap();

    let buf_copy = hbm::CopyBuffer {
        src_offset: 0,
        dst_offset: 0,
        size: buf_size,
    };
    let mut buf_src =
        hbm::Bo::with_constraint(dev.clone(), &buf_class, hbm::Extent::Buffer(buf_size), None)
            .unwrap();
    buf_src.bind_memory(MemoryType::MAPPABLE, None).unwrap();

    buf_bo.copy_buffer(&buf_src, buf_copy, None, true).unwrap();
}

fn main() {
    env_logger::init();

    let backend = hbm::vulkan::Builder::new().build().unwrap();
    let dev = hbm::Builder::new().add_backend(backend).build().unwrap();

    test_buffer(dev.clone());
    test_image(dev.clone());
}
