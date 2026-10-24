//! Read-only motion review: real sampled frames and a bounded composition audit.
use std::collections::{HashMap, HashSet};

use image::{Rgba, RgbaImage};
use serde_json::{Value, json};

use crate::tools::Reply;
use crate::{Backend, Error, Result, encode_png, png_size};

const MAX_FRAMES: usize = 12;
const MAX_PIXELS: u64 = 8_000_000;
const MAX_LAYERS: usize = 128;
const MAX_NODES: usize = 4096;
const MAX_KEYS: usize = 8192;
const MAX_ISSUES: usize = 256;
const MAX_PROJECT_ITEMS: usize = 4096;
const GAP: u32 = 8;

fn schema(properties: Value) -> Value {
    json!({"type":"object", "properties":properties, "additionalProperties":false})
}

fn comp_schema() -> Value {
    json!({"type":["integer","string"],"description":"Composition id or exact name; omit for the active composition."})
}

pub(crate) fn storyboard_schema() -> Value {
    schema(json!({
        "comp":comp_schema(),
        "times":{"type":"array","minItems":1,"maxItems":MAX_FRAMES,
            "items":{"type":"number","minimum":0},
            "description":"Comp seconds, each strictly before the comp duration. Omit for five samples through the last valid frame."},
        "max_side":{"type":"integer","minimum":64,"maximum":960,"default":320},
        "columns":{"type":"integer","minimum":1,"maximum":4,"default":3},
        "transparent":{"type":"boolean","default":false,
            "description":"Keep actual rendered alpha. The atlas gaps are transparent; no checkerboard is baked into the image."}
    }))
}

pub(crate) fn analyze_schema() -> Value {
    schema(json!({"comp":comp_schema()}))
}

fn args<'a>(value: &'a Value, accepted: &[&str]) -> Result<&'a serde_json::Map<String, Value>> {
    let object = value.as_object().ok_or_else(|| Error::BadArgs("arguments must be an object".into()))?;
    if let Some(key) = object.keys().find(|key| !accepted.contains(&key.as_str())) {
        return Err(Error::BadArgs(format!("unknown argument `{key}`")));
    }
    if let Some(comp) = object.get("comp") {
        let valid = comp.as_u64().is_some() || comp.as_str().is_some_and(|name| !name.trim().is_empty() && name.len() <= 4096);
        if !valid {
            return Err(Error::BadArgs("`comp` must be an id or a nonempty name".into()));
        }
    }
    Ok(object)
}

fn integer(value: Option<&Value>, name: &str, default: u32, low: u32, high: u32) -> Result<u32> {
    match value {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|n| *n >= u64::from(low) && *n <= u64::from(high))
            .map(|n| n as u32)
            .ok_or_else(|| Error::BadArgs(format!("`{name}` must be an integer in {low}..{high}"))),
    }
}

fn comp_info(b: &mut Backend, value: Option<&Value>) -> Result<Value> {
    b.exec("comp.info", value.map_or_else(|| json!({}), |comp| json!({"comp":comp})))
}

fn number(info: &Value, field: &str) -> Result<f64> {
    info.get(field).and_then(Value::as_f64).filter(|n| n.is_finite() && *n > 0.0).ok_or_else(|| Error::Other(format!("composition has no valid `{field}`")))
}

fn dimensions(info: &Value) -> Result<(u32, u32)> {
    let dimension = |field| {
        info.get(field)
            .and_then(Value::as_u64)
            .filter(|n| *n > 0 && *n <= 1_000_000)
            .map(|n| n as u32)
            .ok_or_else(|| Error::Other(format!("composition has no valid `{field}`")))
    };
    Ok((dimension("width")?, dimension("height")?))
}

