//! Minimal XML tree on top of quick-xml (FCPXML documents are small).

use super::InterchangeError;
use quick_xml::events::Event;

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Depth-first search for the first element named `name`.
    pub fn find(&self, name: &str) -> Option<&Node> {
        if self.name == name {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(name))
    }
}

pub fn parse(text: &str) -> Result<Node, InterchangeError> {
    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<Node> = vec![Node { name: "#root".into(), ..Default::default() }];
    loop {
        let ev = reader.read_event().map_err(|e| InterchangeError::Xml(e.to_string()))?;
        match ev {
            Event::Start(e) => stack.push(element(&e)?),
            Event::Empty(e) => {
                let n = element(&e)?;
                stack.last_mut().unwrap().children.push(n);
            }
            Event::End(_) => {
                let n = stack.pop().ok_or_else(|| InterchangeError::Xml("unbalanced".into()))?;
                stack.last_mut().ok_or_else(|| InterchangeError::Xml("unbalanced".into()))?.children.push(n);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let root = stack.pop().ok_or_else(|| InterchangeError::Xml("empty".into()))?;
    Ok(root)
}

fn element(e: &quick_xml::events::BytesStart<'_>) -> Result<Node, InterchangeError> {
    let name = e.name().into_inner().to_string();
    let mut attrs = Vec::new();
    for a in e.attributes() {
        let a = a.map_err(|e| InterchangeError::Xml(e.to_string()))?;
        let k = a.key.into_inner().to_string();
        let v = a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map_err(|e| InterchangeError::Xml(e.to_string()))?.into_owned();
        attrs.push((k, v));
    }
    Ok(Node { name, attrs, children: Vec::new() })
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}
