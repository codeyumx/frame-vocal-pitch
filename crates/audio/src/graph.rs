//! Pure (PipeWire-free) model of the parts of the PipeWire graph this crate cares about: node
//! labels and the `default.audio.source` metadata entry.

use std::collections::BTreeMap;

/// A PipeWire node as seen in the registry (only the properties we use).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Node {
    pub id: u32,
    pub name: Option<String>,
    pub description: Option<String>,
    pub nick: Option<String>,
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
}

/// Snapshot of the registry nodes plus the default source.
#[derive(Clone, Debug, Default)]
pub(crate) struct Graph {
    pub nodes: BTreeMap<u32, Node>,
    /// `node.name` from `default.audio.source` in the `default` metadata.
    pub default_source: Option<String>,
}

impl Graph {
    pub fn node_by_name(&self, name: &str) -> Option<&Node> {
        self.nodes.values().find(|n| n.name.as_deref() == Some(name))
    }
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

/// `SPA_AUDIO_CHANNEL_MONO` (spa/param/audio/raw.h); mono streams use this position.
pub(crate) const CHANNEL_MONO: u32 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_prefer_description_then_nick_then_name() {
        let mut n = Node {
            id: 7,
            name: Some("alsa_input.x".into()),
            nick: Some("Nick".into()),
            description: Some("Built-in Audio Capture".into()),
        };
        assert_eq!(n.label(), "Built-in Audio Capture");
        n.description = None;
        assert_eq!(n.label(), "Nick");
        n.nick = None;
        assert_eq!(n.label(), "alsa_input.x");
        n.name = None;
        assert_eq!(n.label(), "node 7");
    }

    #[test]
    fn default_metadata_value_parsing() {
        assert_eq!(parse_default_name(r#"{"name":"alsa_input.x"}"#).as_deref(), Some("alsa_input.x"));
        assert_eq!(parse_default_name(r#"{ "name": "a\"b" }"#).as_deref(), Some("a\"b"));
        assert_eq!(parse_default_name("{ name = alsa_input.y }").as_deref(), Some("alsa_input.y"));
        assert_eq!(parse_default_name("{}"), None);
        assert_eq!(parse_default_name(r#"{"name":""}"#), None);
    }
}