fn atlas_size(width: u32, height: u32, columns: u32, rows: u32) -> Result<(u32, u32)> {
    let axis = |size: u32, count: u32| size.checked_mul(count).and_then(|n| count.saturating_sub(1).checked_mul(GAP).and_then(|gaps| n.checked_add(gaps)));
    let (Some(width), Some(height)) = (axis(width, columns), axis(height, rows)) else {
        return Err(Error::BadArgs("storyboard dimensions overflow; reduce samples or max_side".into()));
    };
    if u64::from(width).checked_mul(u64::from(height)).is_none_or(|n| n > MAX_PIXELS) {
        return Err(Error::BadArgs("storyboard exceeds 8 million pixels; reduce samples or max_side".into()));
    }
    Ok((width, height))
}

fn fingerprint(bytes: &[u8]) -> String {
    // A comparison fingerprint, not a cryptographic checksum or a persistent file identity.
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3));
    format!("{hash:016x}")
}

pub(crate) fn render_storyboard(b: &mut Backend, value: &Value) -> Result<Reply> {
    let a = args(value, &["comp", "times", "max_side", "columns", "transparent"])?;
    let max_side = integer(a.get("max_side"), "max_side", 320, 64, 960)?;
    let requested_columns = integer(a.get("columns"), "columns", 3, 1, 4)?;
    let transparent = match a.get("transparent") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(|| Error::BadArgs("`transparent` must be boolean".into()))?,
    };
    let comp = comp_info(b, a.get("comp"))?;
    let duration = number(&comp, "duration")?;
    let frame_rate = number(&comp, "frameRate")?;
    let (width, height) = dimensions(&comp)?;
    let times = match a.get("times") {
        Some(value) => {
            let values = value
                .as_array()
                .filter(|v| !v.is_empty() && v.len() <= MAX_FRAMES)
                .ok_or_else(|| Error::BadArgs(format!("`times` must contain 1..{MAX_FRAMES} comp-second values")))?;
            values
                .iter()
                .map(|value| {
                    value
                        .as_f64()
                        .filter(|n| n.is_finite() && *n >= 0.0 && *n < duration)
                        .ok_or_else(|| Error::BadArgs(format!("every `times` value must be finite and in 0..{duration} (end exclusive)")))
                })
                .collect::<Result<Vec<_>>>()?
        }
        None => {
            let last = (duration - 1.0 / frame_rate).max(0.0);
            (0..5).map(|index| last * f64::from(index) / 4.0).collect()
        }
    };
    let columns = requested_columns.min(times.len() as u32);
    let rows = (times.len() as u32).div_ceil(columns);
    let scale = (f64::from(max_side) / f64::from(width.max(height))).min(1.0);
    let tile_width = (f64::from(width) * scale).ceil().max(1.0) as u32;
    let tile_height = (f64::from(height) * scale).ceil().max(1.0) as u32;
    // Validate every requested time and the complete allocation before rendering anything.
    let (atlas_width, atlas_height) = atlas_size(tile_width, tile_height, columns, rows)?;
    let mut atlas = RgbaImage::from_pixel(atlas_width, atlas_height, Rgba([0, 0, 0, 0]));
    let cid = comp.get("id").filter(|id| id.is_u64()).ok_or_else(|| Error::Other("composition query returned no id".into()))?;
    let mut tiles = Vec::with_capacity(times.len());
    let mut hashes = HashSet::new();
    let mut all_transparent = true;
    for (index, time) in times.into_iter().enumerate() {
        let frame = b.render_with(Some(cid), Some(time), max_side, transparent)?;
        let (fw, fh) = png_size(&frame.png).ok_or_else(|| Error::Other("frame renderer returned no PNG dimensions".into()))?;
        if fw == 0 || fh == 0 || fw > tile_width || fh > tile_height || fw > max_side || fh > max_side {
            return Err(Error::Other("rendered dimensions exceed the storyboard budget or the composition changed during sampling".into()));
        }
        let image = image::load_from_memory(&frame.png).map_err(|e| Error::Other(format!("cannot decode rendered frame: {e}")))?.to_rgba8();
        let column = index as u32 % columns;
        let row = index as u32 / columns;
        let x = column * (tile_width + GAP);
        let y = row * (tile_height + GAP);
        image::imageops::replace(&mut atlas, &image, i64::from(x), i64::from(y));
        let pixels = u64::from(fw) * u64::from(fh);
        let visible = image.pixels().filter(|pixel| pixel.0.get(3).is_some_and(|a| *a > 0)).count() as u64;
        let hash = fingerprint(image.as_raw());
        hashes.insert(hash.clone());
        all_transparent &= visible == 0;
        tiles.push(json!({"index":index,"time":frame.time,"row":row,"column":column,"x":x,"y":y,
            "width":fw,"height":fh,"alpha_coverage":visible as f64 / pixels.max(1) as f64,
            "frame_fingerprint_fnv1a64":hash}));
    }
    let mut warnings = Vec::new();
    if tiles.len() > 1 && hashes.len() == 1 {
        warnings.push(json!({"code":"sampled_frames_identical","message":"These sampled frames are identical. Try times around the action; this alone does not prove the scene has no animation."}));
    }
    if all_transparent {
        warnings.push(
            json!({"code":"all_transparent","message":"Every sampled output is transparent. Check visibility, in/out times and the selected composition."}),
        );
    }
    let png = encode_png(atlas_width, atlas_height, atlas.into_raw(), 0)?;
    Ok(Reply::Image {
        png,
        info: json!({"comp":cid,"name":comp.get("name"),"width":atlas_width,"height":atlas_height,
            "columns":columns,"rows":rows,"frame_count":tiles.len(),"duration":duration,"frame_rate":frame_rate,
            "max_side":max_side,"transparent":transparent,"frames":tiles,"warnings":warnings,
            "scope":"Actual sequential renders of the current composition; no project, CTI, selection or undo edits and no file writes. Keep a bridged app stable for comparable samples. Alpha coverage measures output opacity, including the background when transparent is false.",
            "limits":{"max_frames":MAX_FRAMES,"max_atlas_pixels":MAX_PIXELS,"aesthetic_quality_certified":false}}),
    })
}

