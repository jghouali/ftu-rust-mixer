use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlKind {
    Integer {
        min: i64,
        max: i64,
        step: i64,
        channels: usize,
        #[serde(default)]
        db_range: Option<(i64, i64)>,
    },
    Boolean {
        channels: usize,
    },
    Enumerated {
        items: Vec<String>,
        channels: usize,
    },
    Unknown {
        type_name: String,
        channels: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlDescriptor {
    pub numid: u32,
    pub name: String,
    pub iface: String,
    pub index: u32,
    pub device: u32,
    pub subdevice: u32,
    pub kind: ControlKind,
    pub values: Vec<String>,
    pub grouped_label: String,
    pub favorite: bool,
}

#[derive(Debug, Clone)]
pub struct RouteRef {
    pub output: usize,
    pub input: usize,
    pub control_index: usize,
}

#[derive(Debug, Clone, Default)]
pub struct RoutingIndex {
    pub analog_routes: Vec<RouteRef>,
    pub digital_routes: Vec<RouteRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetControlValue {
    pub numid: u32,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetFile {
    pub schema_version: u32,
    pub card_name: String,
    pub controls: Vec<PresetControlValue>,
}
