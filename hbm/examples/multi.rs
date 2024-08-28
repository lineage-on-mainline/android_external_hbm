use drm_fourcc::DrmFourcc;
use hbm::{Flags, Format, MemoryType, Usage};

#[cfg(feature = "drm")]
fn main() {
    env_logger::init();

    let drm = hbm::drm_kms::Builder::new()
        .node_path("/dev/dri/card0")
        .build()
        .unwrap();
    let vk = hbm::vulkan::Builder::new().build().unwrap();

    let dev = hbm::Builder::new()
        .add_backend(drm)
        .add_backend(vk)
        .build()
        .unwrap();

    let bo_desc = hbm::Description::new()
        .flags(Flags::EXTERNAL)
        .format(Format(DrmFourcc::Xrgb8888 as u32));
    let bo_usage = [
        Usage::DrmKms(hbm::drm_kms::Usage::PRIMARY),
        Usage::Vulkan(hbm::vulkan::Usage::COLOR),
    ];
    let bo_class = dev.classify(bo_desc, &bo_usage).unwrap();

    let bo_extent = hbm::Extent::Image(256, 256);
    let mut bo = hbm::Bo::with_constraint(dev.clone(), &bo_class, bo_extent, None).unwrap();
    bo.bind_memory(MemoryType::empty(), None).unwrap();
}

#[cfg(not(feature = "drm"))]
fn main() {
    println!("drm feature disabled");
}
