//! Purpose-led MCP workflows must produce editable, changing pixels and durable native output.
use std::collections::HashSet;

use serde_json::{Value, json};

use crate::harness::{Qa, mean_diff};
use effectcraft_automation::base64;

fn create(qa: &mut Qa, recipe: &str, extra: Value) -> Value {
    let mut args = json!({"recipe":recipe,"vibe":"clean","name":"Motion QA","text":"Привет, мир",
        "width":160,"height":90,"frame_rate":12,"duration":1,"transparent":true});
    args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    qa.tool("create_motion_scene", args)
}

/// Original generated fixture: deliberately asymmetric so position, rotation and scale are visible.
fn import_picture(qa: &mut Qa) -> Value {
    let picture = image::RgbaImage::from_fn(160, 90, |x, y| {
        if x < 50 && y < 40 {
            image::Rgba([240, 40, 20, 255])
        } else if x > 100 {
            image::Rgba([10, 220, 70, 255])
        } else if (x / 10 + y / 10) % 2 == 0 {
            image::Rgba([40, 80, 240, 255])
        } else {
            image::Rgba([235, 220, 35, 255])
        }
    });
    picture.save(qa.dir.join("original.png")).unwrap();
    qa.exec("file.import", json!({"paths":[qa.path("original.png")]}))["items"][0].clone()
}

fn project_item(qa: &mut Qa, id: &Value) -> Value {
    qa.tool("get_project", json!({}))["items"].as_array().unwrap().iter().find(|item| item["id"] == *id).unwrap().clone()
}

#[test]
fn brief_to_cyrillic_scene_and_storyboard_is_a_real_read_only_review() {
    let mut qa = Qa::new("motion-brief-review");
    let capabilities = qa.tool("get_capabilities", json!({}));
    assert_eq!(capabilities["compatibility"]["adobe_sdk_plugins"], false);
    assert!(capabilities["renderer"].is_object());
    assert!(qa.tool("list_plugins", json!({}))["plugins"].is_array());
    assert!(qa.tool("get_motion_workflow", json!({}))["guide"].as_str().unwrap().len() > 100);
    let before_plan = qa.tool("get_project", json!({}));
    let plan = qa.tool(
        "plan_motion",
        json!({"recipe":"title","vibe":"soft","text":"Привет, мир",
        "brief":"Мягкое появление заголовка","width":160,"height":90,"frame_rate":12,"duration":1}),
    );
    assert_eq!(plan["timing"]["timeBase"], "comp");
    assert!(!plan["native_steps"].as_array().unwrap().is_empty());
    assert_eq!(qa.tool("get_project", json!({})), before_plan);

    let scene = create(&mut qa, "title", json!({"vibe":"soft"}));
    let comp = scene["comp"].clone();
    assert!(comp.is_u64());
    assert!(scene["layer"].is_u64());
    let tree = qa.tool("get_layer", json!({"comp":comp,"layer":scene["layer"],"flat":true}));
    assert!(tree.to_string().contains("Привет, мир"), "text remains editable: {tree}");
    let entrance = qa.frame(Some(comp.clone()), 0.0);
    let hold = qa.frame(Some(comp.clone()), 0.5);
    assert!(mean_diff(&entrance, &hold) > 0.1, "the scene must change actual pixels");

    let before_query = qa.tool("get_project", json!({}));
    let state_before = qa.tool("get_state", json!({}))["state"].clone();
    let content = qa.try_tool("render_storyboard", json!({"comp":comp,"times":[0,0.25,0.5,0.9],"max_side":160,"columns":2,"transparent":true})).unwrap();
    let png = content.iter().find(|part| part["type"] == "image").unwrap();
    let bytes = base64::decode(png["data"].as_str().unwrap()).unwrap();
    let atlas = image::load_from_memory(&bytes).unwrap().to_rgba8();
    assert!(atlas.pixels().any(|pixel| pixel[3] > 0));
    let info: Value = serde_json::from_str(content.iter().find(|part| part["type"] == "text").unwrap()["text"].as_str().unwrap()).unwrap();
    assert_eq!(info["frame_count"], 4);
    let fingerprints: HashSet<&str> = info["frames"].as_array().unwrap().iter().map(|frame| frame["frame_fingerprint_fnv1a64"].as_str().unwrap()).collect();
    assert!(fingerprints.len() > 1, "storyboard sampled changing frames: {info}");
    assert!(info["frames"][0]["alpha_coverage"].as_f64().unwrap() < info["frames"][2]["alpha_coverage"].as_f64().unwrap());
    let audit = qa.tool("analyze_scene", json!({"comp":comp}));
    assert!(!audit["truncated"].as_bool().unwrap());
    assert!(audit["keyframes"].as_u64().unwrap() > 0);
    assert_eq!(qa.tool("get_project", json!({})), before_query);
    assert_eq!(qa.tool("get_state", json!({}))["state"], state_before);
}