#[derive(Default)]
struct Issues {
    entries: Vec<Value>,
    total: usize,
}

impl Issues {
    fn add(&mut self, code: &str, message: &str, context: Value) {
        self.total = self.total.saturating_add(1);
        if self.entries.len() < MAX_ISSUES {
            self.entries.push(json!({"code":code,"message":message,"context":context}));
        }
    }
}

struct PropertyAudit<'a> {
    duration: f64,
    frame_rate: f64,
    effects: &'a HashSet<String>,
    nodes: usize,
    keys: usize,
    animated_properties: usize,
    effect_instances: usize,
    truncated: bool,
}

impl PropertyAudit<'_> {
    fn layer(&mut self, layer: &Value, tree: &Value, issues: &mut Issues) {
        let start = layer.get("start").and_then(Value::as_f64).unwrap_or(0.0);
        let stretch = layer.get("stretch").and_then(Value::as_f64).unwrap_or(100.0);
        let context = |path: &Value| json!({"layer":layer.get("id"),"name":layer.get("name"),"path":path});
        let Some(properties) = tree.get("properties") else {
            issues.add("missing_property_tree", "The layer query returned no property tree.", context(&Value::Null));
            return;
        };
        let mut stack = vec![properties];
        while let Some(node) = stack.pop() {
            if self.nodes >= MAX_NODES {
                self.truncated = true;
                break;
            }
            self.nodes += 1;
            let path = node.get("path").unwrap_or(&Value::Null);
            if node.get("truncated").and_then(Value::as_bool) == Some(true) {
                self.truncated = true;
            }
            if let Some(effect) = node.get("effect").and_then(Value::as_str) {
                self.effect_instances += 1;
                if !self.effects.contains(effect) {
                    issues.add("unknown_effect", "The effect is absent from the current registry and may be skipped by rendering. Load its supported plugin or replace it before delivery.", json!({"layer":layer.get("id"),"path":path,"effect":effect}));
                }
            }
            if let Some(keys) = node.get("keys").and_then(Value::as_array) {
                if !keys.is_empty() {
                    self.animated_properties += 1;
                }
                let mut previous = None;
                for key in keys {
                    if self.keys >= MAX_KEYS {
                        self.truncated = true;
                        break;
                    }
                    self.keys += 1;
                    let Some(time) = key.get("time").and_then(Value::as_f64).filter(|time| time.is_finite()) else {
                        issues.add("invalid_key_time", "A stored key has no finite layer time.", context(path));
                        continue;
                    };
                    if previous.is_some_and(|previous| time <= previous) {
                        issues.add("unordered_key_times", "Stored keys are not in strictly increasing layer-time order.", context(path));
                    }
                    previous = Some(time);
                    let comp_time = start + time * stretch / 100.0;
                    if !comp_time.is_finite() || comp_time < 0.0 || comp_time > self.duration + 1.0 / self.frame_rate {
                        issues.add(
                            "key_outside_comp_span",
                            "A key maps outside the composition span. It may intentionally shape an interpolation; verify the sampled result.",
                            json!({"layer":layer.get("id"),"path":path,"layer_time":time,"comp_time":comp_time}),
                        );
                    }
                }
            }
            if let Some(children) = node.get("children").and_then(Value::as_array) {
                let room = MAX_NODES.saturating_sub(self.nodes.saturating_add(stack.len()));
                self.truncated |= children.len() > room;
                stack.extend(children.iter().take(room).rev());
            }
        }
    }
}

