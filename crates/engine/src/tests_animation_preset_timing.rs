//! User animation presets use one key origin for selected properties and complete effects.

use std::sync::Arc;

use effectcraft_keyframe::{Ease, Interp, Keyframe, Value as KV};
use effectcraft_project::{ItemId, Layer, LayerId, Node, PropGroup, Property};
use effectcraft_time::Tick;
use serde_json::{Value, json};

use crate::Session;

fn seconds(t: f64) -> Tick {
    Tick::from_seconds_f64(t)
}

fn layer(s: &Session, id: u64) -> &Layer {
    s.active_comp().unwrap().layer(LayerId(id)).unwrap()
}

fn rich_key(t: f64, value: f64) -> Keyframe {
    let mut key = Keyframe::new(seconds(t), KV::Scalar(value)).eased();
    key.in_interp = Interp::Hold;
    key.in_ease = vec![Ease { speed: 2.5, influence: 0.2 }];
    key.out_ease = vec![Ease { speed: 7.5, influence: 0.7 }];
    key.continuous = true;
    key.spatial_in = [-1.0, -2.0, 0.0];
    key.spatial_out = [3.0, 4.0, 0.0];
    key.spatial_auto = false;
    key.spatial_continuous = true;
    key.roving = true;
    key.label = 3;
    key
}

fn fixture() -> (Session, u64, u64) {
    let mut s = Session::default();
    s.execute("comp.new", json!({"width": 1920, "height": 1080, "frameRate": 30, "duration": 20})).unwrap();
    let source = s.execute("layer.newSolid", json!({"name": "Source"})).unwrap()["layer"].as_u64().unwrap();
    let effect = s.execute("effect.apply", json!({"layer": source, "effect": "Slider Control"})).unwrap()["effects"][0].as_u64().unwrap();
    let cid = s.active_comp_id().unwrap();
    let project = Arc::make_mut(&mut s.project);
    let group_uid = project.alloc();
    let property_uid = project.alloc();
    let effect_group = project.comp_mut(cid).unwrap().layer_mut(LayerId(source)).unwrap().props.group_mut("effects/#1").unwrap();
    let slider = effect_group.get_mut("slider").unwrap();
    slider.value = KV::Scalar(77.0);
    slider.keys = vec![rich_key(2.0, 20.0), rich_key(3.0, 30.0)];
    // Effects can contain nested groups. Their keys share the preset origin too.
    let mut nested = Property::new(property_uid, "amount", "Nested Amount", KV::Scalar(88.0));
    nested.keys = vec![rich_key(1.0, 10.0), rich_key(4.0, 40.0)];
    effect_group.children.push(Node::Group(PropGroup::new(group_uid, "nested", "Nested").with(nested)));
    (s, source, effect)
}

fn save(s: &mut Session, name: &str) -> Value {
    let path = std::env::temp_dir().join(format!("ec-user-preset-timing-{}-{name}.ecpreset", std::process::id()));
    s.execute("anim.savePreset", json!({"path": path})).unwrap();
    let preset = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    std::fs::remove_file(path).unwrap();
    preset
}

fn normalized_effect(group: &PropGroup, first: Tick) -> PropGroup {
    let mut expected = group.clone();
    expected.walk_mut(&mut |property| {
        for key in &mut property.keys {
            key.time -= first;
        }
    });
    expected
}

fn canonical_uids(group: &PropGroup) -> PropGroup {
    let mut group = group.clone();
    group.reassign_uids(&mut 1);
    group
}

#[test]
fn user_preset_save_normalizes_nested_effect_and_leaf_keys_to_one_origin() {
    // Exercise both orderings: either the leaf or an effect's nested property starts first.
    for leaf_start in [0.5_f64, 1.5] {
        let (mut s, source, effect) = fixture();
        s.execute("prop.addKey", json!({"layer": source, "path": "transform/opacity", "time": leaf_start, "value": 10})).unwrap();
        s.execute("prop.addKey", json!({"layer": source, "path": "transform/opacity", "time": leaf_start + 1.0, "value": 90})).unwrap();
        let original = layer(&s, source).clone();
        s.execute("prop.select", json!({"layer": source, "prop": effect})).unwrap();
        s.execute("prop.select", json!({"layer": source, "path": "transform/opacity", "add": true})).unwrap();
        // Save CTI must not replace the earliest selected key as the origin.
        s.execute("time.set", json!({"time": 9.0})).unwrap();
        let preset = save(&mut s, if leaf_start < 1.0 { "leaf-first" } else { "nested-first" });
        let first = seconds(leaf_start.min(1.0));
        let saved_effect: PropGroup = serde_json::from_value(preset["effects"][0].clone()).unwrap();
        assert_eq!(saved_effect, normalized_effect(original.props.group("effects/#1").unwrap(), first));
        let mut expected = original.props.prop("transform/opacity").unwrap().keys.clone();
        for key in &mut expected {
            key.time -= first;
        }
        let saved_keys: Vec<Keyframe> = serde_json::from_value(preset["properties"][0]["keys"].clone()).unwrap();
        assert_eq!(saved_keys, expected);
        assert_eq!(layer(&s, source), &original, "saving must not mutate source keys or defaults");
    }
}

