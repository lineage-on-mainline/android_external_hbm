use drm_fourcc::{DrmFourcc, DrmModifier};
use hbm::{Flags, Format, Usage};
use std::slice;

#[cfg(feature = "drm")]
fn main() {
    env_logger::init();

    let backend = hbm::drm_kms::Builder::new()
        .node_path("/dev/dri/card0")
        .build()
        .unwrap();
    let dev = hbm::Builder::new().add_backend(backend).build().unwrap();

    let bo_desc = hbm::Description::new()
        .flags(Flags::MAP)
        .format(Format::new(DrmFourcc::Xrgb8888 as u32))
        .modifier(DrmModifier::Linear.into());
    let bo_usage = Usage::DrmKms(hbm::drm_kms::Usage::OVERLAY);
    let bo_class = dev.classify(bo_desc, slice::from_ref(&bo_usage)).unwrap();

    let bo_width = 63;
    let bo_height = 63;
    let mut bo = hbm::Bo::new(
        dev.clone(),
        &bo_class,
        hbm::Extent::new_2d(bo_width, bo_height),
        None,
    )
    .unwrap();

    let dmabuf = bo.export_dma_buf(Some("test")).unwrap();
    let layout = bo.layout().unwrap();
    println!(
        "bo size {}x{} alloc {} format {} modifier 0x{:x}",
        bo_width, bo_height, layout.size, bo_desc.format, layout.modifier.0,
    );
    for plane in 0..(layout.plane_count as usize) {
        println!(
            "  plane {}: offset {} stride {}",
            plane, layout.offsets[plane], layout.strides[plane]
        );
    }

    let _ = hbm::Bo::with_dma_buf(
        dev.clone(),
        &bo_class,
        hbm::Extent::new_2d(bo_width, bo_height),
        dmabuf,
        layout,
    )
    .unwrap();

    bo.map().unwrap();
    bo.flush().unwrap();
    bo.invalidate().unwrap();
    bo.unmap();
}

#[cfg(not(feature = "drm"))]
fn main() {
    println!("drm feature disabled");
}
