//! `--probe` (audio part): one-shot snapshot of the default source and its mute state.

use std::cell::Cell;
use std::fmt;
use std::rc::Rc;
use std::time::Duration;

use pipewire as pw;

use crate::capture::is_connection_lost;
use crate::graph::Graph;
use crate::mute::{wpctl_muted, DEFAULT_SOURCE_TARGET};
use crate::pwgraph::Watch;

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct ProbeReport {
    /// `Err` if the PipeWire graph could not be read.
    pub pipewire: Result<(), String>,
    /// `node.name` from `default.audio.source`.
    pub default_source: Option<String>,
    pub default_source_description: Option<String>,
    pub default_muted: Result<bool, String>,
}

/// Collects the report. Takes up to a few hundred milliseconds; never modifies the graph.
pub fn probe() -> ProbeReport {
    let graph = snapshot();
    let default_muted = wpctl_muted(DEFAULT_SOURCE_TARGET);
    let mut report = ProbeReport {
        pipewire: graph.as_ref().map(|_| ()).map_err(Clone::clone),
        default_source: None,
        default_source_description: None,
        default_muted,
    };
    let Ok(graph) = graph else { return report };
    report.default_source_description = graph
        .default_source
        .as_deref()
        .and_then(|n| graph.node_by_name(n))
        .map(|n| n.label());
    report.default_source = graph.default_source;
    report
}

/// Connects, waits for two core round trips (registry enumerated, metadata reported), returns
/// the graph and disconnects.
fn snapshot() -> Result<Graph, String> {
    pw::init();
    let err = |e: pw::Error| format!("PipeWire: {e}");
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(err)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(err)?;
    let core = context.connect_rc(None).map_err(err)?;
    let registry = core.get_registry_rc().map_err(err)?;
    let watch = Watch::new(&registry);
    let rounds = Rc::new(Cell::new(0u8));
    let pending = Rc::new(Cell::new(core.sync(0).map_err(err)?.seq()));
    let failure: Rc<Cell<Option<i32>>> = Rc::new(Cell::new(None));
    let _listener = core
        .add_listener_local()
        .done({
            let (rounds, pending, core, mainloop) = (rounds.clone(), pending.clone(), core.downgrade(), mainloop.downgrade());
            move |id, seq| {
                if id != pw::core::PW_ID_CORE || seq.seq() != pending.get() {
                    return;
                }
                rounds.set(rounds.get() + 1);
                let next = (rounds.get() < 2)
                    .then(|| core.upgrade())
                    .flatten()
                    .and_then(|c| c.sync(0).ok());
                match next {
                    Some(s) => pending.set(s.seq()),
                    None => {
                        if let Some(l) = mainloop.upgrade() {
                            l.quit();
                        }
                    }
                }
            }
        })
        .error({
            let (failure, mainloop) = (failure.clone(), mainloop.downgrade());
            move |id, _, res, message| {
                if is_connection_lost(id, res) {
                    log::warn!("PipeWire error: {message}");
                    failure.set(Some(res));
                    if let Some(l) = mainloop.upgrade() {
                        l.quit();
                    }
                }
            }
        })
        .register();
    let timer = mainloop.loop_().add_timer({
        let mainloop = mainloop.downgrade();
        move |_| {
            if let Some(l) = mainloop.upgrade() {
                l.quit();
            }
        }
    });
    let _ = timer.update_timer(Some(PROBE_TIMEOUT), None);
    mainloop.run();
    if let Some(res) = failure.get() {
        return Err(format!("PipeWire connection error ({res})"));
    }
    if rounds.get() < 2 {
        return Err("PipeWire did not answer in time".into());
    }
    let graph = watch.state.borrow().graph.clone();
    Ok(graph)
}

impl fmt::Display for ProbeReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.pipewire {
            Ok(()) => writeln!(f, "PipeWire: connected")?,
            Err(e) => writeln!(f, "PipeWire: unavailable ({e})")?,
        }
        match (&self.default_source, &self.default_source_description) {
            (Some(name), Some(desc)) => writeln!(f, "Default source: {name} ({desc})")?,
            (Some(name), None) => writeln!(f, "Default source: {name} (node not found)")?,
            (None, _) => writeln!(f, "Default source: none")?,
        }
        match &self.default_muted {
            Ok(true) => write!(f, "Default source muted: yes"),
            Ok(false) => write!(f, "Default source muted: no"),
            Err(e) => write!(f, "Default source muted: unknown ({e})"),
        }
    }
}