#[test]
fn user_preset_save_normalizes_an_effect_without_selected_leaf_properties() {
    let (mut s, source, effect) = fixture();
    let original = layer(&s, source).props.group("effects/#1").unwrap().clone();
    s.execute("prop.select", json!({"layer": source, "prop": effect})).unwrap();
    let preset = save(&mut s, "effect-only");
    assert_eq!(preset["properties"], json!([]));
    let saved: PropGroup = serde_json::from_value(preset["effects"][0].clone()).unwrap();
    assert_eq!(saved, normalized_effect(&original, seconds(1.0)));
    assert_eq!(saved.get("slider").unwrap().value, KV::Scalar(77.0));
    assert_eq!(saved.prop("nested/amount").unwrap().value, KV::Scalar(88.0));
}

#[test]
fn user_preset_apply_offsets_nested_effect_keys_for_each_target_and_undoes_once() {
    let (mut s, source, _) = fixture();
    let original = layer(&s, source).props.group("effects/#1").unwrap().clone();
    let relative = normalized_effect(&original, seconds(1.0));
    let mut targets = vec![];
    for (start, stretch) in [(2.0, 100.0), (2.0, 200.0), (12.0, -200.0)] {
        let target = s.execute("layer.newSolid", json!({})).unwrap()["layer"].as_u64().unwrap();
        s.execute("effect.apply", json!({"layer": target, "effect": "Slider Control"})).unwrap();
        let cid = s.active_comp_id().unwrap();
        let target_layer = Arc::make_mut(&mut s.project).comp_mut(cid).unwrap().layer_mut(LayerId(target)).unwrap();
        target_layer.start_time = seconds(start);
        target_layer.stretch = stretch;
        targets.push(target);
    }
    s.execute("time.set", json!({"time": 6.0})).unwrap();
    let before = s.project.to_json();
    let result = s
        .execute(
            "anim.applyPreset",
            json!({
                "layers": targets,
                "preset": {"effectcraftAnimationPreset": 1, "properties": [{
                    "path": "transform/opacity", "value": {"t": "Scalar", "v": 100},
                    "keys": [{"time": 0, "value": {"t": "Scalar", "v": 20}}], "expression": null
                }], "effects": [relative]}
            }),
        )
        .unwrap();
    assert_eq!(result, json!({"applied": 6, "skipped": []}));
    let mut copied_uids = vec![];
    for target in targets {
        let target_layer = layer(&s, target);
        let at = target_layer.layer_time(seconds(6.0));
        let actual = target_layer.props.group("effects/#2").unwrap();
        let mut expected = relative.clone();
        expected.walk_mut(&mut |property| {
            for key in &mut property.keys {
                key.time += at;
            }
        });
        assert_eq!(canonical_uids(actual), canonical_uids(&expected), "target {target}");
        assert_ne!(actual.uid, original.uid);
        assert_ne!(actual.get("slider").unwrap().uid, original.get("slider").unwrap().uid);
        assert!(!copied_uids.contains(&actual.uid));
        copied_uids.push(actual.uid);
        assert_eq!(target_layer.props.prop("transform/opacity").unwrap().keys[0].time, at);
        assert_eq!(actual.prop("nested/amount").unwrap().keys[0].time, at);
        assert_eq!(target_layer.comp_time(at), seconds(6.0), "first key lands at the composition CTI");
        assert_eq!(target_layer.props.group("effects/#1").unwrap().get("slider").unwrap().value, KV::Scalar(0.0));
    }
    let after = s.project.to_json();
    s.undo();
    assert_eq!(s.project.to_json(), before);
    s.redo();
    assert_eq!(s.project.to_json(), after);
}

#[test]
fn user_preset_effect_time_offsets_saturate_at_tick_limits() {
    let (mut s, source, effect) = fixture();
    let cid: ItemId = s.active_comp_id().unwrap();
    let source_layer = Arc::make_mut(&mut s.project).comp_mut(cid).unwrap().layer_mut(LayerId(source)).unwrap();
    let effect_group = source_layer.props.group_mut("effects/#1").unwrap();
    effect_group.walk_mut(&mut |property| property.keys.clear());
    let slider = effect_group.get_mut("slider").unwrap();
    slider.keys = vec![Keyframe::new(Tick(i64::MIN), KV::Scalar(10.0)), Keyframe::new(Tick(i64::MAX), KV::Scalar(20.0))];
    let raw = effect_group.clone();
    s.execute("prop.select", json!({"layer": source, "prop": effect})).unwrap();
    let saved: PropGroup = serde_json::from_value(save(&mut s, "limits")["effects"][0].clone()).unwrap();
    assert_eq!(saved.get("slider").unwrap().keys[0].time, Tick::ZERO);
    assert_eq!(saved.get("slider").unwrap().keys[1].time, Tick(i64::MAX));
    for (name, start, expected_first, expected_last) in
        [("Positive", 0.0, Tick(i64::MIN + seconds(6.0).0), Tick(i64::MAX)), ("Negative", 10.0, Tick(i64::MIN), Tick(i64::MAX - seconds(4.0).0))]
    {
        let target = s.execute("layer.newSolid", json!({"name": name})).unwrap()["layer"].as_u64().unwrap();
        Arc::make_mut(&mut s.project).comp_mut(cid).unwrap().layer_mut(LayerId(target)).unwrap().start_time = seconds(start);
        s.execute("time.set", json!({"time": 6.0})).unwrap();
        s.execute("anim.applyPreset", json!({"layers": [target], "preset": {"effectcraftAnimationPreset": 1, "effects": [raw]}})).unwrap();
        let keys = &layer(&s, target).props.group("effects/#1").unwrap().get("slider").unwrap().keys;
        assert_eq!(keys[0].time, expected_first);
        assert_eq!(keys[1].time, expected_last);
    }
}
