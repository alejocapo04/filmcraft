//! Bounded, lossless transport for a project and the media it references.
//! This is a provider envelope, not a replacement for the native project format.
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_BYTES: usize = 80 * 1024 * 1024;
const MAX_PROJECT: usize = 16 * 1024 * 1024;
const MAX_FILES: usize = 10_000;
const MAGIC: &[u8; 8] = b"ACFPB001";

#[derive(Debug, PartialEq)]
pub struct Bundle {
    pub project: Vec<u8>,
    pub assets: BTreeMap<String, Vec<u8>>,
}

fn crc(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for b in bytes {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = if c & 1 == 0 { c >> 1 } else { 0xedb8_8320 ^ (c >> 1) };
        }
    }
    !c
}

fn valid_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 4096 && !path.chars().any(char::is_control)
}

impl Bundle {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if self.project.is_empty() || self.project.len() > MAX_PROJECT || self.assets.len() > MAX_FILES {
            return Err("provider bundle exceeds its project or asset limits".into());
        }
        let mut size = 20usize.saturating_add(self.project.len());
        for (path, bytes) in &self.assets {
            if !valid_path(path) {
                return Err("invalid provider asset path".into());
            }
            size = size.saturating_add(10).saturating_add(path.len()).saturating_add(bytes.len());
        }
        if size > MAX_BYTES {
            return Err("project and media exceed the 80 MiB provider limit".into());
        }
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.project.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.assets.len() as u32).to_le_bytes());
        out.extend_from_slice(&crc(&self.project).to_le_bytes());
        out.extend_from_slice(&self.project);
        for (path, bytes) in &self.assets {
            out.extend_from_slice(&(path.len() as u16).to_le_bytes());
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&crc(bytes).to_le_bytes());
            out.extend_from_slice(path.as_bytes());
            out.extend_from_slice(bytes);
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("provider bundle exceeds 80 MiB".into());
        }
        fn take<'a>(b: &mut &'a [u8], len: usize) -> Result<&'a [u8], String> {
            if b.len() < len {
                return Err("truncated provider bundle".into());
            }
            let (head, rest) = b.split_at(len);
            *b = rest;
            Ok(head)
        }
        fn u32le(b: &mut &[u8]) -> Result<usize, String> {
            let a: [u8; 4] = take(b, 4)?.try_into().map_err(|_| "invalid provider length")?;
            Ok(u32::from_le_bytes(a) as usize)
        }
        let mut input = bytes;
        if take(&mut input, 8)? != MAGIC {
            return Err("not a provider project bundle".into());
        }
        let len = u32le(&mut input)?;
        let count = u32le(&mut input)?;
        let checksum = u32le(&mut input)? as u32;
        if len == 0 || len > MAX_PROJECT || count > MAX_FILES {
            return Err("invalid provider bundle limits".into());
        }
        let project = take(&mut input, len)?.to_vec();
        if crc(&project) != checksum {
            return Err("provider project checksum mismatch".into());
        }
        let mut assets = BTreeMap::new();
        for _ in 0..count {
            let path_len: [u8; 2] = take(&mut input, 2)?.try_into().map_err(|_| "invalid provider path length")?;
            let path_len = u16::from_le_bytes(path_len) as usize;
            let len = u32le(&mut input)?;
            let checksum = u32le(&mut input)? as u32;
            if path_len > 4096 {
                return Err("provider asset path exceeds its limit".into());
            }
            let path = std::str::from_utf8(take(&mut input, path_len)?).map_err(|_| "provider asset path is not UTF-8")?.to_string();
            if !valid_path(&path) || assets.contains_key(&path) {
                return Err("invalid or duplicate provider asset path".into());
            }
            let data = take(&mut input, len)?.to_vec();
            if crc(&data) != checksum {
                return Err("provider media checksum mismatch".into());
            }
            assets.insert(path, data);
        }
        if !input.is_empty() {
            return Err("trailing provider bundle data".into());
        }
        Ok(Self { project, assets })
    }
}

