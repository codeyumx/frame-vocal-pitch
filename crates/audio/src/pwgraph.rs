//! Registry watcher: mirrors nodes (full info props), ports, links and the `default` /
//! `filters` metadata into a [`Graph`]. Read-only: never writes metadata.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use pipewire as pw;
use pw::metadata::{Metadata, MetadataListener};
use pw::node::{NodeChangeMask, NodeInfoRef, NodeListener};
use pw::registry::{GlobalObject, RegistryRc};
use pw::spa::utils::dict::DictRef;
use pw::types::ObjectType;

use crate::graph::{parse_bool, parse_default_name, Direction, Graph, Link, Node, Port};

const KEY_DEFAULT_SOURCE: &str = "default.audio.source";
const KEY_SMART_DISABLED: &str = "filter.smart.disabled";

/// Graph snapshot plus change bookkeeping for debounced reconciliation.
#[derive(Default)]
pub(crate) struct WatchState {
    pub graph: Graph,
    /// First change not yet consumed by [`WatchState::take_change`].
    pub dirty_since: Option<Instant>,
    /// Most recent change.
    pub last_change: Option<Instant>,
}

impl WatchState {
    fn touch(&mut self) {
        let now = Instant::now();
        self.dirty_since.get_or_insert(now);
        self.last_change = Some(now);
    }

    /// Clears the dirty flag and returns a copy of the graph.
    pub fn take_change(&mut self) -> Graph {
        self.dirty_since = None;
        self.graph.clone()
    }
}

#[derive(Default)]
struct Bound {
    nodes: HashMap<u32, (pw::node::Node, NodeListener)>,
    metadata: HashMap<u32, (Metadata, MetadataListener)>,
}

/// Keeps the registry listener and bound proxies alive; drop to stop watching.
pub(crate) struct Watch {
    pub state: Rc<RefCell<WatchState>>,
    _listener: pw::registry::Listener,
    _bound: Rc<RefCell<Bound>>,
}

impl Watch {
    pub fn new(registry: &RegistryRc) -> Watch {
        let state = Rc::new(RefCell::new(WatchState::default()));
        let bound = Rc::new(RefCell::new(Bound::default()));
        let weak_registry = registry.downgrade();
        let listener = registry
            .add_listener_local()
            .global({
                let state = state.clone();
                let bound = bound.clone();
                move |global| {
                    if let Some(registry) = weak_registry.upgrade() {
                        on_global(&registry, global, &state, &bound);
                    }
                }
            })
            .global_remove({
                let state = state.clone();
                let bound = bound.clone();
                move |id| {
                    // Drop proxies after releasing the borrow: destroying a proxy may emit events.
                    let removed = {
                        let mut b = bound.borrow_mut();
                        (b.nodes.remove(&id), b.metadata.remove(&id))
                    };
                    drop(removed);
                    let mut s = state.borrow_mut();
                    s.graph.remove(id);
                    s.touch();
                }
            })
            .register();
        Watch {
            state,
            _listener: listener,
            _bound: bound,
        }
    }
}

