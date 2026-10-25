//! `--probe` (audio part): one-shot snapshot of the PipeWire graph, the resolved hardware
//! source, mute state and Valve's mic filter nodes with their `filters` metadata state.

use std::cell::Cell;
use std::fmt;
use std::rc::Rc;
use std::time::Duration;

use pipewire as pw;

use crate::graph::{filters_active, mic_filters, resolve_hardware_source, Graph, MicFilter};
use crate::mute::{wpctl_muted, DEFAULT_SOURCE_TARGET};
use crate::pwgraph::Watch;

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The device node bypass mode would record from.
#[derive(Clone, Debug, PartialEq)]
pub struct HardwareSource {
    pub node_id: u32,
    pub name: String,
    pub description: String,
    pub channels: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ProbeReport {
    /// `Err` if the PipeWire graph could not be read.
    pub pipewire: Result<(), String>,
    /// `node.name` from `default.audio.source`.
    pub default_source: Option<String>,
    pub default_source_description: Option<String>,
    pub hardware_source: Result<HardwareSource, String>,
    pub default_muted: Result<bool, String>,
    /// Mute of the hardware node itself (only when it was resolved).
    pub hardware_muted: Option<Result<bool, String>>,
    /// Nodes with `steamos.mic_filter = true`.
    pub mic_filters: Vec<MicFilter>,
    /// Whether Valve's filters are enabled for other apps; `None` without mic filter nodes.
    pub filters_active_for_others: Option<bool>,
}

/// Collects the report. Takes up to a few hundred milliseconds; never modifies the graph.
pub fn probe() -> ProbeReport {
    let graph = snapshot();
    let default_muted = wpctl_muted(DEFAULT_SOURCE_TARGET);
    let mut report = ProbeReport {
        pipewire: graph.as_ref().map(|_| ()).map_err(Clone::clone),
        default_source: None,
        default_source_description: None,
        hardware_source: Err("PipeWire unavailable".into()),
        default_muted,
        hardware_muted: None,
        mic_filters: Vec::new(),
        filters_active_for_others: None,
    };
    let Ok(graph) = graph else { return report };
    report.default_source = graph.default_source.clone();
    report.default_source_description = graph
        .default_source
        .as_deref()
        .and_then(|n| graph.node_by_name(n))
        .map(|n| n.label());
    report.hardware_source = resolve_hardware_source(&graph)
        .map(|hw| HardwareSource {
            node_id: hw.node_id,
            channels: hw.channels(),
            name: hw.name,
            description: hw.description,
        })
        .map_err(|e| e.to_string());
    if let Ok(hw) = &report.hardware_source {
        report.hardware_muted = Some(wpctl_muted(&hw.node_id.to_string()));
    }
    report.mic_filters = mic_filters(&graph);
    report.filters_active_for_others = filters_active(&graph);
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
                if id == pw::core::PW_ID_CORE {
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

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn mute_text(m: &Result<bool, String>) -> String {
    match m {
        Ok(b) => yes_no(*b).to_owned(),
        Err(e) => format!("unknown ({e})"),
    }
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
        writeln!(f, "Default source muted: {}", mute_text(&self.default_muted))?;
        match &self.hardware_source {
            Ok(hw) => {
                writeln!(
                    f,
                    "Resolved hardware source: {} ({}), node {}, channels {}",
                    hw.name,
                    hw.description,
                    hw.node_id,
                    hw.channels.join(",")
                )?;
                if let Some(m) = &self.hardware_muted {
                    writeln!(f, "Hardware source muted: {}", mute_text(m))?;
                }
            }
            Err(e) => writeln!(f, "Resolved hardware source: unavailable ({e})")?,
        }
        if self.mic_filters.is_empty() {
            writeln!(f, "Mic filter nodes: none (no steamos.mic_filter nodes)")?;
        } else {
            writeln!(f, "Mic filter nodes:")?;
            for m in &self.mic_filters {
                let origin = match &m.metadata_value {
                    Some(v) => format!("filters metadata = {v}"),
                    None => "not set in filters metadata".to_owned(),
                };
                writeln!(
                    f,
                    "  {} {}: filter.smart.disabled {} ({origin})",
                    m.node_id,
                    m.name,
                    m.disabled
                )?;
            }
        }
        let active = match self.filters_active_for_others {
            Some(b) => yes_no(b),
            None => "n/a",
        };
        write!(f, "Valve mic filters active for other apps: {active}")
    }
}