/// Reject missing media before any provider bytes replace the current project.
/// Sequence duration describes the full source, independent of source marks or interpreted playback rate.
pub fn validate_media<'a>(project: &filmcraft_project::Project, paths: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let available: BTreeSet<&str> = paths.into_iter().collect();
    if available.len() > MAX_FILES {
        return Err("too many provider media files".into());
    }
    for item in project.items.values() {
        let Some(media) = item.as_media() else { continue };
        if let filmcraft_project::MediaRef::File { path } = &media.media {
            if !available.contains(path.as_str()) {
                return Err(format!("provider bundle is missing {path}"));
            }
            if media.info.kind == filmcraft_media::MediaKind::ImageSequence {
                validate_image_sequence(path, &media.info, &available)?;
            }
        }
        if let Some(filmcraft_project::MediaRef::File { path }) = &media.proxy
            && !available.contains(path.as_str())
        {
            return Err(format!("provider bundle is missing proxy {path}"));
        }
    }
    Ok(())
}

fn validate_image_sequence(first: &str, info: &filmcraft_media::MediaInfo, available: &BTreeSet<&str>) -> Result<(), String> {
    let numbered = filmcraft_media::sequence::Numbered::parse(first).ok_or("invalid provider image-sequence path")?;
    let rate = info.video.as_ref().ok_or("provider image sequence has no video metadata")?.frame_rate;
    if rate.num <= 0 || rate.den <= 0 || rate.frame_duration().0 <= 0 || info.duration.0 <= 0 {
        return Err("invalid provider image-sequence timing".into());
    }
    let count = rate.frame_at(info.duration);
    if count <= 0 || count > MAX_FILES as i64 || rate.tick_of(count) != info.duration {
        return Err("invalid or excessive provider image-sequence frame range".into());
    }
    let end = numbered.number.checked_add(count as u64).ok_or("provider image-sequence range overflows")?;
    let mut included = BTreeSet::new();
    for path in available {
        let cut = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
        let (dir, name) = path.split_at(cut);
        if dir != numbered.dir {
            continue;
        }
        if let Some(number) = numbered.number_of(name).filter(|number| *number >= numbered.number && *number < end)
            && !included.insert(number)
        {
            return Err("provider image-sequence frame paths are ambiguous".into());
        }
    }
    for number in numbered.number..end {
        if !included.contains(&number) {
            return Err(format!("provider bundle is missing image-sequence frame {}", numbered.path(number)));
        }
    }
    Ok(())
}

/// Rebase only native primary/proxy MediaRef::File paths, never arbitrary text or arrays.
pub fn rewrite(value: &mut serde_json::Value, replacements: &BTreeMap<String, String>) {
    if let Some(project) = value.get_mut("project") {
        rewrite_project(project, replacements);
    } else {
        rewrite_project(value, replacements);
    }
}

fn rewrite_project(value: &mut serde_json::Value, replacements: &BTreeMap<String, String>) {
    let Some(items) = value.get_mut("items").and_then(serde_json::Value::as_object_mut) else { return };
    for item in items.values_mut() {
        if let Some(media) = item.get_mut("kind").and_then(|kind| kind.get_mut("Media")) {
            for key in ["media", "proxy"] {
                if let Some(serde_json::Value::String(path)) =
                    media.get_mut(key).and_then(|reference| reference.get_mut("File")).and_then(|file| file.get_mut("path"))
                    && let Some(staged) = replacements.get(path)
                {
                    *path = staged.clone();
                }
            }
        }
    }
}

/// A private browser-table location. No provider identity is interpreted as an OS path.
/// Relative sidecar relationships are retained below this per-open root.
pub fn stage_path(root: &str, path: &str) -> Result<String, String> {
    if !root.starts_with("/__file_provider/") || !valid_path(path) {
        return Err("invalid provider staging path".into());
    }
    let normalized = path.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err("provider asset escapes its root".into());
                }
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err("empty provider asset path".into());
    }
    Ok(format!("{root}/{}", parts.join("/")))
}

