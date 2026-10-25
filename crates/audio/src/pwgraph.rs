//! Registry watcher: mirrors node labels and the `default` metadata's `default.audio.source`
//! into a [`Graph`]. Read-only: never writes metadata.
//!
//! Nodes are taken from their global properties (which carry `node.name` and
//! `node.description`); they are not bound, so no proxy can outlive its global.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use pipewire as pw;
use pw::metadata::{Metadata, MetadataListener};
use pw::registry::{GlobalObject, RegistryRc};
use pw::spa::utils::dict::DictRef;
use pw::types::ObjectType;

use crate::graph::{parse_default_name, Graph, Node};

const KEY_DEFAULT_SOURCE: &str = "default.audio.source";

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

type BoundMetadata = HashMap<u32, (Metadata, MetadataListener)>;

/// Keeps the registry listener and the bound `default` metadata alive; drop to stop watching.
pub(crate) struct Watch {
    pub state: Rc<RefCell<WatchState>>,
    _listener: pw::registry::Listener,
    _bound: Rc<RefCell<BoundMetadata>>,
}

impl Watch {
    pub fn new(registry: &RegistryRc) -> Watch {
        let state = Rc::new(RefCell::new(WatchState::default()));
        let bound = Rc::new(RefCell::new(BoundMetadata::new()));
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
                    // Drop the proxy after releasing the borrow: destroying it may emit events.
                    let removed = bound.borrow_mut().remove(&id);
                    drop(removed);
                    let mut s = state.borrow_mut();
                    if s.graph.nodes.remove(&id).is_some() {
                        s.touch();
                    }
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
    bound: &Rc<RefCell<BoundMetadata>>,
) {
    let Some(props) = global.props else { return };
    match global.type_ {
        ObjectType::Node => {
            let get = |k: &str| props.get(k).map(str::to_owned);
            let node = Node {
                id: global.id,
                name: get("node.name"),
                description: get("node.description"),
                nick: get("node.nick"),
            };
            let mut s = state.borrow_mut();
            s.graph.nodes.insert(global.id, node);
            s.touch();
        }
        ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
            let Ok(meta) = registry.bind::<Metadata, _>(global) else {
                log::warn!("cannot bind metadata {}", global.id);
                return;
            };
            let st = state.clone();
            let listener = meta
                .add_listener_local()
                .property(move |subject, key, _type, value| {
                    on_default_metadata(&mut st.borrow_mut(), subject, key, value);
                    0
                })
                .register();
            bound.borrow_mut().insert(global.id, (meta, listener));
        }
        _ => {}
    }
}

fn on_default_metadata(s: &mut WatchState, subject: u32, key: Option<&str>, value: Option<&str>) {
    if subject != 0 {
        return;
    }
    match key {
        Some(KEY_DEFAULT_SOURCE) => s.graph.default_source = value.and_then(parse_default_name),
        None => s.graph.default_source = None,
        Some(_) => return,
    }
    s.touch();
}