fn on_global(
    registry: &RegistryRc,
    global: &GlobalObject<&DictRef>,
    state: &Rc<RefCell<WatchState>>,
    bound: &Rc<RefCell<Bound>>,
) {
    let Some(props) = global.props else { return };
    match global.type_ {
        ObjectType::Node => {
            let node = node_from_props(global.id, props);
            {
                let mut s = state.borrow_mut();
                s.graph.nodes.insert(global.id, node);
                s.touch();
            }
            // Global props omit link-group/target.object/custom keys: bind for full info.
            let Ok(proxy) = registry.bind::<pw::node::Node, _>(global) else {
                log::warn!("cannot bind node {}", global.id);
                return;
            };
            let id = global.id;
            let st = state.clone();
            let listener = proxy
                .add_listener_local()
                .info(move |info: &NodeInfoRef| {
                    // Node info is incremental: only PROPS events carry a property dict, the
                    // others (state/params changes) report an empty one. Never overwrite the
                    // node from those.
                    if !info.change_mask().contains(NodeChangeMask::PROPS) {
                        return;
                    }
                    let Some(props) = info.props() else { return };
                    let mut s = st.borrow_mut();
                    if let Some(node) = s.graph.nodes.get_mut(&id) {
                        *node = node_from_props(id, props);
                        s.touch();
                    }
                })
                .register();
            bound.borrow_mut().nodes.insert(id, (proxy, listener));
        }
        ObjectType::Port => {
            let num = |k: &str| props.get(k).and_then(|v| v.parse::<u32>().ok());
            let (Some(node_id), Some(dir)) = (num("node.id"), props.get("port.direction")) else {
                return;
            };
            let port = Port {
                id: global.id,
                node_id,
                direction: if dir == "in" { Direction::In } else { Direction::Out },
                channel: props.get("audio.channel").map(str::to_owned),
                index: num("port.id").unwrap_or(u32::MAX),
                monitor: props.get("port.monitor").is_some_and(parse_bool),
            };
            let mut s = state.borrow_mut();
            s.graph.ports.insert(global.id, port);
            s.touch();
        }
        ObjectType::Link => {
            let num = |k: &str| props.get(k).and_then(|v| v.parse::<u32>().ok());
            let (Some(output_node), Some(output_port), Some(input_node), Some(input_port)) = (
                num("link.output.node"),
                num("link.output.port"),
                num("link.input.node"),
                num("link.input.port"),
            ) else {
                return;
            };
            let link = Link {
                id: global.id,
                output_node,
                output_port,
                input_node,
                input_port,
            };
            let mut s = state.borrow_mut();
            s.graph.links.insert(global.id, link);
            s.touch();
        }
        ObjectType::Metadata => {
            let kind = match props.get("metadata.name") {
                Some("default") => MetaKind::Default,
                Some("filters") => MetaKind::Filters,
                _ => return,
            };
            let Ok(meta) = registry.bind::<Metadata, _>(global) else {
                log::warn!("cannot bind metadata {}", global.id);
                return;
            };
            let st = state.clone();
            let listener = meta
                .add_listener_local()
                .property(move |subject, key, _type, value| {
                    on_metadata(&mut st.borrow_mut(), kind, subject, key, value);
                    0
                })
                .register();
            bound.borrow_mut().metadata.insert(global.id, (meta, listener));
        }
        _ => {}
    }
}

#[derive(Clone, Copy)]
enum MetaKind {
    Default,
    Filters,
}

fn on_metadata(s: &mut WatchState, kind: MetaKind, subject: u32, key: Option<&str>, value: Option<&str>) {
    match kind {
        MetaKind::Default => {
            if subject != 0 {
                return;
            }
            match key {
                Some(KEY_DEFAULT_SOURCE) => s.graph.default_source = value.and_then(parse_default_name),
                None => s.graph.default_source = None,
                Some(_) => return,
            }
        }
        MetaKind::Filters => match (key, value) {
            (Some(KEY_SMART_DISABLED), Some(v)) => {
                s.graph.filters_disabled.insert(subject, v.to_owned());
            }
            (Some(KEY_SMART_DISABLED), None) | (None, _) => {
                s.graph.filters_disabled.remove(&subject);
            }
            _ => return,
        },
    }
    s.touch();
}

/// Builds a node from a property dict (global props on discovery, full info props later).
fn node_from_props(id: u32, props: &DictRef) -> Node {
    let get = |k: &str| props.get(k).map(str::to_owned);
    Node {
        id,
        name: get("node.name"),
        description: get("node.description"),
        nick: get("node.nick"),
        media_class: get("media.class"),
        object_serial: get("object.serial"),
        link_group: get("node.link-group"),
        target_object: get("target.object"),
        mic_filter: props.get("steamos.mic_filter").is_some_and(parse_bool),
        smart_disabled_prop: props.get(KEY_SMART_DISABLED).map(parse_bool),
    }
}