#[test]
fn source_screen_survives_undo_save_reopen_and_native_alpha_sequence_export() {
    let mut qa = Qa::new("motion-source-delivery");
    let source = import_picture(&mut qa);
    let old_comp = qa.exec("comp.new", json!({"name":"Existing","width":160,"height":90,"frameRate":12,"duration":1}))["comp"].clone();
    qa.exec("layer.newSolid", json!({"name":"Keep","color":"#304080","width":30,"height":20}));
    let old = qa.exec("comp.info", json!({"comp":old_comp}));
    let source_before = project_item(&mut qa, &source);
    let undo_before = qa.tool("get_project", json!({}))["undo"].as_array().unwrap().len();
    let scene = create(&mut qa, "screen", json!({"name":"Screen","duration":0.5,"source_item":source}));
    let comp = scene["comp"].clone();
    assert_ne!(comp, old_comp);
    assert_eq!(qa.exec("comp.info", json!({"comp":old_comp})), old);
    assert_eq!(project_item(&mut qa, &source), source_before);
    assert_eq!(qa.tool("get_project", json!({}))["undo"].as_array().unwrap().len(), undo_before + 1);
    qa.tool("undo", json!({}));
    assert!(qa.exec_err("comp.info", json!({"comp":comp})).contains("composition"));
    assert_eq!(qa.exec("comp.info", json!({"comp":old_comp})), old);
    assert_eq!(project_item(&mut qa, &source), source_before);
    qa.tool("redo", json!({}));
    let full = qa.frame(Some(comp.clone()), 0.0);
    let reduced = qa.frame(Some(comp.clone()), 0.25);
    assert!(mean_diff(&full, &reduced) > 2.0, "source content must actually transform");
    let save = qa.path("screen.ecproj");
    qa.tool("save_project", json!({"path":save}));
    qa.tool("open_project", json!({"new":true}));
    qa.tool("open_project", json!({"path":save}));
    assert!(mean_diff(&reduced, &qa.frame(Some(comp.clone()), 0.25)) < 1e-9, "reopen preserves rendered pixels");
    let position = qa.tool("get_property", json!({"comp":comp,"layer":scene["layer"],"path":"transform/scale","time":0.25}));
    assert!(position["keys"].as_array().unwrap().len() >= 4, "reopen keeps native animation editable");
    qa.exec("renderQueue.add", json!({"comp":comp,"format":"png","channels":"rgba","output":qa.path("frames/screen_[##].png")}));
    let rendered = qa.exec("renderQueue.render", json!({"wait":true}));
    assert_eq!(rendered["items"][0]["status"], "Done", "{rendered}");
    let frames: Vec<_> = std::fs::read_dir(qa.dir.join("frames"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "png"))
        .collect();
    assert_eq!(frames.len(), 6, "0.5 seconds at 12fps produces exactly six native frames");
    let images: Vec<_> = frames.iter().map(|path| image::open(path).unwrap().to_rgba8()).collect();
    assert!(images.iter().all(|image| image.dimensions() == (160, 90)));
    assert!(images.iter().any(|image| image.pixels().any(|pixel| pixel[3] == 0)), "RGBA export preserves transparent surround");
    assert!(images.iter().any(|image| image.pixels().any(|pixel| pixel[3] > 0)), "export contains real source content");
}

