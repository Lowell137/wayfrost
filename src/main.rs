use libadwaita as adw;
use libadwaita::prelude::*;
use wayfrost::{constants, overlay};

fn main() {
    env_logger::init();
    log::info!("Starting Wayfrost...");

    std::thread::spawn(|| {
        wayfrost::ocr::pipeline::init_global_ocr();
    });

    let app = adw::Application::builder()
        .application_id(constants::APP_ID)
        .build();

    app.connect_activate(move |app| {
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        overlay::build_overlay_window(app);
    });

    app.run();
}
