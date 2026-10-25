//! Instance-tree browser behind the Explorer and Properties panes.
//!
//! Node shape the UI expects:
//!     { "address": "0x1234", "name": "Workspace", "className": "Workspace", "hasChildren": true }
//! Lookahead keys are decimal address strings; the UI converts them to hex.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use crate::sdk::roblox::{Humanoid, Instance, Part, Roblox, Vector3};

const SEARCH_SERVICES: &[&str] = &[
    "Workspace",
    "Players",
    "ReplicatedStorage",
    "ServerStorage",
    "StarterGui",
    "StarterPack",
    "StarterPlayer",
    "Lighting",
    "SoundService",
    "Chat",
    "Teams",
];

const PART_CLASSES: &[&str] = &["part", "basepart", "meshpart"];

pub struct Explorer {
    objects: Mutex<HashMap<u64, Instance>>,
}

impl Explorer {
    pub fn new() -> Self {
        Self {
            objects: Mutex::new(HashMap::new()),
        }
    }

    pub fn clear(&self) {
        self.objects.lock().clear();
    }

    // -- public ----------------------------------------------------------

    pub fn tree(&self, roblox: &Arc<Roblox>) -> Value {
        let datamodel = match roblox.get_datamodel() {
            Ok(datamodel) => datamodel,
            Err(err) => {
                log::error!(target: "GamingChairLLC.explorer", "Explorer tree failed: {err}");
                return json!({ "nodes": [], "lookahead": {}, "error": err.to_string() });
            }
        };

        let (nodes, lookahead) = self.children_with_lookahead(&datamodel);
        json!({ "nodes": nodes, "lookahead": lookahead })
    }

    pub fn children(&self, roblox: &Arc<Roblox>, address: &str) -> Value {
        let Some(address_value) = parse_address(address) else {
            return json!({
                "address": address,
                "nodes": [],
                "lookahead": {},
                "error": "Invalid address",
            });
        };

        let parent = self.instance(roblox, address_value);
        let (nodes, lookahead) = self.children_with_lookahead(&parent);

        json!({
            "address": address,
            "nodes": nodes,
            "lookahead": lookahead,
        })
    }

    pub fn search(&self, roblox: &Arc<Roblox>, query: &str) -> Value {
        if query.len() < 2 {
            return json!({ "query": query, "results": [] });
        }

        let needle = query.to_lowercase();
        let cached: Vec<Instance> = self.objects.lock().values().cloned().collect();
        let mut results = Vec::new();

        for instance in cached {
            if results.len() >= 100 {
                break;
            }
            if instance.get_name().to_lowercase().contains(&needle) {
                results.push(node(&instance, !instance.get_children().is_empty()));
            }
        }

        if results.len() < 50 {
            if let Ok(datamodel) = roblox.get_datamodel() {
                for service_name in SEARCH_SERVICES {
                    if results.len() >= 100 {
                        break;
                    }
                    if let Some(service) = datamodel.find_first_child_of_name(service_name) {
                        self.search_recursive(&service, &needle, &mut results, 0);
                    }
                }
            }
        }

        json!({ "query": query, "results": results })
    }

    pub fn properties(&self, roblox: &Arc<Roblox>, address: &str) -> Value {
        let Some(address_value) = parse_address(address) else {
            return json!({
                "address": address,
                "properties": {},
                "error": "Invalid address",
            });
        };

        let instance = self.instance(roblox, address_value);
        let class_name = instance.get_class_name();
        let mut properties = Map::new();

        properties.insert("Name".into(), Value::String(instance.get_name()));
        properties.insert("ClassName".into(), Value::String(class_name.clone()));
        properties.insert(
            "Address".into(),
            Value::String(format!("{:#x}", instance.address())),
        );

        match class_name.to_lowercase().as_str() {
            "humanoid" => {
                let humanoid = Humanoid::new(instance);
                properties.insert("WalkSpeed".into(), number(humanoid.walk_speed()));
                properties.insert("JumpPower".into(), number(humanoid.jump_power()));
                properties.insert("Health".into(), number(humanoid.health()));
                properties.insert("MaxHealth".into(), number(humanoid.max_health()));
                properties.insert("State".into(), Value::String(humanoid.state()));
            }
            class if PART_CLASSES.contains(&class) => {
                let part = Part::new(instance);
                if let (Some(position), Some(size)) = (part.position(), part.size()) {
                    properties.insert("Position".into(), vec3(position));
                    properties.insert("Size".into(), vec3(size));
                    properties.insert("Anchored".into(), boolean(part.anchored()));
                    properties.insert("CanCollide".into(), boolean(part.can_collide()));
                    properties.insert("Massless".into(), boolean(part.massless()));
                }
            }
            _ => {}
        }

        // The UI renders strings; match the Python `str(value)` conversion.
        let properties: Map<String, Value> = properties
            .into_iter()
            .map(|(key, value)| (key, Value::String(value_to_string(&value))))
            .collect();

        json!({ "address": address, "properties": properties })
    }

    // -- internals -------------------------------------------------------

    fn instance(&self, roblox: &Arc<Roblox>, address: u64) -> Instance {
        if let Some(instance) = self.objects.lock().get(&address) {
            return instance.clone();
        }

        let instance = roblox.instance(address);
        self.objects.lock().insert(address, instance.clone());
        instance
    }

    fn children_with_lookahead(&self, parent: &Instance) -> (Vec<Value>, Map<String, Value>) {
        let mut nodes = Vec::new();
        let mut lookahead = Map::new();

        for child in parent.get_children() {
            // One extra read per child, reused for both `hasChildren` and the
            // lookahead list.
            let grandchildren = child.get_children();
            nodes.push(node(&child, !grandchildren.is_empty()));
            self.objects.lock().insert(child.address(), child.clone());

            let mut grand_nodes = Vec::with_capacity(grandchildren.len());
            for grandchild in &grandchildren {
                grand_nodes.push(node(grandchild, !grandchild.get_children().is_empty()));
                self.objects
                    .lock()
                    .insert(grandchild.address(), grandchild.clone());
            }

            lookahead.insert(child.address().to_string(), Value::Array(grand_nodes));
        }

        (nodes, lookahead)
    }

    fn search_recursive(
        &self,
        instance: &Instance,
        needle: &str,
        results: &mut Vec<Value>,
        depth: usize,
    ) {
        if results.len() >= 100 || depth > 10 {
            return;
        }

        if instance.get_name().to_lowercase().contains(needle) {
            results.push(node(instance, !instance.get_children().is_empty()));
        }

        for child in instance.get_children() {
            if results.len() >= 100 {
                return;
            }
            self.search_recursive(&child, needle, results, depth + 1);
        }
    }
}

impl Default for Explorer {
    fn default() -> Self {
        Self::new()
    }
}

fn node(instance: &Instance, has_children: bool) -> Value {
    json!({
        "address": format!("{:#x}", instance.address()),
        "name": instance.get_name(),
        "className": instance.get_class_name(),
        "hasChildren": has_children,
    })
}

fn parse_address(address: &str) -> Option<u64> {
    let address = address.trim();
    if let Some(hex) = address
        .strip_prefix("0x")
        .or_else(|| address.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        address.parse::<u64>().ok()
    }
}

fn number(value: f32) -> Value {
    Value::String(format!("{value:?}"))
}

fn boolean(value: bool) -> Value {
    Value::String(if value { "true" } else { "false" }.to_string())
}

fn vec3(value: Vector3) -> Value {
    Value::String(value.to_string())
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(value) => (if *value { "True" } else { "False" }).to_string(),
        Value::Number(number) => number.to_string(),
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}
