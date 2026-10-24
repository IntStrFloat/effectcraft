//! Purpose-led, inspectable motion recipes built from the existing command registry.
//! The client interprets language; these deterministic recipes never pretend to be a model.

use crate::tools::Reply;
use crate::{Backend, Error, Result};
use effectcraft_engine::time::FrameRate;
use serde_json::{Value, json};

const RECIPES: &[&str] = &["title", "caption", "card", "screen", "impact", "glitch"];
const VIBES: &[&str] = &["clean", "soft", "sharp", "playful", "retro", "futuristic", "dark", "cinematic"];
const MOTIONS: &[&str] = &["fade", "slide_left", "slide_right", "pop", "zoom", "squash"];
const DESIGN_KEYS: &[&str] = &["recipe", "vibe", "brief", "name", "text", "width", "height", "frame_rate", "duration", "source_item", "transparent", "accent"];
const ANIMATE_KEYS: &[&str] = &["comp", "layer", "motion", "start", "duration", "enter", "exit", "distance", "replace", "rest_time"];

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn reference_schema(description: &str) -> Value {
    json!({"type":["integer","string"],"description":description})
}

pub(crate) fn catalogue_schema() -> Value {
    schema(json!({}), &[])
}

pub(crate) fn plan_schema() -> Value {
    schema(
        json!({
            "recipe":{"type":"string","enum":RECIPES,"default":"title"},
            "vibe":{"type":"string","enum":VIBES,"default":"clean"},
            "brief":{"type":"string","maxLength":4096,"description":"Creative context, interpreted by the client model. Explicit recipe/vibe select the deterministic starting point."},
            "name":{"type":"string","maxLength":160},"text":{"type":"string","maxLength":1024},
            "width":{"type":"integer","minimum":64,"maximum":3840,"default":1280},
            "height":{"type":"integer","minimum":64,"maximum":2160,"default":720},
            "frame_rate":{"type":"number","minimum":1,"maximum":120,"default":30},
            "duration":{"type":"number","minimum":0.25,"maximum":30,"default":2},
            "source_item":reference_schema("Existing visual footage, solid or composition item id/name. Import first with file.import; this tool never edits its source item."),
            "transparent":{"type":"boolean","default":false},
            "accent":{"type":"string","pattern":"^#[0-9a-fA-F]{6}$","description":"Optional original accent colour, #rrggbb."}
        }),
        &[],
    )
}

pub(crate) fn create_schema() -> Value {
    plan_schema()
}

pub(crate) fn animate_schema() -> Value {
    schema(
        json!({
            "comp":reference_schema("Explicit composition id/name."),"layer":reference_schema("Explicit layer id/name/#n in that composition."),
            "motion":{"type":"string","enum":MOTIONS,"default":"fade"},
            "start":{"type":"number","minimum":0,"description":"Comp seconds; default the layer's visible in point."},
            "duration":{"type":"number","minimum":0.25,"maximum":30,"description":"Window length, default up to two seconds within the visible layer."},
            "enter":{"type":"number","exclusiveMinimum":0,"description":"Entrance seconds; rounded to whole frames."},
            "exit":{"type":"number","exclusiveMinimum":0,"description":"Exit seconds; rounded to whole frames."},
            "distance":{"type":"number","minimum":0,"maximum":7680,"description":"Slide distance in comp pixels; default 20% of comp width."},
            "replace":{"type":"boolean","default":false,"description":"False refuses keys/expressions on affected properties. True replaces all animation/expressions on only those properties; undo restores them."},
            "rest_time":{"type":"number","minimum":0,"description":"Comp time to sample the rest pose; default the midpoint of the animation window."}
        }),
        &["comp", "layer"],
    )
}

fn bad(message: impl Into<String>) -> Error {
    Error::BadArgs(message.into())
}

fn object(a: &Value, accepted: &[&str]) -> Result<()> {
    let map = a.as_object().ok_or_else(|| bad("arguments must be an object"))?;
    if let Some(key) = map.keys().find(|key| !accepted.contains(&key.as_str())) {
        return Err(bad(format!("unknown argument `{key}`; expected {}", accepted.join(", "))));
    }
    Ok(())
}

fn text(a: &Value, key: &str, default: &str, max: usize) -> Result<String> {
    match a.get(key) {
        None => Ok(default.to_string()),
        Some(Value::String(s)) if s.chars().count() <= max => Ok(s.trim().to_string()),
        _ => Err(bad(format!("`{key}` must be a string of at most {max} characters"))),
    }
}

fn number(a: &Value, key: &str, default: f64, min: f64, max: f64) -> Result<f64> {
    match a.get(key) {
        None => Ok(default),
        Some(v) => v.as_f64().filter(|v| v.is_finite() && (min..=max).contains(v)).ok_or_else(|| bad(format!("`{key}` must be in {min}..{max}"))),
    }
}

