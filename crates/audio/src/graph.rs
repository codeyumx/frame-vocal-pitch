//! Pure (PipeWire-free) model of the parts of the PipeWire graph this crate cares about,
//! plus the decision logic of SPEC §6.3: hardware-source resolution, channel matching and
//! the read-only "are Valve's mic filters active" query. Everything here is unit tested
//! against hand-built snapshots.

use std::collections::BTreeMap;
use std::fmt;

/// Upper bound on link-group hops while resolving the hardware source (guards against cycles).
const MAX_RESOLVE_HOPS: usize = 8;

/// A PipeWire node as seen in the registry (only the properties we use).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Node {
    pub id: u32,
    pub name: Option<String>,
    pub description: Option<String>,
    pub nick: Option<String>,
    pub media_class: Option<String>,
    pub link_group: Option<String>,
    pub target_object: Option<String>,
    pub object_serial: Option<String>,
    /// `steamos.mic_filter = true`
    pub mic_filter: bool,
    /// `filter.smart.disabled` node property (the metadata value overrides it).
    pub smart_disabled_prop: Option<bool>,
}

impl Node {
    /// Human readable label: description, nick, name, or the id.
    pub fn label(&self) -> String {
        self.description
            .as_deref()
            .or(self.nick.as_deref())
            .or(self.name.as_deref())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("node {}", self.id))
    }

    fn is_input_stream(&self) -> bool {
        self.media_class
            .as_deref()
            .is_some_and(|c| c.starts_with("Stream/Input"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    In,
    Out,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Port {
    pub id: u32,
    pub node_id: u32,
    pub direction: Direction,
    /// `audio.channel` (e.g. `FL`, `MONO`, `AUX0`).
    pub channel: Option<String>,
    /// `port.id` (index within the node and direction); used for ordering.
    pub index: u32,
    pub monitor: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    pub id: u32,
    pub output_node: u32,
    pub output_port: u32,
    pub input_node: u32,
    pub input_port: u32,
}

/// Snapshot of the registry plus the two metadata objects we read.
#[derive(Clone, Debug, Default)]
pub(crate) struct Graph {
    pub nodes: BTreeMap<u32, Node>,
    pub ports: BTreeMap<u32, Port>,
    pub links: BTreeMap<u32, Link>,
    /// `node.name` from `default.audio.source` in the `default` metadata.
    pub default_source: Option<String>,
    /// `filter.smart.disabled` entries of the `filters` metadata, by subject (node id).
    pub filters_disabled: BTreeMap<u32, String>,
}

impl Graph {
    pub fn remove(&mut self, id: u32) {
        self.nodes.remove(&id);
        self.ports.remove(&id);
        self.links.remove(&id);
        self.filters_disabled.remove(&id);
    }

    pub fn node_by_name(&self, name: &str) -> Option<&Node> {
        self.nodes.values().find(|n| n.name.as_deref() == Some(name))
    }

    /// `target.object` may hold a `node.name`, an `object.serial` or (legacy) an object id.
    fn node_by_target(&self, target: &str) -> Option<&Node> {
        self.node_by_name(target)
            .or_else(|| {
                self.nodes
                    .values()
                    .find(|n| n.object_serial.as_deref() == Some(target))
            })
            .or_else(|| target.parse::<u32>().ok().and_then(|id| self.nodes.get(&id)))
    }

    /// Output ports of `node_id` (monitor ports excluded), ordered by port index.
    pub fn output_ports(&self, node_id: u32) -> Vec<&Port> {
        self.ports_of(node_id, Direction::Out)
    }

    /// Input ports of `node_id`, ordered by port index.
    pub fn input_ports(&self, node_id: u32) -> Vec<&Port> {
        self.ports_of(node_id, Direction::In)
    }

    fn ports_of(&self, node_id: u32, direction: Direction) -> Vec<&Port> {
        let mut ports: Vec<&Port> = self
            .ports
            .values()
            .filter(|p| p.node_id == node_id && p.direction == direction && !p.monitor)
            .collect();
        ports.sort_by_key(|p| (p.index, p.id));
        ports
    }

    pub fn has_link(&self, output_port: u32, input_port: u32) -> bool {
        self.links
            .values()
            .any(|l| l.output_port == output_port && l.input_port == input_port)
    }

    /// Nodes that feed `node_id` through links (deduplicated, id order).
    pub fn upstream_of(&self, node_id: u32) -> Vec<&Node> {
        let mut ids: Vec<u32> = self
            .links
            .values()
            .filter(|l| l.input_node == node_id)
            .map(|l| l.output_node)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.iter().filter_map(|id| self.nodes.get(id)).collect()
    }
}

/// The device node the app records from in bypass mode.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HwSource {
    pub node_id: u32,
    pub name: String,
    pub description: String,
    /// `(port id, audio.channel)` of every output port, in port order.
    pub ports: Vec<(u32, String)>,
}

impl HwSource {
    pub fn channels(&self) -> Vec<String> {
        self.ports.iter().map(|(_, c)| c.clone()).collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ResolveError {
    NoDefaultSource,
    DefaultSourceMissing(String),
    ChainBroken(String),
    TooDeep,
    NoPorts(String),
    UnnamedChannel(String),
    UnknownChannel(String, String),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDefaultSource => write!(f, "no default source"),
            Self::DefaultSourceMissing(n) => write!(f, "default source {n} not found"),
            Self::ChainBroken(n) => write!(f, "hardware mic node not found (behind {n})"),
            Self::TooDeep => write!(f, "hardware mic node not found (link-group loop)"),
            Self::NoPorts(n) => write!(f, "hardware mic node {n} has no output ports"),
            Self::UnnamedChannel(n) => write!(f, "hardware mic node {n} has a port without audio.channel"),
            Self::UnknownChannel(n, c) => write!(f, "hardware mic node {n} has unsupported channel {c}"),
        }
    }
}

/// SPEC §6.3 step 1: start at `default.audio.source`; while the node belongs to a
/// `node.link-group` whose input-stream node has `target.object`, follow it.
pub(crate) fn resolve_hardware_source(graph: &Graph) -> Result<HwSource, ResolveError> {
    let default = graph
        .default_source
        .as_deref()
        .ok_or(ResolveError::NoDefaultSource)?;
    let mut cur = graph
        .node_by_name(default)
        .ok_or_else(|| ResolveError::DefaultSourceMissing(default.to_owned()))?;
    let mut hops = 0;
    while let Some(group) = cur.link_group.as_deref() {
        hops += 1;
        if hops > MAX_RESOLVE_HOPS {
            return Err(ResolveError::TooDeep);
        }
        let next = graph
            .nodes
            .values()
            .filter(|n| n.id != cur.id && n.link_group.as_deref() == Some(group) && n.is_input_stream())
            .filter_map(|n| n.target_object.as_deref())
            .filter_map(|t| graph.node_by_target(t))
            .find(|n| n.link_group.as_deref() != Some(group));
        cur = next.ok_or_else(|| ResolveError::ChainBroken(cur.label()))?;
    }
    let name = cur.name.clone().unwrap_or_else(|| cur.id.to_string());
    let ports = graph.output_ports(cur.id);
    if ports.is_empty() {
        return Err(ResolveError::NoPorts(name));
    }
    let mut out = Vec::with_capacity(ports.len());
    for p in ports {
        let ch = p
            .channel
            .clone()
            .ok_or_else(|| ResolveError::UnnamedChannel(name.clone()))?;
        if channel_position(&ch).is_none() {
            return Err(ResolveError::UnknownChannel(name, ch));
        }
        out.push((p.id, ch));
    }
    Ok(HwSource {
        node_id: cur.id,
        description: cur.label(),
        name,
        ports: out,
    })
}

/// SPEC §6.3 step 3: pair each hardware output port with the stream input port carrying the
/// same `audio.channel`. Returns `(hw port id, stream port id)` pairs, or the first channel
/// that has no partner.
pub(crate) fn match_channels(hw: &[(u32, String)], stream: &[&Port]) -> Result<Vec<(u32, u32)>, String> {
    hw.iter()
        .map(|(hw_port, ch)| {
            stream
                .iter()
                .find(|p| p.channel.as_deref() == Some(ch.as_str()))
                .map(|p| (*hw_port, p.id))
                .ok_or_else(|| ch.clone())
        })
        .collect()
}

/// A Valve mic filter node (`steamos.mic_filter = true`) and its effective state.
#[derive(Clone, Debug, PartialEq)]
pub struct MicFilter {
    pub node_id: u32,
    pub name: String,
    /// Effective `filter.smart.disabled`: metadata entry, else node property, else false.
    pub disabled: bool,
    /// Raw value from the `filters` metadata, if the key is set for this node.
    pub metadata_value: Option<String>,
}

pub(crate) fn mic_filters(graph: &Graph) -> Vec<MicFilter> {
    graph
        .nodes
        .values()
        .filter(|n| n.mic_filter)
        .map(|n| {
            let metadata_value = graph.filters_disabled.get(&n.id).cloned();
            let disabled = match metadata_value.as_deref() {
                Some(v) => parse_bool(v),
                None => n.smart_disabled_prop.unwrap_or(false),
            };
            MicFilter {
                node_id: n.id,
                name: n.name.clone().unwrap_or_else(|| n.label()),
                disabled,
                metadata_value,
            }
        })
        .collect()
}

/// Whether Valve's mic filters are currently enabled for other apps; `None` when the graph
/// has no `steamos.mic_filter` nodes (e.g. a normal PC).
pub(crate) fn filters_active(graph: &Graph) -> Option<bool> {
    let filters = mic_filters(graph);
    if filters.is_empty() {
        None
    } else {
        Some(filters.iter().any(|f| !f.disabled))
    }
}

/// PipeWire/SPA-JSON booleans: `true` (optionally quoted) or `1`.
pub(crate) fn parse_bool(v: &str) -> bool {
    matches!(v.trim().trim_matches('"'), "true" | "1")
}

/// Extracts `name` from the `default.audio.source` metadata value (`{ "name": "…" }`).
/// Accepts both strict JSON and SPA-JSON (unquoted keys, `=` separator).
pub(crate) fn parse_default_name(value: &str) -> Option<String> {
    let rest = &value[value.find("name")? + 4..];
    let rest = rest.strip_prefix('"').unwrap_or(rest).trim_start();
    let rest = rest.strip_prefix([':', '=']).unwrap_or(rest).trim_start();
    if let Some(body) = rest.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out).filter(|s| !s.is_empty()),
                '\\' => out.push(chars.next()?),
                c => out.push(c),
            }
        }
        None
    } else {
        let end = rest.find([',', '}', ' ', '\n', '\t']).unwrap_or(rest.len());
        Some(rest[..end].to_owned()).filter(|s| !s.is_empty())
    }
}