/// Expand textual 3D/LUT sidecars without asking the provider to interpret project data.
/// Unknown sidecar formats fail closed instead of silently producing broken saved projects.
pub fn sidecars(path: &str, bytes: &[u8]) -> Result<Vec<String>, String> {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
    let join = |p: &str| -> Result<String, String> {
        if p.starts_with("data:") {
            return Ok(String::new());
        }
        if p.contains("://") || p.starts_with('/') || p.contains('\\') || p.contains('?') || p.contains('#') || p.split('/').any(|segment| segment == "..") {
            return Err("external model resource URI is unsupported".into());
        }
        Ok(if parent.is_empty() { p.into() } else { format!("{parent}/{p}") })
    };
    let mut out = BTreeSet::new();
    if ext == "gltf" {
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| format!("invalid glTF sidecars: {e}"))?;
        for key in ["buffers", "images"] {
            for v in value.get(key).and_then(serde_json::Value::as_array).into_iter().flatten() {
                if let Some(uri) = v.get("uri").and_then(serde_json::Value::as_str) {
                    let p = join(uri)?;
                    if !p.is_empty() {
                        out.insert(p);
                    }
                }
            }
        }
    } else if matches!(ext.as_str(), "obj" | "mtl") {
        let text = std::str::from_utf8(bytes).map_err(|_| "3D sidecar is not UTF-8")?;
        for line in text.lines() {
            let mut words = line.split_whitespace();
            let kind = words.next().unwrap_or("");
            if kind == "mtllib" || kind.starts_with("map_") || matches!(kind, "bump" | "disp" | "decal" | "norm") {
                let p = words.collect::<Vec<_>>().join(" ");
                if p.starts_with('-') {
                    return Err("OBJ material map options are unsupported in provider bundles".into());
                }
                if !p.is_empty() {
                    out.insert(join(&p)?);
                }
            }
        }
    } else if matches!(ext.as_str(), "ocio" | "c4d" | "fbx") {
        return Err("this external-resource format cannot yet be packaged losslessly".into());
    }
    Ok(out.into_iter().collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    #[test]
    fn project_and_media_round_trip_and_corruption_is_rejected() {
        let b = Bundle { project: br#"{"native":true}"#.to_vec(), assets: BTreeMap::from([("/picked/movie.bin".into(), vec![1, 0, 255, 7])]) };
        let bytes = b.encode().unwrap();
        assert_eq!(Bundle::decode(&bytes).unwrap(), b);
        let mut corrupt = bytes.clone();
        let n = corrupt.len();
        corrupt[n - 1] ^= 1;
        assert!(Bundle::decode(&corrupt).is_err());
        for n in 0..bytes.len() {
            assert!(Bundle::decode(&bytes[..n]).is_err());
        }
    }
    fn sequence_project(frames: i64) -> filmcraft_project::Project {
        use filmcraft_media::MediaSource;
        use filmcraft_project::{Interpretation, ItemKind, Label, MediaClip, MediaRef, Project};
        let rate = filmcraft_time::FrameRate::FPS_25;
        let mut info = filmcraft_media::generators::GeneratorSource::demo(filmcraft_media::DemoScene::OceanSunset).info().clone();
        info.kind = filmcraft_media::MediaKind::ImageSequence;
        info.duration = rate.tick_of(frames);
        info.audio_streams.clear();
        if let Some(video) = &mut info.video {
            video.frame_rate = rate;
        }
        let mut project = Project::new("Sequence bundle");
        project.add_item(
            "shot_0042.png",
            Label::Iris,
            ItemKind::Media(Box::new(MediaClip {
                media: MediaRef::File { path: "/picked/shot_0042.png".into() },
                info,
                interpret: Interpretation { frame_rate: Some(filmcraft_time::FrameRate::FPS_60), ..Default::default() },
                mark_in: Some(rate.tick_of(1)),
                mark_out: Some(rate.tick_of(2)),
                markers: Vec::new(),
                offline: false,
                proxy: None,
                identity: None,
            })),
            None,
        );
        project
    }

    #[test]
    fn sequence_bundle_rejects_missing_later_frame_before_open() {
        let project = sequence_project(2);
        let bundle = Bundle { project: serde_json::to_vec(&project).unwrap(), assets: BTreeMap::from([("/picked/shot_0042.png".into(), vec![42])]) };
        // Transport integrity alone is insufficient: the native source duration still declares two frames.
        let decoded = Bundle::decode(&bundle.encode().unwrap()).unwrap();
        let native: filmcraft_project::Project = serde_json::from_slice(&decoded.project).unwrap();
        let error = validate_media(&native, decoded.assets.keys().map(String::as_str)).unwrap_err();
        assert!(error.contains("/picked/shot_0043.png"));
    }

    #[test]
    fn sequence_bundle_round_trip_keeps_all_declared_frames() {
        let project = sequence_project(3);
        let bundle = Bundle {
            project: serde_json::to_vec(&project).unwrap(),
            assets: BTreeMap::from([
                ("/picked/shot_0042.png".into(), vec![42]),
                ("/picked/shot_0043.png".into(), vec![43]),
                ("/picked/shot_0044.PNG".into(), vec![44]),
            ]),
        };
        let decoded = Bundle::decode(&bundle.encode().unwrap()).unwrap();
        let native: filmcraft_project::Project = serde_json::from_slice(&decoded.project).unwrap();
        assert_eq!(native, project);
        assert_eq!(decoded, bundle);
        // Source marks and a 60 fps interpretation must not truncate the original 25 fps range.
        validate_media(&native, decoded.assets.keys().map(String::as_str)).unwrap();
        assert!(validate_media(&native, ["/picked/shot_0042.png", "/picked/shot_0043.png"]).is_err());
    }

    #[test]
    fn sequence_bundle_rejects_gaps_and_invalid_declared_ranges() {
        assert!(validate_media(&sequence_project(3), ["/picked/shot_0042.png", "/picked/shot_0044.png"]).is_err());
        assert!(validate_media(&sequence_project(2), ["/picked/shot_0042.png", "/elsewhere/shot_0043.png"]).is_err());
        assert!(validate_media(&sequence_project(2), ["/picked/shot_0042.png", "/picked/shot_0043.png", "/picked/shot_0043.PNG"]).is_err());
        for count in [0, -1, MAX_FILES as i64 + 1] {
            assert!(validate_media(&sequence_project(count), ["/picked/shot_0042.png"]).is_err());
        }
        let mut project = sequence_project(2);
        let media = project.items.values_mut().next().unwrap().as_media_mut().unwrap();
        media.info.duration.0 += 1;
        assert!(validate_media(&project, ["/picked/shot_0042.png", "/picked/shot_0043.png"]).is_err());
        let media = project.items.values_mut().next().unwrap().as_media_mut().unwrap();
        media.info.video.as_mut().unwrap().frame_rate.den = 0;
        assert!(validate_media(&project, ["/picked/shot_0042.png", "/picked/shot_0043.png"]).is_err());
    }

    #[test]
    fn provider_media_rebase_preserves_native_text_metadata_and_caption_arrays() {
        use filmcraft_project::{CaptionFormat, CaptionTrack, ItemKind, Label, MediaRef, ParamValue, SequenceSettings, SourceGraphic, TrackId};
        let path = "/picked/shot_0042.png";
        let next = "/picked/shot_0043.png";
        let mut project = sequence_project(2);
        let media_id = *project.items.keys().next().unwrap();
        let item = project.items.get_mut(&media_id).unwrap();
        item.metadata.insert("path".into(), path.into());
        item.as_media_mut().unwrap().proxy = Some(MediaRef::File { path: path.into() });
        project.notes = path.into();
        let sequence_id = project.new_sequence("Captions", SequenceSettings::default(), 1, 0, None);
        let mut captions = CaptionTrack::new(TrackId(project.alloc_id()), "Subtitles".into(), CaptionFormat::Subtitle);
        captions.vtt_blocks.push(path.into());
        project.sequence_mut(sequence_id).unwrap().caption_tracks.push(captions);
        let graphic_id =
            project.add_item("Literal title", Label::Iris, ItemKind::Graphic { width: 1, height: 1, rate: filmcraft_time::FrameRate::FPS_25 }, None);
        project.source_graphics.insert(
            graphic_id,
            SourceGraphic { layers: vec![filmcraft_project::graphic::new_text_layer(path, filmcraft_geom::Vec2::new(0.0, 0.0), 20.0)], meta: None },
        );
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba([12, 34, 56, 255])))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let bytes = png.into_inner();
        let bundle = Bundle { project: serde_json::to_vec(&project).unwrap(), assets: BTreeMap::from([(path.into(), bytes.clone()), (next.into(), bytes)]) };
        let decoded = Bundle::decode(&bundle.encode().unwrap()).unwrap();
        let native: filmcraft_project::Project = serde_json::from_slice(&decoded.project).unwrap();
        validate_media(&native, decoded.assets.keys().map(String::as_str)).unwrap();
        let staged = stage_path("/__file_provider/open-text", path).unwrap();
        let replacements = BTreeMap::from([(path.into(), staged.clone()), (next.into(), stage_path("/__file_provider/open-text", next).unwrap())]);
        // Both the native file envelope and the legacy bare project preserve non-file strings.
        for envelope in [false, true] {
            let mut json = serde_json::to_value(&native).unwrap();
            if envelope {
                json = serde_json::json!({"project": json, "view": {"selected": [path]}});
            }
            rewrite(&mut json, &replacements);
            if envelope {
                assert_eq!(json["view"]["selected"][0], path);
            }
            let restored: filmcraft_project::Project = serde_json::from_value(if envelope { json["project"].take() } else { json }).unwrap();
            let item = restored.items.get(&media_id).unwrap();
            let media = item.as_media().unwrap();
            assert_eq!(media.media, MediaRef::File { path: staged.clone() });
            assert_eq!(media.proxy, Some(MediaRef::File { path: staged.clone() }));
            assert_eq!(item.metadata.get("path").unwrap(), path);
            assert_eq!(restored.notes, path);
            assert_eq!(restored.sequence(sequence_id).unwrap().caption_tracks.first().unwrap().vtt_blocks, vec![path]);
            let layer = restored.source_graphics.get(&graphic_id).unwrap().layers.first().unwrap();
            assert_eq!(layer.params.get("text").unwrap().value, ParamValue::Text(path.into()));
        }
        assert_eq!(image::load_from_memory(decoded.assets.get(path).unwrap()).unwrap().width(), 1);
    }

    #[test]
    fn staging_is_private_and_rejects_escape() {
        assert_eq!(stage_path("/__file_provider/page-1", "/files/a/../b.png").unwrap(), "/__file_provider/page-1/files/b.png");
        assert!(stage_path("/__file_provider/page-1", "../../secret").is_err());
    }
    #[test]
    fn model_sidecars_are_kept_and_network_dependencies_are_refused() {
        assert_eq!(
            sidecars("/files/scene/model.gltf", br#"{"buffers":[{"uri":"mesh.bin"}],"images":[{"uri":"textures/albedo.png"}]}"#).unwrap(),
            vec!["/files/scene/mesh.bin", "/files/scene/textures/albedo.png"]
        );
        assert!(sidecars("/files/a.gltf", br#"{"images":[{"uri":"https://example.invalid/image"}]}"#).is_err());
    }
}