fn integer(a: &Value, key: &str, default: u64, min: u64, max: u64) -> Result<u64> {
    match a.get(key) {
        None => Ok(default),
        Some(v) => v.as_u64().filter(|v| (min..=max).contains(v)).ok_or_else(|| bad(format!("`{key}` must be an integer in {min}..{max}"))),
    }
}

fn boolean(a: &Value, key: &str, default: bool) -> Result<bool> {
    a.get(key).map_or(Ok(default), |v| v.as_bool().ok_or_else(|| bad(format!("`{key}` must be boolean"))))
}

fn choice(a: &Value, key: &str, default: &str, options: &[&str]) -> Result<String> {
    let value = text(a, key, default, 80)?.to_ascii_lowercase();
    if options.contains(&value.as_str()) { Ok(value) } else { Err(bad(format!("unknown `{key}`: choose {}", options.join(", ")))) }
}

fn reference(a: &Value, key: &str) -> Result<Value> {
    match a.get(key) {
        Some(v) if v.as_u64().is_some_and(|v| v > 0) => Ok(v.clone()),
        Some(Value::String(s)) if !s.trim().is_empty() && s.chars().count() <= 256 => Ok(json!(s)),
        _ => Err(bad(format!("`{key}` needs an explicit positive id or nonempty name"))),
    }
}

