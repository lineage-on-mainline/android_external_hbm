use drm_fourcc::DrmFourcc;
use hbm::{Flags, Format, Usage};
use std::slice;
use std::sync::Arc;

fn test_image(dev: Arc<hbm::Device>) {
    let img_desc = hbm::Description::new()
        .flags(Flags::MAP | Flags::COPY)
        .format(Format::new(DrmFourcc::Argb8888 as u32));
    let img_usage = Usage::Vulkan(hbm::vulkan::Usage::empty());
    let img_class = dev.classify(img_desc, slice::from_ref(&img_usage)).unwrap();

    let img_width = 63;
    let img_height = 63;
    let mut img_bo = hbm::Bo::new(
        dev.clone(),
        &img_class,
        hbm::Extent::new_2d(img_width, img_height),
        None,
    )
    .unwrap();

    let img_dmabuf = img_bo.export_dma_buf(Some("img")).unwrap();
    let img_layout = img_bo.layout().unwrap();
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

    let _ = hbm::Bo::with_dma_buf(
        dev.clone(),
        &img_class,
        hbm::Extent::new_2d(img_width, img_height),
        img_dmabuf,
        img_layout,
    )
    .unwrap();

    img_bo.map().unwrap();
    img_bo.flush().unwrap();
    img_bo.invalidate().unwrap();
    img_bo.unmap();

    let img_copy = hbm::CopyBufferImage {
        offset: 0,
        stride: 0,
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
    let buf_bo =
        hbm::Bo::new(dev.clone(), &buf_class, hbm::Extent::new_1d(buf_size), None).unwrap();

    buf_bo.copy_buffer_image(&img_bo, img_copy, None).unwrap();
    img_bo.copy_buffer_image(&buf_bo, img_copy, None).unwrap();
}

fn test_buffer(dev: Arc<hbm::Device>) {
    let buf_desc = hbm::Description::new().flags(Flags::MAP | Flags::COPY);
    let buf_usage = Usage::Vulkan(hbm::vulkan::Usage::empty());
    let buf_class = dev.classify(buf_desc, slice::from_ref(&buf_usage)).unwrap();

    let buf_size = 13;
    let mut buf_bo =
        hbm::Bo::new(dev.clone(), &buf_class, hbm::Extent::new_1d(buf_size), None).unwrap();

    let buf_dmabuf = buf_bo.export_dma_buf(Some("buf")).unwrap();
    let buf_layout = buf_bo.layout().unwrap();
    println!("buf size {} alloc {}", buf_size, buf_layout.size);

    let _ = hbm::Bo::with_dma_buf(
        dev.clone(),
        &buf_class,
        hbm::Extent::new_1d(buf_size),
        buf_dmabuf,
        buf_layout,
    )
    .unwrap();

    buf_bo.map().unwrap();
    buf_bo.flush().unwrap();
    buf_bo.invalidate().unwrap();
    buf_bo.unmap();

    let buf_copy = hbm::CopyBuffer {
        src_offset: 0,
        dst_offset: 0,
        size: buf_size,
    };
    let buf_src =
        hbm::Bo::new(dev.clone(), &buf_class, hbm::Extent::new_1d(buf_size), None).unwrap();
    buf_bo.copy_buffer(&buf_src, buf_copy, None).unwrap();
}

fn main() {
    env_logger::init();

    let backend = hbm::vulkan::Builder::new().build().unwrap();
    let dev = hbm::Builder::new().add_backend(backend).build().unwrap();

    test_buffer(dev.clone());
    test_image(dev.clone());
}
