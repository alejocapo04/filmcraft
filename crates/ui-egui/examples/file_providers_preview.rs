//! Render FilmCraft's real Home/Import page offscreen with synthetic Recent projects.
//!
//! `cargo run -p filmcraft-ui-egui --example file_providers_preview -- --provider-preview --out target/provider-recents.png`
//! Omit `--provider-preview` to check the default local-only view. The example registers no
//! browser API, contacts no storage service and does not read or save any project files.

use std::path::PathBuf;

use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::file_providers::Resource;
use filmcraft_ui_egui::state::Mode;

fn render() -> Result<(), String> {
    let mut provider = false;
    let mut output = PathBuf::from("target/provider-recents.png");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--provider-preview" => provider = true,
            "--out" => output = args.next().map(PathBuf::from).ok_or("--out needs a path")?,
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    let mut app = FilmcraftApp::new(Session::default());
    app.ui.mode = Mode::Import;
    app.session.prefs.general.recent_projects = vec!["Device edit.fcproj".into()];
    if provider {
        let host = &app.hooks.file_providers;
        host.set_session("00112233445566778899aabbccddeeff".into())?;
        let connection = host.connect("preview".into(), "External files".into(), "example-account".into())?;
        host.set_resources(&connection, vec![Resource { id: "edit".into(), name: "Provider edit.fcbundle".into(), updated_at: "2026-10-31 12:00".into() }])?;
    }
    let mut harness =
        egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).with_pixels_per_point(1.0).wgpu().build_eframe(move |_creation| app);
    for _ in 0..12 {
        harness.step();
    }
    harness.render().map_err(|error| error.to_string())?.save(&output).map_err(|error| error.to_string())?;
    println!("wrote {} (provider preview: {provider})", output.display());
    Ok(())
}

fn main() {
    if let Err(error) = render() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
