use drm_fourcc::DrmFourcc;
use hbm::{Format, Usage};

fn main() {
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

    let bo_desc = hbm::Description::new().format(Format::new(DrmFourcc::Xrgb8888 as u32));
    let bo_usage = [
        Usage::DrmKms(hbm::drm_kms::Usage::OVERLAY),
        Usage::Vulkan(hbm::vulkan::Usage::COLOR),
    ];
    let bo_class = dev.classify(bo_desc, &bo_usage).unwrap();

    let bo_extent = hbm::Extent::new_2d(256, 256);
    let _bo = hbm::Bo::new(dev.clone(), &bo_class, bo_extent, None).unwrap();
}