pub(crate) fn analyze_scene(b: &mut Backend, value: &Value) -> Result<Reply> {
    let a = args(value, &["comp"])?;
    let comp = comp_info(b, a.get("comp"))?;
    let duration = number(&comp, "duration")?;
    let frame_rate = number(&comp, "frameRate")?;
    let (width, height) = dimensions(&comp)?;
    let cid = comp.get("id").filter(|id| id.is_u64()).ok_or_else(|| Error::Other("composition query returned no id".into()))?;
    let layers = comp.get("layers").and_then(Value::as_array).ok_or_else(|| Error::Other("composition query returned no layers".into()))?;
    let project = b.exec("project.summary", json!({}))?;
    let registry = b.exec("effect.list", json!({}))?;
    let effects: HashSet<String> = registry
        .as_array()
        .ok_or_else(|| Error::Other("effect registry returned no list".into()))?
        .iter()
        .filter_map(|effect| effect.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let ids: HashSet<u64> = layers.iter().filter_map(|layer| layer.get("id").and_then(Value::as_u64)).collect();
    let parents: HashMap<u64, u64> = layers.iter().filter_map(|layer| Some((layer.get("id")?.as_u64()?, layer.get("parent")?.as_u64()?))).collect();
    let mut issues = Issues::default();
    let mut audit = PropertyAudit { duration, frame_rate, effects: &effects, nodes: 0, keys: 0, animated_properties: 0, effect_instances: 0, truncated: false };
    let mut hidden = 0;
    let mut active_picture_layers = 0;
    if layers.is_empty() {
        issues.add("empty_comp", "The composition contains no layers.", json!({"comp":cid}));
    }
    for layer in layers.iter().take(MAX_LAYERS) {
        let context = json!({"layer":layer.get("id"),"name":layer.get("name")});
        let start = layer.get("in").and_then(Value::as_f64);
        let end = layer.get("out").and_then(Value::as_f64);
        let picture = !matches!(layer.get("type").and_then(Value::as_str), Some("Null" | "Camera" | "Light"));
        let visible = layer.pointer("/switches/video").and_then(Value::as_bool) != Some(false);
        if picture && !visible {
            hidden += 1;
            issues.add("hidden_layer", "This picture layer has its video switch off.", context.clone());
        }
        match (start, end) {
            (Some(start), Some(end)) if start.is_finite() && end.is_finite() && end > start => {
                if end <= 0.0 || start >= duration {
                    issues.add("layer_outside_comp", "The layer never overlaps the composition span.", context.clone());
                } else if visible && picture {
                    active_picture_layers += 1;
                }
                if start < 0.0 || end > duration {
                    issues.add(
                        "layer_extends_comp",
                        "The layer extends beyond the composition. This can be intentional; verify delivery bounds.",
                        context.clone(),
                    );
                }
            }
            _ => issues.add("invalid_layer_span", "The layer has a nonpositive or invalid in/out span.", context.clone()),
        }
        if layer.get("stretch").and_then(Value::as_f64).is_none_or(|stretch| !stretch.is_finite() || stretch == 0.0) {
            issues.add("invalid_layer_stretch", "The layer has invalid or zero time stretch.", context.clone());
        }
        for (field, dependency) in
            [("parent", layer.get("parent").and_then(Value::as_u64)), ("trackMatte", layer.pointer("/trackMatte/layer").and_then(Value::as_u64))]
        {
            if dependency.is_some_and(|id| !ids.contains(&id)) {
                issues.add(
                    "missing_layer_dependency",
                    "A parent or track-matte layer is absent from this composition.",
                    json!({"layer":layer.get("id"),"field":field,"target":dependency}),
                );
            }
        }
        if let Some(id) = layer.get("id").and_then(Value::as_u64) {
            let mut seen = HashSet::new();
            let mut cursor = id;
            for _ in 0..=MAX_LAYERS {
                if !seen.insert(cursor) {
                    issues.add("parent_cycle", "The layer's parent chain is cyclic.", context.clone());
                    break;
                }
                let Some(parent) = parents.get(&cursor) else { break };
                cursor = *parent;
            }
        }
        if audit.nodes < MAX_NODES && audit.keys < MAX_KEYS {
            let tree = b.exec("layer.tree", json!({"comp":cid,"layer":layer.get("id"),"depth":8}))?;
            audit.layer(layer, &tree, &mut issues);
        } else {
            audit.truncated = true;
        }
    }
    if !layers.is_empty() && active_picture_layers == 0 {
        issues.add(
            "no_visible_picture_layers",
            "No audited picture layer overlaps the comp span with video enabled. Background or procedural dependencies can still contribute pixels.",
            json!({"comp":cid}),
        );
    }
    let mut source_problems = 0;
    let project_items = project.get("items").and_then(Value::as_array);
    if let Some(items) = project_items {
        for item in items.iter().take(MAX_PROJECT_ITEMS) {
            if let Some(problem) = item.get("problem") {
                source_problems += 1;
                issues.add(
                    "source_problem",
                    "The project's footage backend reports a problem. This is project-wide cached metadata, not a new filesystem check.",
                    json!({"item":item.get("id"),"name":item.get("name"),"problem":problem}),
                );
            }
        }
    }
    let truncated = layers.len() > MAX_LAYERS
        || audit.truncated
        || issues.total > issues.entries.len()
        || project_items.is_some_and(|items| items.len() > MAX_PROJECT_ITEMS);
    Ok(Reply::Json(json!({"comp":cid,"name":comp.get("name"),"width":width,"height":height,"duration":duration,"frame_rate":frame_rate,
        "layers":layers.len(),"audited_layers":layers.len().min(MAX_LAYERS),"hidden_picture_layers":hidden,
        "visible_picture_layers_overlapping_comp":active_picture_layers,"property_nodes":audit.nodes,"keyframes":audit.keys,
        "animated_properties":audit.animated_properties,"effect_instances":audit.effect_instances,
        "audited_project_items":project_items.map_or(0, |items| items.len().min(MAX_PROJECT_ITEMS)),
        "project_source_problems":source_problems,"issues":issues.entries,"issue_count":issues.total,"truncated":truncated,
        "source_availability":"Project metadata only; files and decoders have not been revalidated.",
        "scope":"Read-only structural review of one composition and project-wide cached source problems. Keys are mapped from layer time to comp time using start and stretch. Visibility counts describe switches and timing, not guaranteed rendered content. Successful rendering does not certify all effects, aesthetic quality or After Effects fidelity.",
        "limits":{"layers":MAX_LAYERS,"property_nodes":MAX_NODES,"keyframes":MAX_KEYS,"project_items":MAX_PROJECT_ITEMS,"issues":MAX_ISSUES,"tree_depth":8}})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> Backend {
        Backend::headless(effectcraft_host::session())
    }

    fn setup(b: &mut Backend) -> Value {
        let comp = b.exec("comp.new", json!({"name":"Motion","width":96,"height":64,"frameRate":10,"duration":1})).unwrap()["comp"].clone();
        let layer = b.exec("layer.newSolid", json!({"name":"Red","color":"#ff0000","width":24,"height":24})).unwrap()["layer"].clone();
        b.exec("prop.addKey", json!({"comp":comp,"layer":layer,"path":"transform/position","time":0,"timeBase":"comp","value":[20,32]})).unwrap();
        b.exec("prop.addKey", json!({"comp":comp,"layer":layer,"path":"transform/position","time":0.9,"timeBase":"comp","value":[76,32]})).unwrap();
        comp
    }

    #[test]
    fn storyboard_renders_changing_real_pixels_without_document_edits() {
        let mut b = backend();
        let comp = setup(&mut b);
        let before = b.exec("project.summary", json!({})).unwrap();
        let state = b.exec("editor.state", json!({})).unwrap();
        let Reply::Image { png, info } = render_storyboard(&mut b, &json!({"comp":comp,"times":[0,0.9],"columns":2,"max_side":96,"transparent":true})).unwrap()
        else {
            panic!("expected image")
        };
        let image = image::load_from_memory(&png).unwrap().to_rgba8();
        assert!(image.pixels().any(|p| p[0] > 200 && p[3] > 0));
        assert_ne!(info["frames"][0]["frame_fingerprint_fnv1a64"], info["frames"][1]["frame_fingerprint_fnv1a64"]);
        assert!(image.pixels().any(|p| p[3] == 0));
        assert_eq!(info["frame_count"], 2);
        assert_eq!(b.exec("project.summary", json!({})).unwrap(), before);
        assert_eq!(b.exec("editor.state", json!({})).unwrap(), state);
    }

    #[test]
    fn default_times_end_at_the_last_frame_and_empty_alpha_is_reported() {
        let mut b = backend();
        b.exec("comp.new", json!({"width":64,"height":64,"duration":1,"frameRate":10})).unwrap();
        let Reply::Image { info, .. } = render_storyboard(&mut b, &json!({"transparent":true})).unwrap() else { panic!("image") };
        assert_eq!(info["frames"][4]["time"], 0.9);
        assert!(info["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "all_transparent"));
        let Reply::Json(audit) = analyze_scene(&mut b, &json!({})).unwrap() else { panic!("json") };
        assert!(audit["issues"].as_array().unwrap().iter().any(|w| w["code"] == "empty_comp"));
    }

    #[test]
    fn hostile_preview_parameters_fail_without_edits() {
        let mut b = backend();
        setup(&mut b);
        let before = b.exec("project.summary", json!({})).unwrap();
        for value in [
            json!({"times":[]}),
            json!({"times":[-0.1]}),
            json!({"times":[1.0]}),
            json!({"times":["0"]}),
            json!({"times":vec![0;13]}),
            json!({"columns":0}),
            json!({"columns":5}),
            json!({"max_side":0}),
            json!({"max_side":961}),
            json!({"transparent":"false"}),
            json!({"comp":null}),
            json!({"path":"out.png"}),
            Value::Null,
        ] {
            assert!(render_storyboard(&mut b, &value).is_err(), "{value}");
            assert_eq!(b.exec("project.summary", json!({})).unwrap(), before);
        }
        b.exec("comp.settings", json!({"width":960,"height":960})).unwrap();
        assert!(render_storyboard(&mut b, &json!({"times":vec![0;12],"max_side":960})).is_err());
    }

    #[test]
    fn structural_audit_reports_hidden_layers_and_unknown_effects() {
        let mut b = backend();
        let comp = setup(&mut b);
        let layer = b.exec("comp.info", json!({"comp":comp})).unwrap()["layers"][0]["id"].clone();
        b.exec("layer.setSwitch", json!({"layers":[layer],"switch":"video","value":false})).unwrap();
        let before = b.exec("project.summary", json!({})).unwrap();
        let Reply::Json(audit) = analyze_scene(&mut b, &json!({"comp":comp})).unwrap() else { panic!("json") };
        assert!(audit["issues"].as_array().unwrap().iter().any(|issue| issue["code"] == "hidden_layer"));
        assert_eq!(audit["animated_properties"], 1);
        assert!(!audit["truncated"].as_bool().unwrap());
        assert_eq!(b.exec("project.summary", json!({})).unwrap(), before);
        // Registry absence is checked structurally without requiring an invalid engine edit.
        let mut issues = Issues::default();
        let effects = HashSet::new();
        let mut checker = PropertyAudit {
            duration: 1.0,
            frame_rate: 10.0,
            effects: &effects,
            nodes: 0,
            keys: 0,
            animated_properties: 0,
            effect_instances: 0,
            truncated: false,
        };
        checker.layer(&json!({"id":1,"start":0,"stretch":100}), &json!({"properties":{"effect":"unloaded.plugin","path":"effects/#1"}}), &mut issues);
        assert_eq!(issues.entries[0]["code"], "unknown_effect");
    }

    #[test]
    fn stored_key_times_are_converted_to_comp_time_for_offset_reversed_layers() {
        let mut issues = Issues::default();
        let effects = HashSet::new();
        let mut checker = PropertyAudit {
            duration: 5.0,
            frame_rate: 10.0,
            effects: &effects,
            nodes: 0,
            keys: 0,
            animated_properties: 0,
            effect_instances: 0,
            truncated: false,
        };
        checker.layer(
            &json!({"id":1,"start":4,"stretch":-100}),
            &json!({"properties":{"path":"transform/opacity","keys":[{"time":0},{"time":1},{"time":2}]}}),
            &mut issues,
        );
        assert!(issues.entries.is_empty(), "{:#?}", issues.entries);
        checker.layer(&json!({"id":1,"start":4,"stretch":100}), &json!({"properties":{"path":"transform/opacity","keys":[{"time":2}]}}), &mut issues);
        assert_eq!(issues.entries[0]["code"], "key_outside_comp_span");
    }

    #[test]
    fn property_and_issue_budgets_are_explicit() {
        let mut issues = Issues::default();
        let effects = HashSet::new();
        let mut checker = PropertyAudit {
            duration: 1.0,
            frame_rate: 10.0,
            effects: &effects,
            nodes: 0,
            keys: 0,
            animated_properties: 0,
            effect_instances: 0,
            truncated: false,
        };
        let children: Vec<Value> = (0..MAX_NODES + 100).map(|i| json!({"path":format!("effects/#{i}"),"effect":"missing.plugin"})).collect();
        checker.layer(&json!({"id":1,"start":0,"stretch":100}), &json!({"properties":{"children":children}}), &mut issues);
        assert!(checker.truncated);
        assert_eq!(checker.nodes, MAX_NODES);
        assert_eq!(issues.entries.len(), MAX_ISSUES);
        assert!(issues.total > issues.entries.len());
    }
}