#[test]
fn effect_chain_errors_leave_no_partial_stack_and_success_has_one_undo() {
    let mut qa = Qa::new("motion-effect-chain");
    let source = import_picture(&mut qa);
    let comp = qa.exec("comp.new", json!({"name":"Effects","width":160,"height":90,"frameRate":12,"duration":1}))["comp"].clone();
    let layer = qa.exec("layer.addItem", json!({"item":source}))["layer"].clone();
    let before = qa.tool("get_project", json!({}));
    let tree = qa.tool("get_layer", json!({"comp":comp,"layer":layer}));
    for effects in [
        json!([{"effect":"Gaussian Blur","values":{"blurriness":8}},{"effect":"unknown-native-plugin"}]),
        json!([{"effect":"Gaussian Blur","values":{"blurriness":8}},{"effect":"Gaussian Blur","values":{"blurriness":"bad-value"}}]),
        json!([{"effect":"Gaussian Blur","enabled":"false"}]),
    ] {
        assert!(qa.try_tool("apply_effect_chain", json!({"comp":comp,"layer":layer,"effects":effects})).is_err());
        assert_eq!(qa.tool("get_project", json!({})), before);
        assert_eq!(qa.tool("get_layer", json!({"comp":comp,"layer":layer})), tree);
    }
    let dry = qa.frame(Some(comp.clone()), 0.0);
    qa.tool("apply_effect_chain", json!({"comp":comp,"layer":layer,"effects":[{"effect":"Gaussian Blur","values":{"blurriness":10}}]}));
    let wet = qa.frame(Some(comp.clone()), 0.0);
    assert!(mean_diff(&dry, &wet) > 0.2, "applied effect changes source pixels");
    assert_eq!(qa.tool("get_project", json!({}))["undo"].as_array().unwrap().len(), before["undo"].as_array().unwrap().len() + 1);
    qa.tool("undo", json!({}));
    assert!(mean_diff(&dry, &qa.frame(Some(comp.clone()), 0.0)) < 1e-9);
    qa.tool("redo", json!({}));
    assert!(mean_diff(&wet, &qa.frame(Some(comp), 0.0)) < 1e-9);
}

#[test]
fn animation_uses_comp_time_and_explicit_replacement_is_reversible() {
    let mut qa = Qa::new("motion-comp-time");
    let comp = qa.exec("comp.new", json!({"name":"Offset","width":160,"height":90,"frameRate":12,"duration":4}))["comp"].clone();
    let layer = qa.exec("layer.newSolid", json!({"name":"Subject","color":"#20d080","width":50,"height":30}))["layer"].clone();
    qa.exec("layer.timing", json!({"layers":[layer],"start":0.5}));
    qa.exec("layer.timeStretch", json!({"layers":[layer],"percent":150}));
    let animate = json!({"comp":comp,"layer":layer,"motion":"pop","start":0.5,"duration":1.5});
    let result = qa.tool("animate_layer", animate.clone());
    assert_eq!(result["timing"]["timeBase"], "comp");
    let opacity = qa.tool("get_property", json!({"comp":comp,"layer":layer,"path":"transform/opacity","time":1.25}));
    assert!((opacity["value"].as_f64().unwrap() - 100.0).abs() < 1e-9);
    assert!(opacity["keys"].as_array().unwrap().iter().any(|key| key["compTime"].as_f64().is_some_and(|t| (t - 0.5).abs() < 1e-9)));
    let first = qa.tool("get_layer", json!({"comp":comp,"layer":layer,"flat":true}));
    let before_failure = qa.tool("get_project", json!({}));
    assert!(qa.try_tool("animate_layer", animate).is_err());
    assert_eq!(qa.tool("get_project", json!({})), before_failure);
    assert_eq!(qa.tool("get_layer", json!({"comp":comp,"layer":layer,"flat":true})), first);
    qa.tool("animate_layer", json!({"comp":comp,"layer":layer,"motion":"squash","start":0.5,"duration":1.5,"replace":true}));
    qa.tool("undo", json!({}));
    assert_eq!(qa.tool("get_layer", json!({"comp":comp,"layer":layer,"flat":true})), first, "one undo restores the original native keys");
}