/// Short SPA channel names, indexed by `enum spa_audio_channel` value (spa/param/audio/raw.h).
const CHANNEL_NAMES: [&str; 38] = [
    "UNK", "NA", "MONO", "FL", "FR", "FC", "LFE", "SL", "SR", "FLC", "FRC", "RC", "RL", "RR", "TC", "TFL",
    "TFC", "TFR", "TRL", "TRC", "TRR", "RLC", "RRC", "FLW", "FRW", "LFE2", "FLH", "FCH", "FRH", "TFLC",
    "TFRC", "TSL", "TSR", "LLFE", "RLFE", "BC", "BLC", "BRC",
];
const CHANNEL_AUX0: u32 = 0x1000;
const CHANNEL_AUX_COUNT: u32 = 64;

/// `SPA_AUDIO_CHANNEL_MONO` (spa/param/audio/raw.h); mono streams use this position.
pub(crate) const CHANNEL_MONO: u32 = 2;

/// Maps an `audio.channel` name to its `spa_audio_channel` position (named positions from
/// `MONO` upward and `AUX0`..`AUX63`).
pub(crate) fn channel_position(name: &str) -> Option<u32> {
    if let Some(n) = name.strip_prefix("AUX") {
        return n
            .parse::<u32>()
            .ok()
            .filter(|n| *n < CHANNEL_AUX_COUNT)
            .map(|n| CHANNEL_AUX0 + n);
    }
    CHANNEL_NAMES
        .iter()
        .position(|c| *c == name)
        .filter(|&i| i >= 2)
        .map(|i| i as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u32, name: &str, class: &str) -> Node {
        Node {
            id,
            name: Some(name.into()),
            description: Some(format!("{name} desc")),
            media_class: Some(class.into()),
            object_serial: Some((id + 1000).to_string()),
            ..Node::default()
        }
    }

    fn port(id: u32, node_id: u32, direction: Direction, ch: &str, index: u32) -> Port {
        Port {
            id,
            node_id,
            direction,
            channel: Some(ch.into()),
            index,
            monitor: false,
        }
    }

    const HW: &str = "alsa_input.platform-sound.HiFi__Mic__source";
    const WRAP: &str = "alsa_loopback_device.alsa_input.platform-sound.HiFi__Mic__source";

    /// The Steam Frame topology from SPEC §2: wrapper source + its loopback stream targeting
    /// the 2-channel hardware node, plus three mic filters.
    fn frame_graph() -> Graph {
        let mut g = Graph::default();
        let mut hw = node(40, HW, "Audio/Source");
        hw.description = Some("Mic hardware".into());
        g.nodes.insert(40, hw);
        let mut wrap = node(50, WRAP, "Audio/Source");
        wrap.link_group = Some("loopback-1-23".into());
        g.nodes.insert(50, wrap);
        let mut wrap_stream = node(51, "input.loopback", "Stream/Input/Audio");
        wrap_stream.link_group = Some("loopback-1-23".into());
        wrap_stream.target_object = Some(HW.into());
        g.nodes.insert(51, wrap_stream);
        for (id, name) in [(60, "eq_mic"), (61, "echo_cancel_mic"), (62, "ns_mic")] {
            let mut f = node(id, name, "Audio/Source");
            f.mic_filter = true;
            g.nodes.insert(id, f);
        }
        // Hardware ports deliberately inserted out of index order.
        g.ports.insert(101, port(101, 40, Direction::Out, "AUX1", 1));
        g.ports.insert(100, port(100, 40, Direction::Out, "AUX0", 0));
        g.ports.insert(102, Port { monitor: true, ..port(102, 40, Direction::Out, "AUX0", 2) });
        g.default_source = Some(WRAP.into());
        g
    }

    #[test]
    fn resolves_frame_wrapper_to_hardware_node() {
        let hw = resolve_hardware_source(&frame_graph()).unwrap();
        assert_eq!(hw.node_id, 40);
        assert_eq!(hw.name, HW);
        assert_eq!(hw.description, "Mic hardware");
        assert_eq!(hw.ports, vec![(100, "AUX0".into()), (101, "AUX1".into())]);
    }

    #[test]
    fn plain_default_source_is_itself() {
        let mut g = Graph::default();
        g.nodes.insert(7, node(7, "alsa_input.usb-mic", "Audio/Source"));
        g.ports.insert(8, port(8, 7, Direction::Out, "MONO", 0));
        g.default_source = Some("alsa_input.usb-mic".into());
        let hw = resolve_hardware_source(&g).unwrap();
        assert_eq!(hw.node_id, 7);
        assert_eq!(hw.channels(), vec!["MONO".to_string()]);
    }

    #[test]
    fn follows_nested_link_groups_and_serial_targets() {
        let mut g = frame_graph();
        // An extra virtual source whose stream targets the wrapper by object.serial.
        let mut outer = node(70, "virtual.mic", "Audio/Source");
        outer.link_group = Some("group-b".into());
        g.nodes.insert(70, outer);
        let mut outer_stream = node(71, "virtual.mic.capture", "Stream/Input/Audio");
        outer_stream.link_group = Some("group-b".into());
        outer_stream.target_object = Some("1050".into()); // serial of node 50
        g.nodes.insert(71, outer_stream);
        g.default_source = Some("virtual.mic".into());
        assert_eq!(resolve_hardware_source(&g).unwrap().node_id, 40);
    }

    #[test]
    fn resolution_errors() {
        let mut g = frame_graph();
        g.default_source = None;
        assert_eq!(resolve_hardware_source(&g), Err(ResolveError::NoDefaultSource));

        g.default_source = Some("gone".into());
        assert_eq!(
            resolve_hardware_source(&g),
            Err(ResolveError::DefaultSourceMissing("gone".into()))
        );

        // Hardware node unplugged: the wrapper's stream points at nothing.
        let mut g = frame_graph();
        g.remove(40);
        assert!(matches!(resolve_hardware_source(&g), Err(ResolveError::ChainBroken(_))));
        assert!(resolve_hardware_source(&g)
            .unwrap_err()
            .to_string()
            .starts_with("hardware mic node not found"));

        // Hardware node present but its ports are gone.
        let mut g = frame_graph();
        g.ports.clear();
        assert_eq!(resolve_hardware_source(&g), Err(ResolveError::NoPorts(HW.into())));

        let mut g = frame_graph();
        g.ports.get_mut(&100).unwrap().channel = Some("WEIRD".into());
        assert_eq!(
            resolve_hardware_source(&g),
            Err(ResolveError::UnknownChannel(HW.into(), "WEIRD".into()))
        );
    }

    #[test]
    fn link_group_cycle_is_bounded() {
        let mut g = Graph::default();
        for (id, group, target) in [(1, "a", "s2"), (3, "b", "s1")] {
            let mut src = node(id, &format!("s{}", id.div_ceil(2)), "Audio/Source");
            src.link_group = Some(group.into());
            g.nodes.insert(id, src);
            let mut st = node(id + 1, &format!("st{id}"), "Stream/Input/Audio");
            st.link_group = Some(group.into());
            st.target_object = Some(target.into());
            g.nodes.insert(id + 1, st);
        }
        g.default_source = Some("s1".into());
        assert_eq!(resolve_hardware_source(&g), Err(ResolveError::TooDeep));
    }

    #[test]
    fn channel_matching_by_audio_channel() {
        let s0 = port(200, 90, Direction::In, "AUX1", 1);
        let s1 = port(201, 90, Direction::In, "AUX0", 0);
        let stream = [&s0, &s1];
        let hw = [(100, "AUX0".to_string()), (101, "AUX1".to_string())];
        assert_eq!(match_channels(&hw, &stream), Ok(vec![(100, 201), (101, 200)]));
        let hw = [(100, "FL".to_string())];
        assert_eq!(match_channels(&hw, &stream), Err("FL".into()));
    }

    #[test]
    fn filter_status() {
        let mut g = frame_graph();
        // No metadata entries: smart filters default to enabled.
        assert_eq!(filters_active(&g), Some(true));
        for id in [60, 61, 62] {
            g.filters_disabled.insert(id, "true".into());
        }
        assert_eq!(filters_active(&g), Some(false));
        g.filters_disabled.insert(61, "false".into());
        assert_eq!(filters_active(&g), Some(true));
        let list = mic_filters(&g);
        assert_eq!(list.len(), 3);
        assert_eq!(list[1].metadata_value.as_deref(), Some("false"));

        // Node property is used when the metadata has no entry.
        g.filters_disabled.clear();
        for id in [60, 61, 62] {
            g.nodes.get_mut(&id).unwrap().smart_disabled_prop = Some(true);
        }
        assert_eq!(filters_active(&g), Some(false));

        // A normal PC has no Valve mic filters.
        let mut pc = Graph::default();
        pc.nodes.insert(7, node(7, "alsa_input.usb-mic", "Audio/Source"));
        assert_eq!(filters_active(&pc), None);
    }

    #[test]
    fn upstream_and_links() {
        let mut g = frame_graph();
        g.nodes.insert(90, node(90, "frame-vocal-pitch-capture", "Stream/Input/Audio"));
        g.links.insert(
            300,
            Link { id: 300, output_node: 40, output_port: 100, input_node: 90, input_port: 201 },
        );
        assert!(g.has_link(100, 201));
        assert!(!g.has_link(101, 200));
        let up: Vec<u32> = g.upstream_of(90).iter().map(|n| n.id).collect();
        assert_eq!(up, vec![40]);
        g.remove(300);
        assert!(g.upstream_of(90).is_empty());
    }

    #[test]
    fn default_metadata_value_parsing() {
        assert_eq!(parse_default_name(r#"{"name":"alsa_input.x"}"#).as_deref(), Some("alsa_input.x"));
        assert_eq!(parse_default_name(r#"{ "name": "a\"b" }"#).as_deref(), Some("a\"b"));
        assert_eq!(parse_default_name("{ name = alsa_input.y }").as_deref(), Some("alsa_input.y"));
        assert_eq!(parse_default_name("{}"), None);
        assert_eq!(parse_default_name(r#"{"name":""}"#), None);
    }

    #[test]
    fn channel_positions() {
        assert_eq!(channel_position("MONO"), Some(2));
        assert_eq!(channel_position("FL"), Some(3));
        assert_eq!(channel_position("FR"), Some(4));
        assert_eq!(channel_position("BRC"), Some(37));
        assert_eq!(channel_position("AUX0"), Some(0x1000));
        assert_eq!(channel_position("AUX63"), Some(0x103f));
        assert_eq!(channel_position("AUX64"), None);
        assert_eq!(channel_position("UNK"), None);
        assert_eq!(channel_position("NA"), None);
        assert_eq!(channel_position("XX"), None);
        assert!(parse_bool("true") && parse_bool("\"true\"") && parse_bool("1"));
        assert!(!parse_bool("false"));
    }
}
