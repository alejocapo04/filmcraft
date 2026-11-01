//! UI integration for the optional external file capability.
use crate::FilmcraftApp;
use serde_json::{Value, json};

pub fn document_id(app: &FilmcraftApp) -> u64 {
    app.session.document_epoch
}
pub fn choose(app: &mut FilmcraftApp, id: &str) -> Option<Result<Value, String>> {
    let host = app.hooks.file_providers.clone();
    let doc = document_id(app);
    if !host.enabled() {
        if matches!(id, "file.save" | "file.saveAs") && (host.bound(doc) || host.pending_for(doc)) {
            return Some(Err("Re-enable the provider before saving this linked or pending project; use Save a Copy for a device download".into()));
        }
        return None;
    }
    if matches!(
        id,
        "file.newProject" | "file.open" | "file.openRecent" | "file.closeProject" | "file.revert" | "file.openDemoProject" | "templates.create" | "app.quit"
    ) && host.busy()
    {
        return Some(Err("Finish or cancel the pending provider operation before replacing the project".into()));
    }
    if !matches!(id, "file.save" | "file.saveAs") {
        return None;
    }
    let name = host.filename(doc).unwrap_or_else(|| suggested_name(&app.session.project.name, "fcbundle"));
    Some(host.choose(doc, app.session.revision, name, id == "file.saveAs").map(|_| json!({"pending":"fileProvider"})))
}

/// Check the live editor immediately before provider decoding or media staging.
/// The read itself is busy, so only saves for this document block its replacement here.
pub fn check_open(app: &FilmcraftApp, ctx: Option<&egui::Context>) -> Result<(), String> {
    let draft = app.dialog.is_some()
        || app.native_dialog.is_some()
        || app.overwrite_prompt.is_some()
        || app.ui.clip_dialog.is_some()
        || app.ui.gfx_edit.is_some()
        || !app.ui.pen_points.is_empty()
        || app.ui.mask_pen.is_some()
        || app.ui.link_media.is_some()
        || app.ui.create_proxies.is_some()
        || app.ui.project_manager.is_some()
        || app.ui.make_offline.is_some()
        || app.ui.color_dialog.is_some()
        || app.ui.save_preset.is_some()
        || !app.ui.audio_fx_editors.is_empty()
        || app.ui.sync_dialog.is_some()
        || app.ui.edit_cameras.is_some()
        || app.ui.guide_dialog.is_some()
        || app.ui.marker_dialog.is_some()
        || app.ui.text_props_dialog.is_some()
        || app.ui.workspace_dialog.is_some()
        || app.ui.extras.dialog.is_some()
        || app.ui.gfx_templates.export.is_some()
        || app.ui.gfx_templates.replace.is_some()
        || app.ui.project_panel.rename.is_some()
        || app.ui.project_panel.dialog.is_some();
    let interaction = app.tl.drag.is_some()
        || app.tl.pressed.is_some()
        || app.value_press.is_some()
        || app.pending_timeline_drop.is_some()
        || app.ime_composition.is_some()
        || !app.synthetic.is_empty()
        || app.synthetic_pending_step.is_some()
        || ctx.is_some_and(|ctx| ctx.input(|input| !input.events.is_empty() || !input.raw.dropped_files.is_empty() || input.pointer.any_down()));
    let jobs = !app.session.jobs.is_empty()
        || !app.session.media_jobs.is_empty()
        || !app.session.mask_jobs.is_empty()
        || !app.session.scene_jobs.is_empty()
        || app.session.voiceover.recording()
        || app.session.mixrec.active()
        || app.session.mcrec.active()
        || app.session.trim_play.active();
    if app.session.is_dirty() || app.hooks.file_providers.pending_for(document_id(app)) || draft || interaction || jobs {
        return Err("Save changes and finish pending editor work before opening a provider project".into());
    }
    Ok(())
}

pub fn publish(app: &FilmcraftApp) {
    let host = &app.hooks.file_providers;
    if !host.connected() {
        return;
    }
    let doc = document_id(app);
    host.publish_documents(json!([{"id":doc.to_string(),"revision":app.session.revision,"name":app.session.project.name,"dirty":app.session.is_dirty(),"provider":host.bound(doc)}]),&[doc]);
}

/// Provider recent files are separate from device paths and remain account-scoped.
pub fn recent(app: &FilmcraftApp, ui: &mut egui::Ui) {
    app.hooks.file_providers.recent_ui(ui);
}

