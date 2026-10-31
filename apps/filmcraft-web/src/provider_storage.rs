//! Browser media is frozen with the project, then copied asynchronously into a bounded envelope.
use filmcraft_engine::{Session, project::MediaRef};
use filmcraft_ui_egui::{
    FilmcraftApp,
    file_providers::{Capture, Command},
    provider_bundle::{self, Bundle, MAX_BYTES},
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use wasm_bindgen_futures::JsFuture;

fn plan(session: &Session) -> Result<(Vec<u8>, Vec<(String, web_sys::Blob)>), String> {
    let mut project = (*session.project).clone();
    let mut files = BTreeSet::new();
    for item in project.items.values_mut() {
        if let Some(media) = item.as_media_mut() {
            if let MediaRef::File { path } = &mut media.media {
                *path = session.media.follow(path).map_err(|e| format!("Cannot include media: {e}"))?;
                files.insert(path.clone());
                if media.info.kind == filmcraft_media::MediaKind::ImageSequence {
                    files.extend(
                        filmcraft_engine::media_pool::image_sequence_frames(path, &*session.services).map_err(|e| e.to_string())?.into_iter().flatten(),
                    );
                }
            }
            if let Some(MediaRef::File { path }) = &mut media.proxy {
                *path = session.media.follow(path).map_err(|e| format!("Cannot include proxy: {e}"))?;
                files.insert(path.clone());
            }
        }
    }
    if files.len() > 10_000 {
        return Err("Too many provider media files".into());
    }
    let project = filmcraft_format::encode_with_view(&project, Some(&session.project_view()), false);
    if project.len() > 16 * 1024 * 1024 {
        return Err("Provider project metadata exceeds 16 MiB".into());
    }
    if project.is_empty() {
        return Err("Project serialization failed".into());
    }
    let mut frozen = Vec::new();
    let mut total = project.len();
    for path in files {
        let blob = crate::fs::blob(&path).ok_or_else(|| format!("Cannot include missing media {path}"))?;
        if blob.size() > MAX_BYTES.saturating_sub(total) as f64 {
            return Err("Project and media exceed 80 MiB".into());
        }
        total = total.saturating_add(blob.size() as usize);
        frozen.push((path, blob));
    }
    Ok((project, frozen))
}

async fn capture(project: Vec<u8>, files: Vec<(String, web_sys::Blob)>) -> Result<Vec<u8>, String> {
    let mut assets = BTreeMap::new();
    let mut total = project.len();
    for (path, blob) in files {
        if blob.size() > MAX_BYTES.saturating_sub(total) as f64 {
            return Err("Project and media exceed 80 MiB".into());
        }
        let array = JsFuture::from(blob.array_buffer()).await.map_err(|e| format!("Cannot read {path}: {e:?}"))?;
        let data = js_sys::Uint8Array::new(&array).to_vec();
        total = total.saturating_add(data.len());
        if total > MAX_BYTES {
            return Err("Project and media exceed 80 MiB".into());
        }
        assets.insert(path, data);
    }
    Bundle { project, assets }.encode()
}

fn open(app: &mut FilmcraftApp, request: &str, bytes: &[u8]) -> Result<(), String> {
    if app.session.is_dirty() || app.hooks.file_providers.pending_for(filmcraft_ui_egui::provider_ui::document_id(app)) {
        return Err("Save the current project, or close it explicitly, before opening a provider project".into());
    }
    if !app.session.jobs.is_empty() {
        return Err("Finish the running media jobs before replacing the provider project".into());
    }
    let bundle = Bundle::decode(bytes)?;
    let loaded = filmcraft_format::decode(&bundle.project).map_err(|e| e.to_string())?;
    for item in loaded.project.items.values() {
        if let Some(media) = item.as_media() {
            if let MediaRef::File { path } = &media.media {
                if !bundle.assets.contains_key(path) {
                    return Err(format!("Provider bundle is missing {path}"));
                }
            }
            if let Some(MediaRef::File { path }) = &media.proxy {
                if !bundle.assets.contains_key(path) {
                    return Err(format!("Provider bundle is missing proxy {path}"));
                }
            }
        }
    }
    let root = format!("/__file_provider/{request}");
    let mut replacements = BTreeMap::new();
    let mut staged = BTreeSet::new();
    for path in bundle.assets.keys() {
        let target = provider_bundle::stage_path(&root, path)?;
        if !staged.insert(target.clone()) {
            return Err("Provider media paths alias after normalization".into());
        }
        replacements.insert(path.clone(), target);
    }
    let mut value: Value = serde_json::from_slice(&bundle.project).map_err(|e| e.to_string())?;
    provider_bundle::rewrite(&mut value, &replacements);
    let native = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    // Validate before staging anything, and before the old project is replaced.
    let loaded = filmcraft_format::decode(&native).map_err(|e| e.to_string())?;
    for (path, data) in bundle.assets {
        if let Some(target) = replacements.get(&path) {
            crate::fs::put(target, data);
        }
    }
    app.session.replace_external_project(loaded);
    for target in replacements.values() {
        app.session.media.allow(target);
    }
    crate::fs::retain_provider_root(&root);
    app.ui.mode = filmcraft_ui_egui::state::Mode::Edit;
    Ok(())
}

pub fn tick(app: &mut FilmcraftApp) {
    let host = app.hooks.file_providers.clone();
    for command in host.take_commands() {
        match command {
            Command::Save { request, doc, destination, name, save_as } => {
                if doc != filmcraft_ui_egui::provider_ui::document_id(app) {
                    host.failed(&request, "The document was replaced before capture");
                    continue;
                }
                if destination == "device" {
                    let native_name = std::path::Path::new(&name).with_extension("fcproj").to_string_lossy().into_owned();
                    let bytes = filmcraft_format::encode_with_view(&app.session.project, Some(&app.session.project_view()), true);
                    let result = if bytes.is_empty() {
                        Err("Project serialization failed".into())
                    } else {
                        crate::fs::download(&native_name, &bytes).map_err(|e| format!("Download failed: {e:?}"))
                    };
                    match result {
                        Ok(()) => host.finish_device(&request),
                        Err(error) => {
                            host.failed(&request, &error);
                            app.ui.status = error;
                        }
                    }
                    continue;
                }
                let revision = app.session.revision;
                let (project, files) = match plan(&app.session) {
                    Ok(p) => p,
                    Err(error) => {
                        host.failed(&request, &error);
                        app.ui.status = error;
                        continue;
                    }
                };
                let host = host.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let bytes = capture(project, files).await;
                    crate::post(move |web, _ctx| {
                        if doc != filmcraft_ui_egui::provider_ui::document_id(&web.app) {
                            host.failed(&request, "The document was replaced during capture");
                            return;
                        }
                        let result = bytes.and_then(|bytes| {
                            if destination == "device" {
                                crate::fs::download(&name, &bytes).map_err(|e| format!("Download failed: {e:?}"))?;
                                host.finish_device(&request);
                            } else {
                                host.capture(&request, Capture { doc, revision, name, save_as }, bytes)?;
                            }
                            Ok(())
                        });
                        if let Err(error) = result {
                            host.failed(&request, &error);
                            web.app.ui.status = error;
                        }
                    });
                });
            }
            Command::Result { capture, resource, status } => {
                if status == "saved" && capture.doc == filmcraft_ui_egui::provider_ui::document_id(app) {
                    host.bind(&capture, resource);
                    app.session.saved_revision = capture.revision;
                }
            }
            Command::Open { request, resource: _, name, bytes } => match open(app, &request, &bytes) {
                Ok(()) => host.opened(
                    &request,
                    Some(Capture { doc: filmcraft_ui_egui::provider_ui::document_id(app), revision: app.session.revision, name, save_as: false }),
                    "",
                ),
                Err(error) => {
                    host.opened(&request, None, &error);
                    app.ui.status = error;
                }
            },
        }
    }
    filmcraft_ui_egui::provider_ui::publish(app);
    crate::file_providers::publish(&host);
}

/// Open a downloaded transport bundle without treating its file name as a provider resource.
pub fn open_device(app: &mut FilmcraftApp, bytes: &[u8]) -> Result<(), String> {
    let root = format!("device-{:016x}-{:016x}", js_sys::Date::now().to_bits(), js_sys::Math::random().to_bits());
    open(app, &root, bytes)
}