// engine.batch treats an entire `$N.key` string as a result reference. Keep user-visible
// literals out of that grammar instead of accidentally substituting another step's value.
fn literal(s: &str, key: &str) -> Result<()> {
    if let Some(rest) = s.strip_prefix('$') {
        let mut parts = rest.split('.');
        if parts.next().is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            && parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        {
            return Err(bad(format!("`{key}` looks like a batch reference; set this literal later with a native text command")));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Window {
    start: f64,
    enter: f64,
    hold: f64,
    end: f64,
    duration: f64,
    fps: f64,
}

impl Window {
    fn new(start: f64, duration: f64, fps: f64, enter: Option<f64>, exit: Option<f64>) -> Result<Self> {
        let frames = (duration * fps + 1e-8).floor() as u64;
        if frames < 4 {
            return Err(bad("animation needs at least four frames; increase duration or frame rate"));
        }
        let default = ((frames as f64 * 0.2).round() as u64).clamp(1, frames.saturating_sub(3));
        let phase = |seconds: Option<f64>| -> Result<u64> {
            match seconds {
                Some(s) => {
                    let n = (s * fps).round() as u64;
                    if n == 0 || n >= frames.saturating_sub(2) { Err(bad("enter/exit must each span at least one frame and leave a hold")) } else { Ok(n) }
                }
                None => Ok(default),
            }
        };
        let en = phase(enter)?;
        let ex = phase(exit)?;
        if en.saturating_add(ex) >= frames.saturating_sub(1) {
            return Err(bad("enter + exit must leave at least one hold frame"));
        }
        let start = (start * fps).round() / fps;
        Ok(Self {
            start,
            enter: start + en as f64 / fps,
            hold: start + (frames - 1 - ex) as f64 / fps,
            end: start + (frames - 1) as f64 / fps,
            duration: frames as f64 / fps,
            fps,
        })
    }

    fn json(self) -> Value {
        json!({"start":self.start,"enter_end":self.enter,"hold_end":self.hold,"last_visible_frame":self.end,
            "duration":self.duration,"frame_rate":self.fps,"timeBase":"comp",
            "quantization":"Duration is rounded down to whole frames; entrance and exit are whole-frame phases."})
    }
}

struct Design {
    recipe: String,
    vibe: String,
    brief: String,
    name: String,
    text: String,
    width: u64,
    height: u64,
    window: Window,
    source: Option<Value>,
    source_size: Option<(f64, f64)>,
    transparent: bool,
    accent: String,
    warnings: Vec<String>,
}

fn parse_design(b: &mut Backend, a: &Value) -> Result<Design> {
    object(a, DESIGN_KEYS)?;
    let recipe = choice(a, "recipe", "title", RECIPES)?;
    let vibe = choice(a, "vibe", "clean", VIBES)?;
    let brief = text(a, "brief", "", 4096)?;
    let name = text(a, "name", &format!("{vibe} {recipe}"), 160)?;
    let title = text(a, "text", if recipe == "caption" { "Subtitle" } else { "Motion" }, 1024)?;
    if name.is_empty() || title.is_empty() {
        return Err(bad("`name` and `text` must not be empty"));
    }
    literal(&name, "name")?;
    literal(&title, "text")?;
    let width = integer(a, "width", 1280, 64, 3840)?;
    let height = integer(a, "height", 720, 64, 2160)?;
    if width.checked_mul(height).is_none_or(|n| n > 8_294_400) {
        return Err(bad("scene exceeds the 4K pixel budget"));
    }
    let fps = FrameRate::from_f64(number(a, "frame_rate", 30.0, 1.0, 120.0)?).as_f64();
    let seconds = number(a, "duration", 2.0, 0.25, 30.0)?;
    let window = Window::new(0.0, seconds, fps, None, None)?;
    let transparent = boolean(a, "transparent", false)?;
    let default_accent = match vibe.as_str() {
        "soft" => "#b5d4ce",
        "sharp" => "#ffb749",
        "playful" => "#fc7da9",
        "retro" => "#b9d875",
        "futuristic" => "#66d9ff",
        "dark" => "#bb71ca",
        "cinematic" => "#d9b77e",
        _ => "#6ea8ff",
    };
    let accent = text(a, "accent", default_accent, 7)?;
    if accent.len() != 7 || !accent.starts_with('#') || !accent.get(1..).is_some_and(|s| s.chars().all(|c| c.is_ascii_hexdigit())) {
        return Err(bad("`accent` must be #rrggbb"));
    }
    let mut warnings = vec!["Recipe and vibe are deterministic starting points; the client model interprets the creative brief.".into()];
    let (source, source_size) = if a.get("source_item").is_some() {
        if !matches!(recipe.as_str(), "card" | "screen") {
            return Err(bad("`source_item` is used only by card/screen recipes"));
        }
        let wanted = reference(a, "source_item")?;
        let summary = b.exec("project.summary", json!({}))?;
        let items = summary.get("items").and_then(Value::as_array).ok_or_else(|| bad("project summary returned no items"))?;
        let item = items
            .iter()
            .find(|item| item.get("id") == Some(&wanted) || wanted.as_str().is_some_and(|name| item.get("name").and_then(Value::as_str) == Some(name)))
            .ok_or_else(|| bad("source_item does not exist; import media first or use an item id from get_project"))?;
        let size = item.get("size").and_then(Value::as_array).ok_or_else(|| bad("source_item must have visual dimensions"))?;
        let w = size.first().and_then(Value::as_f64).filter(|n| *n > 0.0).ok_or_else(|| bad("source_item is not visual footage"))?;
        let h = size.get(1).and_then(Value::as_f64).filter(|n| *n > 0.0).ok_or_else(|| bad("source_item is not visual footage"))?;
        if item.get("duration").and_then(Value::as_f64).is_some_and(|d| d > 0.0 && d + 1.0 / fps < window.duration) {
            return Err(bad("source_item is shorter than this scene; lower duration or prepare a loop/time remap first"));
        }
        warnings.push("The existing item is reused without changing it. Screen uses cover scaling, so a different aspect ratio may crop at full size.".into());
        (Some(item.get("id").cloned().ok_or_else(|| bad("source item has no id"))?), Some((w, h)))
    } else {
        if matches!(recipe.as_str(), "card" | "screen") {
            warnings.push("No source_item: the card/screen contains an original procedural solid placeholder, not generated or imported footage.".into());
        }
        (None, None)
    };
    if title.chars().count() > 80 {
        warnings.push("Long text may need a larger paragraph box or smaller type; inspect a rendered frame.".into());
    }
    Ok(Design { recipe, vibe, brief, name, text: title, width, height, window, source, source_size, transparent, accent, warnings })
}

fn push(steps: &mut Vec<Value>, command: &str, params: Value) -> usize {
    steps.push(json!({"command":command,"params":params}));
    steps.len()
}

fn result_ref(step: usize, key: &str) -> Value {
    json!(format!("${step}.{key}"))
}

fn set(steps: &mut Vec<Value>, comp: &Value, layer: &Value, path: &str, value: Value) {
    push(steps, "prop.set", json!({"comp":comp,"layer":layer,"path":path,"value":value}));
}

fn curve(steps: &mut Vec<Value>, comp: &Value, layer: &Value, path: &str, keys: &[(f64, Value)], interpolation: &str) {
    for (time, value) in keys {
        push(steps, "prop.addKey", json!({"comp":comp,"layer":layer,"path":path,"time":time,"timeBase":"comp","value":value}));
    }
    let selected: Vec<Value> = keys.iter().map(|(t, _)| json!({"layer":layer,"path":path,"time":t})).collect();
    push(steps, "keys.select", json!({"keys":selected,"timeBase":"comp"}));
    if interpolation == "easyEase" {
        push(steps, "keys.easyEase", json!({}));
    } else {
        push(steps, "keys.interpolation", json!({"interpolation":interpolation}));
    }
}

fn four(window: Window, a: Value, b: Value, c: Value, d: Value) -> Vec<(f64, Value)> {
    vec![(window.start, a), (window.enter, b), (window.hold, c), (window.end, d)]
}

fn effect(steps: &mut Vec<Value>, comp: &Value, layer: &Value, effect: &str, index: &mut usize) -> String {
    push(steps, "effect.apply", json!({"comp":comp,"layers":[layer],"effect":effect}));
    *index += 1;
    format!("effects/#{}", *index)
}

fn build(d: &Design) -> Vec<Value> {
    let mut steps = vec![];
    let comp = result_ref(
        push(
            &mut steps,
            "comp.new",
            json!({"name":d.name,"width":d.width,"height":d.height,"frameRate":d.window.fps,"duration":d.window.duration,"background":"#11151e"}),
        ),
        "comp",
    );
    let smooth = d.recipe != "glitch" && d.vibe != "retro";
    if smooth {
        push(&mut steps, "comp.settings", json!({"comp":comp,"motionBlurSamples":8}));
        push(&mut steps, "comp.setSwitch", json!({"comp":comp,"switch":"motionBlur","value":true}));
    }
    let (w, h) = (d.width as f64, d.height as f64);
    if !d.transparent {
        push(&mut steps, "layer.newSolid", json!({"comp":comp,"name":"Background","color":"#11151e","width":d.width,"height":d.height}));
    }
    let card = matches!(d.recipe.as_str(), "card" | "screen");
    if d.recipe == "caption" {
        let plate = result_ref(
            push(
                &mut steps,
                "layer.newShape",
                json!({"comp":comp,"name":"Caption plate","kind":"rounded","size":[w*0.82,h*0.2],"fill":"#253246","position":[w*0.5,h*0.82]}),
            ),
            "layer",
        );
        curve(&mut steps, &comp, &plate, "transform/opacity", &four(d.window, json!(0), json!(88), json!(88), json!(0)), "easyEase");
    } else if !card {
        let accent = result_ref(
            push(
                &mut steps,
                "layer.newShape",
                json!({"comp":comp,"name":"Accent","kind":"rect","size":[w*0.16,(h*0.009).max(2.0)],"fill":d.accent,"position":[w*0.5,h*0.64]}),
            ),
            "layer",
        );
        curve(
            &mut steps,
            &comp,
            &accent,
            "transform/scale",
            &four(d.window, json!([0, 100, 100]), json!([100, 100, 100]), json!([100, 100, 100]), json!([0, 100, 100])),
            "easyEase",
        );
    }
    let layer = if card {
        let id = if let Some(source) = &d.source {
            push(&mut steps, "layer.addItem", json!({"comp":comp,"item":source,"time":0,"duration":d.window.duration}))
        } else {
            push(&mut steps, "layer.newSolid", json!({"comp":comp,"name":"Content","color":d.accent,"width":d.width,"height":d.height}))
        };
        let layer = result_ref(id, "layer");
        push(&mut steps, "layer.rename", json!({"comp":comp,"layer":layer,"name":"Content"}));
        layer
    } else {
        let y = if d.recipe == "caption" { h * 0.83 } else { h * 0.51 };
        result_ref(
            push(
                &mut steps,
                "layer.newText",
                json!({"comp":comp,"name":"Title","text":d.text,"size":(h*if d.recipe=="caption" {0.065} else {0.14}).max(12.0),"fill":"#f7f8fb","justify":"center","position":[w*0.5,y]}),
            ),
            "layer",
        )
    };
    if smooth {
        push(&mut steps, "layer.setSwitch", json!({"comp":comp,"layers":[layer],"switch":"motionBlur","value":true}));
    }
    let rest = if d.recipe == "caption" { json!([w * 0.5, h * 0.83, 0]) } else { json!([w * 0.5, h * 0.5, 0]) };
    let distance = match d.vibe.as_str() {
        "soft" => 0.04,
        "sharp" => 0.14,
        "playful" => 0.18,
        _ => 0.09,
    } * w;
    let positions = match d.recipe.as_str() {
        "card" => four(d.window, json!([-w * 0.25, h * 0.5, 0]), rest.clone(), rest.clone(), json!([w * 1.25, h * 0.5, 0])),
        "caption" => four(d.window, json!([w * 0.5, h * 0.98, 0]), rest.clone(), rest.clone(), json!([w * 0.5, h * 0.98, 0])),
        "glitch" => four(
            d.window,
            json!([w * 0.5 + distance, h * 0.5, 0]),
            rest.clone(),
            json!([w * 0.5 + distance * 0.03, h * 0.5, 0]),
            json!([w * 0.5 - distance, h * 0.5, 0]),
        ),
        _ => four(d.window, json!([w * 0.5, h * 0.5 + distance * 0.3, 0]), rest.clone(), rest.clone(), json!([w * 0.5, h * 0.5 - distance * 0.2, 0])),
    };
    curve(&mut steps, &comp, &layer, "transform/position", &positions, if d.recipe == "glitch" { "hold" } else { "easyEase" });
    let (sx, sy) = d.source_size.map_or((100.0, 100.0), |(sw, sh)| {
        let factor = if d.recipe == "screen" { (w / sw).max(h / sh) } else { (w / sw).min(h / sh) * 0.74 };
        (factor * 100.0, factor * 100.0)
    });
    let scale = if d.recipe == "screen" {
        four(d.window, json!([sx, sy, 100]), json!([sx * 0.62, sy * 0.62, 100]), json!([sx * 0.62, sy * 0.62, 100]), json!([sx, sy, 100]))
    } else {
        let (entry, settle) = match d.recipe.as_str() {
            "impact" => (1.6, 0.92),
            _ => match d.vibe.as_str() {
                "playful" => (0.6, 1.1),
                "soft" => (0.95, 1.0),
                _ => (0.85, 1.0),
            },
        };
        four(d.window, json!([sx * entry, sy * entry, 100]), json!([sx * settle, sy * settle, 100]), json!([sx, sy, 100]), json!([sx * 0.92, sy * 0.92, 100]))
    };
    curve(&mut steps, &comp, &layer, "transform/scale", &scale, "easyEase");
    if d.recipe == "screen" {
        curve(&mut steps, &comp, &layer, "transform/rotation", &four(d.window, json!(0), json!(-5), json!(-5), json!(0)), "easyEase");
    } else {
        curve(&mut steps, &comp, &layer, "transform/opacity", &four(d.window, json!(0), json!(100), json!(100), json!(0)), "easyEase");
    }
    let mut index = 0;
    if d.recipe == "glitch" {
        let prefix = effect(&mut steps, &comp, &layer, "ec.distort.turbulentdisplace", &mut index);
        set(&mut steps, &comp, &layer, &format!("{prefix}/size"), json!(18));
        curve(&mut steps, &comp, &layer, &format!("{prefix}/amount"), &four(d.window, json!(32), json!(0), json!(0), json!(22)), "hold");
    }
    if d.vibe == "retro" || d.recipe == "glitch" {
        let prefix = effect(&mut steps, &comp, &layer, "ec.time.posterizetime", &mut index);
        set(&mut steps, &comp, &layer, &format!("{prefix}/frameRate"), json!(12));
    }
    if matches!(d.vibe.as_str(), "soft" | "cinematic") || d.recipe == "impact" {
        let prefix = effect(&mut steps, &comp, &layer, "ec.blur.gaussian", &mut index);
        curve(&mut steps, &comp, &layer, &format!("{prefix}/blurriness"), &four(d.window, json!(12), json!(0), json!(0), json!(8)), "easyEase");
    }
    if matches!(d.vibe.as_str(), "futuristic" | "dark" | "cinematic") {
        let prefix = effect(&mut steps, &comp, &layer, "ec.stylize.glow", &mut index);
        set(&mut steps, &comp, &layer, &format!("{prefix}/intensity"), json!(if d.vibe == "dark" { 0.55 } else { 1.1 }));
    }
    push(&mut steps, "comp.info", json!({"comp":comp}));
    steps
}

fn report(d: &Design, steps: &[Value]) -> Value {
    json!({"recipe":d.recipe,"vibe":d.vibe,"brief":d.brief,"name":d.name,"text":d.text,
        "width":d.width,"height":d.height,"timing":d.window.json(),"source_item":d.source,"transparent":d.transparent,"accent":d.accent,
        "implementation":"2D layers, masks/shapes where applicable, native property keys and stock effects; no volumetric 3D or automatic rotoscoping is implied",
        "stages":["entrance","readable hold/rest pose","exit or screen return"],"warnings":d.warnings,"native_steps":steps,
        "sound_cues":{"status":"suggestions only; no audio synthesized or imported","entry":d.window.enter,"exit":d.window.hold,
            "note":"Choose an SFX by purpose and vibe, import it explicitly, keep narration separate from scene/music perspective."},
        "limits":{"duration_seconds":30,"width":3840,"height":2160,"fps":120,"minimum_frames":4}})
}

pub(crate) fn catalogue(_: &mut Backend, a: &Value) -> Result<Reply> {
    object(a, &[])?;
    Ok(Reply::Json(json!({"recipes":[
        {"id":"title","purpose":"Readable title with accent and complete entrance/hold/exit."},
        {"id":"caption","purpose":"Lower caption with plate, readable hold and exit."},
        {"id":"card","purpose":"Existing visual item or original solid crosses a 2D card stage."},
        {"id":"screen","purpose":"Full-size source becomes a smaller rotated image, holds and returns; 2D simulation."},
        {"id":"impact","purpose":"Scale/blur accent followed by a readable rest pose."},
        {"id":"glitch","purpose":"Stepped motion, native displacement and posterized time."}],
        "vibes":VIBES,"language":"Client model interprets arbitrary language; explicit recipe/vibe select starting points.",
        "native_editing":"All commands, property trees, masks, tracking, 3D cameras, expressions and render queue remain available through the existing tools.",
        "adobe_sdk_plugins":false,"ae_project_compatibility":false})))
}

pub(crate) fn plan(b: &mut Backend, a: &Value) -> Result<Reply> {
    let d = parse_design(b, a)?;
    Ok(Reply::Json(report(&d, &build(&d))))
}

pub(crate) fn create(b: &mut Backend, a: &Value) -> Result<Reply> {
    let d = parse_design(b, a)?;
    let steps = build(&d);
    let applied = b.exec("engine.batch", json!({"steps":steps,"atomic":true,"label":format!("Create {} scene",d.recipe)}))?;
    let results = applied.get("results").and_then(Value::as_array).ok_or_else(|| bad("batch returned no results; inspect the project before retrying"))?;
    let comp = results.last().ok_or_else(|| bad("batch returned no composition; inspect before retrying"))?;
    let mut out = report(&d, &[]);
    if let Some(map) = out.as_object_mut() {
        map.remove("native_steps");
    }
    out["comp"] = comp.get("id").cloned().unwrap_or(Value::Null);
    out["name"] = comp.get("name").cloned().unwrap_or(Value::Null);
    out["layers"] = comp.get("layers").cloned().unwrap_or_else(|| json!([]));
    let content = comp
        .get("layers")
        .and_then(Value::as_array)
        .and_then(|layers| layers.iter().find(|l| l.get("name").and_then(Value::as_str).is_some_and(|n| n == "Title" || n == "Content")));
    out["layer"] = content.and_then(|l| l.get("id")).cloned().unwrap_or(Value::Null);
    out["undo"] = json!("One undo removes the complete new scene; existing compositions and source items are preserved.");
    Ok(Reply::Json(out))
}

fn vector(value: &Value, path: &str) -> Result<Vec<f64>> {
    let values = value.as_array().ok_or_else(|| bad(format!("`{path}` is not a vector")))?;
    if !(2..=3).contains(&values.len()) {
        return Err(bad(format!("`{path}` must have two or three components")));
    }
    values.iter().map(|v| v.as_f64().filter(|n| n.is_finite()).ok_or_else(|| bad(format!("`{path}` has an invalid component")))).collect()
}

fn scaled(values: &[f64], x: f64, y: f64) -> Value {
    json!(
        values
            .iter()
            .enumerate()
            .map(|(i, v)| v * if i == 0 {
                x
            } else if i == 1 {
                y
            } else {
                1.0
            })
            .collect::<Vec<_>>()
    )
}

fn shifted(values: &[f64], x: f64) -> Value {
    json!(values.iter().enumerate().map(|(i, v)| v + if i == 0 { x } else { 0.0 }).collect::<Vec<_>>())
}

pub(crate) fn animate(b: &mut Backend, a: &Value) -> Result<Reply> {
    object(a, ANIMATE_KEYS)?;
    let comp = reference(a, "comp")?;
    let layer = reference(a, "layer")?;
    let motion = choice(a, "motion", "fade", MOTIONS)?;
    let replace = boolean(a, "replace", false)?;
    let info = b.exec("comp.info", json!({"comp":comp}))?;
    let cid = info.get("id").cloned().ok_or_else(|| bad("composition has no id"))?;
    let comp_end = info.get("duration").and_then(Value::as_f64).ok_or_else(|| bad("composition has no duration"))?;
    let fps = info
        .get("frameRate")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 1.0 && *v <= 120.0)
        .ok_or_else(|| bad("helper supports compositions from 1 to 120 fps"))?;
    let tree = b.exec("layer.tree", json!({"comp":cid,"layer":layer,"depth":2}))?;
    let lid = tree.get("id").cloned().ok_or_else(|| bad("layer has no id"))?;
    let layer_in = tree.get("in").and_then(Value::as_f64).unwrap_or(0.0).max(0.0);
    let layer_out = tree.get("out").and_then(Value::as_f64).unwrap_or(comp_end).min(comp_end);
    let start = number(a, "start", layer_in, 0.0, comp_end)?;
    let available = (layer_out - start).min(30.0);
    if available < 0.25 || start + 1e-8 < layer_in {
        return Err(bad("animation window must be inside the layer's visible comp range"));
    }
    let duration = number(a, "duration", available.min(2.0), 0.25, 30.0)?;
    let optional_phase = |key: &str| -> Result<Option<f64>> { a.get(key).map(|_| number(a, key, 0.0, 1e-6, 30.0)).transpose() };
    let window = Window::new(start, duration, fps, optional_phase("enter")?, optional_phase("exit")?)?;
    if window.start + window.duration > layer_out + 1e-8 {
        return Err(bad("animation extends past the layer or composition; reduce duration"));
    }
    let rest_time = number(a, "rest_time", window.start + window.duration * 0.5, layer_in, layer_out)?;
    let width = info.get("width").and_then(Value::as_f64).unwrap_or(1280.0);
    let distance = number(a, "distance", width * 0.2, 0.0, 7680.0)?;
    let mut paths = vec!["transform/opacity"];
    if matches!(motion.as_str(), "slide_left" | "slide_right") {
        paths.push("transform/position");
    }
    if matches!(motion.as_str(), "pop" | "zoom" | "squash") {
        paths.push("transform/scale");
    }
    let mut properties = vec![];
    for path in &paths {
        let property = b.exec("prop.get", json!({"comp":cid,"layer":lid,"path":path,"time":rest_time}))?;
        let keys = property.get("keys").and_then(Value::as_array).map_or(0, Vec::len);
        let expr = property.get("expression").and_then(Value::as_str).is_some_and(|v| !v.trim().is_empty());
        if !replace && (keys > 0 || expr) {
            return Err(bad(format!(
                "`{path}` already has animation/expression; use replace:true to replace only the affected properties, or refine with native keyframe tools"
            )));
        }
        properties.push((path.to_string(), property, keys, expr));
    }
    let mut curves = vec![];
    for (path, property, _, _) in &properties {
        let value = property.get("evaluated").or_else(|| property.get("value")).ok_or_else(|| bad("property returned no value"))?;
        let keys = match path.as_str() {
            "transform/opacity" => {
                let opacity = value.as_f64().filter(|v| v.is_finite()).ok_or_else(|| bad("opacity is not numeric"))?;
                four(window, json!(0), json!(opacity), json!(opacity), json!(0))
            }
            "transform/position" => {
                let position = vector(value, path)?;
                let sign = if motion == "slide_left" { -1.0 } else { 1.0 };
                four(window, shifted(&position, sign * distance), value.clone(), value.clone(), shifted(&position, -sign * distance))
            }
            _ => {
                let scale = vector(value, path)?;
                let (entry, overshoot, exit) = match motion.as_str() {
                    "squash" => ((1.45, 0.2), (0.95, 1.05), (1.4, 0.15)),
                    "zoom" => ((0.65, 0.65), (1.0, 1.0), (1.25, 1.25)),
                    _ => ((0.4, 0.4), (1.08, 1.08), (0.65, 0.65)),
                };
                four(window, scaled(&scale, entry.0, entry.1), scaled(&scale, overshoot.0, overshoot.1), value.clone(), scaled(&scale, exit.0, exit.1))
            }
        };
        curves.push((path.clone(), keys));
    }
    let mut steps = vec![];
    push(&mut steps, "comp.open", json!({"comp":cid}));
    for (path, _, keys, expr) in &properties {
        if *keys > 0 {
            push(&mut steps, "prop.toggleAnimation", json!({"comp":cid,"layer":lid,"path":path,"value":false}));
        }
        if *expr {
            push(&mut steps, "prop.setExpression", json!({"comp":cid,"layer":lid,"path":path,"expression":""}));
        }
    }
    for (path, keys) in &curves {
        curve(&mut steps, &cid, &lid, path, keys, "easyEase");
    }
    b.exec("engine.batch", json!({"steps":steps,"atomic":true,"label":format!("Animate layer: {motion}")}))?;
    Ok(Reply::Json(json!({"comp":cid,"layer":lid,"motion":motion,"timing":window.json(),"rest_time":rest_time,
        "affected_properties":paths,"replace":replace,"replaced":properties.iter().map(|(path,_,keys,expr)|json!({"path":path,"prior_keys":keys,"prior_expression":expr})).collect::<Vec<_>>(),
        "note":"Rest pose sampled at rest_time. Times are comp seconds, including offset/stretched layers. One undo restores the previous affected properties."})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> Backend {
        Backend::headless(effectcraft_host::session())
    }
    fn value(reply: Reply) -> Value {
        match reply {
            Reply::Json(v) => v,
            _ => panic!("expected JSON"),
        }
    }

    #[test]
    fn planning_is_read_only_and_has_explicit_assumptions() {
        let mut b = backend();
        let before = b.exec("project.summary", json!({})).unwrap();
        let planned = value(plan(&mut b, &json!({"brief":"Мягкое появление заголовка","recipe":"title","vibe":"soft"})).unwrap());
        assert_eq!(planned["recipe"], "title");
        assert_eq!(planned["timing"]["timeBase"], "comp");
        assert!(!planned["native_steps"].as_array().unwrap().is_empty());
        assert_eq!(b.exec("project.summary", json!({})).unwrap(), before);
    }

    #[test]
    fn every_recipe_vibe_builds_native_editable_commands() {
        let mut b = backend();
        for recipe in RECIPES {
            for vibe in VIBES {
                let result = value(create(&mut b, &json!({"recipe":recipe,"vibe":vibe,"width":128,"height":72,"duration":0.5,"frame_rate":24})).unwrap());
                assert!(result["comp"].as_u64().is_some(), "{recipe}/{vibe}: {result}");
                let layer = b.exec("layer.tree", json!({"comp":result["comp"],"layer":result["layer"]})).unwrap();
                assert!(layer["properties"].to_string().contains("keys"));
            }
        }
    }

    #[test]
    fn source_and_existing_project_survive_create_undo_redo() {
        let mut b = backend();
        let source = b.exec("comp.new", json!({"name":"Original","width":160,"height":90,"duration":3,"frameRate":30})).unwrap()["comp"].clone();
        b.exec("layer.newSolid", json!({"name":"Source picture","color":"#e85c34"})).unwrap();
        let original = b.exec("comp.info", json!({"comp":source})).unwrap();
        let before = b.exec("project.summary", json!({})).unwrap()["undo"].as_array().unwrap().len();
        let scene = value(create(&mut b, &json!({"recipe":"screen","source_item":source,"duration":1,"width":160,"height":90})).unwrap());
        assert_ne!(scene["comp"], source);
        assert_eq!(b.exec("comp.info", json!({"comp":source})).unwrap(), original);
        assert_eq!(b.exec("project.summary", json!({})).unwrap()["undo"].as_array().unwrap().len(), before + 1);
        assert!(b.session().unwrap().undo());
        assert!(b.exec("comp.info", json!({"comp":scene["comp"]})).is_err());
        assert!(b.session().unwrap().redo());
        assert!(b.exec("comp.info", json!({"comp":scene["comp"]})).is_ok());
    }

    #[test]
    fn title_recipe_renders_visible_pixels_during_its_hold() {
        let mut b = backend();
        let scene = value(create(&mut b, &json!({"recipe":"title","text":"Motion","width":256,"height":144,"duration":1,"transparent":true})).unwrap());
        let first = b.render_with(Some(&scene["comp"]), Some(0.0), 256, true).unwrap();
        let middle = b.render_with(Some(&scene["comp"]), Some(0.5), 256, true).unwrap();
        let image = image::load_from_memory(&middle.png).unwrap().to_rgba8();
        assert!(image.pixels().any(|p| p.0[3] > 0));
        assert_ne!(first.png, middle.png, "the recipe must change actual rendered frames");
    }

    #[test]
    fn bad_arguments_do_not_make_partial_scenes() {
        let mut b = backend();
        let before = b.exec("project.summary", json!({})).unwrap();
        for args in [
            json!({"recipe":"unknown"}),
            json!({"width":999999}),
            json!({"duration":31}),
            json!({"frame_rate":1,"duration":0.25}),
            json!({"transparent":"false"}),
            json!({"recipe":"card","source_item":"missing"}),
            json!({"text":"$1.comp"}),
            json!({"vibe":"missing"}),
            json!({"accent":"blue"}),
            json!({"duraton":1}),
        ] {
            assert!(create(&mut b, &args).is_err(), "{args}");
            assert_eq!(b.exec("project.summary", json!({})).unwrap(), before);
        }
    }

    #[test]
    fn animation_is_comp_time_and_replacement_is_explicit() {
        let mut b = backend();
        let comp = b.exec("comp.new", json!({"name":"Offset","width":160,"height":90,"duration":8,"frameRate":30})).unwrap()["comp"].clone();
        let layer = b.exec("layer.newSolid", json!({"name":"Card"})).unwrap()["layer"].clone();
        b.exec("layer.timing", json!({"layer":layer,"start":2,"in":2,"out":7})).unwrap();
        b.exec("layer.timeStretch", json!({"layers":[layer],"percent":150})).unwrap();
        let result = value(animate(&mut b, &json!({"comp":comp,"layer":layer,"motion":"pop","start":2,"duration":1})).unwrap());
        assert_eq!(result["timing"]["start"], 2.0);
        let opacity = b.exec("prop.get", json!({"comp":comp,"layer":layer,"path":"transform/opacity","time":2.5})).unwrap();
        assert!(opacity["value"].as_f64().unwrap() > 90.0);
        assert!(opacity["keys"].as_array().unwrap().iter().all(|k| k["compTime"].as_f64().unwrap() >= 2.0));
        let before = b.exec("project.summary", json!({})).unwrap();
        assert!(animate(&mut b, &json!({"comp":comp,"layer":layer,"motion":"pop","start":2,"duration":1})).is_err());
        assert_eq!(b.exec("project.summary", json!({})).unwrap(), before);
        assert!(animate(&mut b, &json!({"comp":comp,"layer":layer,"motion":"squash","start":2,"duration":1,"replace":true})).is_ok());
    }
}