/// Safe suggested filename; project names may contain path separators or be long UTF-8.
pub fn suggested_name(stem: &str, extension: &str) -> String {
    let mut name = String::new();
    for c in stem.chars() {
        if name.len() + c.len_utf8() > 230 {
            break;
        }
        name.push(if c.is_control() || matches!(c, '/' | '\\') { '-' } else { c });
    }
    if name.trim().is_empty() {
        name = "Untitled".into();
    }
    format!("{name}.{extension}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    #[test]
    fn suggestion_is_bounded_utf8_and_not_a_device_path() {
        let name = suggested_name(&"á/\\nube".repeat(100), "fcbundle");
        assert!(name.len() <= 255);
        assert!(!name.contains(['/', '\\']));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod provider_routing_tests {
    use super::*;
    #[test]
    fn default_off_keeps_original_save_and_registration_only_emits_a_choice() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        assert!(choose(&mut app, "file.save").is_none());
        let host = app.hooks.file_providers.clone();
        host.set_session("0123456789abcdef0123456789abcdef".into()).unwrap();
        host.connect("test".into(), "Provider".into(), "account".into()).unwrap();
        let before = app.session.revision;
        assert!(choose(&mut app, "file.save").unwrap().is_ok());
        let events = host.take_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].metadata["type"], "saveDialog");
        assert_eq!(app.session.revision, before, "a choice must not mutate document state");
    }

    #[test]
    fn disabled_linked_or_pending_save_cannot_fall_through_to_device_saving() {
        for linked in [false, true] {
            let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
            app.session.execute("file.newSequence", json!({"name":"Edited","width":16,"height":16})).unwrap();
            let host = app.hooks.file_providers.clone();
            host.set_session("0123456789abcdef0123456789abcdef".into()).unwrap();
            host.connect("test".into(), "Provider".into(), "account".into()).unwrap();
            let doc = document_id(&app);
            if linked {
                host.bind(&crate::file_providers::Capture { doc, revision: 1, name: "Linked.fcbundle".into(), save_as: false }, "linked".into());
            } else {
                host.choose(doc, app.session.revision, "Pending.fcbundle".into(), false).unwrap();
                host.take_events();
            }
            host.set_enabled(false).unwrap();
            let project = app.session.project.clone();
            let saved = app.session.saved_revision;
            let ctx = egui::Context::default();
            for command in ["file.save", "file.saveAs"] {
                let error = crate::menus::invoke(&mut app, &ctx, command, json!({"path":""})).unwrap_err();
                assert!(error.contains("Re-enable the provider"));
                assert!(app.session.is_dirty());
                assert_eq!(app.session.saved_revision, saved);
                assert!(std::sync::Arc::ptr_eq(&project, &app.session.project));
                assert!(app.session.path.is_none());
                assert_eq!(host.bound(doc), linked);
                assert_eq!(host.pending_for(doc), !linked);
                assert!(host.take_events().is_empty(), "rejected Save must deliver neither a chooser nor bytes");
            }
            host.set_enabled(true).unwrap();
            publish(&app);
            assert_eq!(host.snapshot()["documents"][0]["dirty"], true);
            assert_eq!(host.snapshot()["documents"][0]["provider"], linked);
        }
    }

    #[test]
    fn disabled_unbound_document_keeps_legacy_save_routing() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let host = app.hooks.file_providers.clone();
        host.set_session("0123456789abcdef0123456789abcdef".into()).unwrap();
        host.connect("test".into(), "Provider".into(), "account".into()).unwrap();
        host.set_enabled(false).unwrap();
        assert!(choose(&mut app, "file.save").is_none());
        assert!(choose(&mut app, "file.saveCopy").is_none());
        assert!(host.take_events().is_empty());
    }

    #[test]
    fn provider_read_rechecks_sequence_draft_started_after_request_without_rejecting_its_own_read() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.session.execute("file.newSequence", json!({"name":"A","width":16,"height":16})).unwrap();
        app.session.saved_revision = app.session.revision;
        let host = app.hooks.file_providers.clone();
        host.set_session("0123456789abcdef0123456789abcdef".into()).unwrap();
        let connection = host.connect("test".into(), "Provider".into(), "account".into()).unwrap();
        let doc = document_id(&app);
        host.bind(&crate::file_providers::Capture { doc, revision: app.session.revision, name: "A.fcbundle".into(), save_as: false }, "a".into());
        let resources = vec![crate::file_providers::Resource { id: "b".into(), name: "B.fcbundle".into(), updated_at: String::new() }];
        host.set_resources(&connection, resources).unwrap();
        let request = host.request_open("b").unwrap();
        host.take_events();
        let ctx = egui::Context::default();
        assert!(host.busy());
        assert!(check_open(&app, Some(&ctx)).is_ok(), "an idle editor must admit its pending read");
        crate::panels::sequence_settings::open(&mut app);
        app.ui.sequence_settings.width = 64;
        host.open(request.clone(), "B.fcbundle".into(), vec![]).unwrap();
        assert!(matches!(host.take_commands().as_slice(), [crate::file_providers::Command::Open { .. }]));
        let project = app.session.project.clone();
        let revision = app.session.revision;
        let resources = host.snapshot()["resources"].clone();
        let error = check_open(&app, Some(&ctx)).unwrap_err();
        host.opened(&request, None, &error);
        assert!(std::sync::Arc::ptr_eq(&project, &app.session.project));
        assert_eq!(document_id(&app), doc);
        assert_eq!(app.session.revision, revision);
        assert_eq!(app.ui.sequence_settings.width, 64);
        assert!(app.dialog.is_some());
        assert_eq!(host.filename(doc).as_deref(), Some("A.fcbundle"));
        assert_eq!(host.snapshot()["resources"], resources);
        let event = host.take_events().remove(0);
        assert_eq!(event.metadata["type"], "openFailed");
        assert_eq!(event.metadata["resourceId"], "b");
        assert!(event.bytes.is_none());
        app.dialog = None;
        assert!(check_open(&app, Some(&ctx)).is_ok());
    }

    #[test]
    fn provider_open_refuses_unconsumed_input_and_editor_gestures() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        assert!(check_open(&app, Some(&ctx)).is_ok());
        app.ui.pen_points.push([1.0, 2.0]);
        assert!(check_open(&app, Some(&ctx)).is_err());
        app.ui.pen_points.clear();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Text("pending edit".into()));
        let _ = ctx.run_ui(raw, |ui| assert!(check_open(&app, Some(ui.ctx())).is_err()));
    }
}
