//! Reading a JSON object out of whatever a model actually wrote.
//!
//! Shape-agnostic on purpose: this module knows nothing about intents, steps or
//! look-ups, only about the three ways content that is *meant* to be a JSON
//! object arrives in practice. That is what lets both llm phases share it, and
//! what keeps the phase modules readable - the tolerance is ~380 lines and the
//! decision each phase makes with it is about twenty.
//!
//! Three tiers, in order:
//!
//! 1. **Strict JSON.** What a model honoring the `format` schema produces.
//! 2. **A fenced code block.** A model that wrapped its JSON in markdown,
//!    which [`strip_code_fence`] unwraps.
//! 3. **XML.** Some models - gemma and kimi variants especially - answer a
//!    schema-constrained request with `<root><field>value</field></root>`
//!    instead. [`parse_xml`] is a small hand-rolled parser rather than a
//!    dependency because the input is not general XML: no namespaces, no
//!    entities beyond the five predefined ones, and no attributes worth reading.
//!
//! Being strict instead would turn a chatty model into a broken pipeline, which
//! is the whole argument for this file existing. The cost is bounded: every tier
//! either produces an object or declines, and a caller handed `None` reads the
//! content as prose.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// Read a JSON object out of whatever the model wrote: strict JSON, then a
/// fenced code block, then the XML tier — three tiers, in that order, because
/// not every model honors `format`, and not every model that ignores `format`
/// produces JSON rather than XML.
///
/// `None` only when no tier found an object. A caller reads that as "the model
/// answered in prose", which is a usable answer rather than an error.
pub fn object(text: &str, xml_roots: &[&str]) -> Option<Value> {
    if let Ok(v @ Value::Object(_)) = serde_json::from_str(text) {
        return Some(v);
    }
    if let Some(fenced) = strip_code_fence(text) {
        if let Ok(v @ Value::Object(_)) = serde_json::from_str(fenced) {
            return Some(v);
        }
    }
    parse_xml(text, xml_roots)
}

pub fn strip_code_fence(text: &str) -> Option<&str> {
    let text = text.strip_prefix("```")?;
    let text = text.strip_prefix("json").unwrap_or(text);
    let (body, _) = text.split_once("```")?;
    Some(body.trim())
}

/// Try to parse `<intent>…</intent>`-shaped XML into a `Value` matching the
/// JSON schema the model is normally asked to produce.
///
/// The shapes accepted are deliberately narrow: only element names that
/// match the schema field names are recognised (case-insensitive, since
/// `<Mode>` and `<MODE>` show up in the wild), attributes are ignored
/// (the JSON path uses string values, so XML must too), and text content is
/// treated as a string. Anything that does not look like the schema is
/// rejected — a freeform essay with an `<intent>` substring somewhere
/// should not be silently misread as an intent structure.
///
/// Recognised shape:
///
/// ```xml
/// <intent>
///   <mode>direct|inquire</mode>
///   <response>text</response>
///   <memory>true|false</memory>
///   <next_prompt>text</next_prompt>
///   <inquiries>
///     <inquiry>
///       <kind>documents|actions</kind>
///       <question>text</question>
///       <terms><term>word</term><term>word</term></terms>
///       <prompt>text</prompt>
///     </inquiry>
///   </inquiries>
/// </intent>
/// ```
///
/// The parser is hand-rolled to keep the wasm artifact flat: a small
/// recursive-descent walker over a slice of bytes, no allocation until a
/// `Value` is built.
/// The XML tier: one root element named in `roots`, as an object of its
/// children.
///
/// `roots` is the caller's, not this module's, and matching on it is what keeps
/// prose out. A model answering in prose that happens to contain an angle
/// bracket must not be read as a document, and the only reliable signal that a
/// model *meant* to answer in XML is that it used the element name the prompt
/// asked it for.
fn parse_xml(text: &str, roots: &[&str]) -> Option<Value> {
    let bytes = text.as_bytes();
    let mut p = Parser::new(bytes);
    p.skip_ws();
    let elem = p.parse_element()?;
    if !roots.iter().any(|r| elem.name.eq_ignore_ascii_case(r)) {
        return None;
    }
    match element_value(elem) {
        v @ Value::Object(_) => Some(v),
        // A root carrying only text has no fields for a caller to read, so
        // declining sends the content to be read as prose instead - which is
        // what it is.
        _ => None,
    }
}

struct Element<'a> {
    name: &'a str,
    children: Vec<Node<'a>>,
}

enum Node<'a> {
    Element(Element<'a>),
    Text(&'a str),
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    /// Parse a single `<name …>children</name>` or `<name …/>` element.
    /// Returns `None` if the input does not start with `<`.
    fn parse_element(&mut self) -> Option<Element<'a>> {
        self.skip_ws();
        if self.peek()? != b'<' {
            return None;
        }
        self.pos += 1; // consume '<'
        let name_start = self.pos;
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if c == b'>' || c == b'/' || c.is_ascii_whitespace() {
                break;
            }
            self.pos += 1;
        }
        let name = std::str::from_utf8(&self.bytes[name_start..self.pos]).ok()?;
        // Skip attributes (we ignore them all).
        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'>' {
            self.pos += 1;
        }
        if self.pos >= self.bytes.len() {
            return None;
        }
        if self.bytes[self.pos] == b'/' {
            // Self-closing tag <name/>.
            self.pos += 1;
            if self.bytes.get(self.pos) != Some(&b'>') {
                return None;
            }
            self.pos += 1;
            return Some(Element { name, children: Vec::new() });
        }
        self.pos += 1; // consume '>'

        // Children until matching close tag.
        let mut children: Vec<Node<'a>> = Vec::new();
        loop {
            // Look for end of element.
            if self.bytes.get(self.pos) == Some(&b'<')
                && self.bytes.get(self.pos + 1) == Some(&b'/')
            {
                self.pos += 2; // consume "</"
                let close_start = self.pos;
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'>' {
                    self.pos += 1;
                }
                let close = std::str::from_utf8(&self.bytes[close_start..self.pos]).ok()?;
                if !close.eq_ignore_ascii_case(name) {
                    return None; // mismatched close tag: bail
                }
                if self.bytes.get(self.pos) != Some(&b'>') {
                    return None;
                }
                self.pos += 1;
                return Some(Element { name, children });
            }
            if self.peek()? != b'<' {
                // Text node up to the next `<`.
                let text_start = self.pos;
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'<' {
                    self.pos += 1;
                }
                let text = std::str::from_utf8(&self.bytes[text_start..self.pos])
                    .ok()?
                    .trim();
                if !text.is_empty() {
                    children.push(Node::Text(text));
                }
            } else {
                if let Some(child) = self.parse_element() {
                    children.push(Node::Element(child));
                } else {
                    return None;
                }
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
}

/// Convert an XML element to a `serde_json::Value`. Sub-elements recurse
/// into `element_to_value`; `<terms>` flattens to an array of strings;
/// everything else with no sub-elements is its text content as a string.
/// One element as a JSON value, knowing nothing about any schema.
///
/// Three rules, and nothing else:
///
/// * An element with no element children is its **trimmed text**, or `Null`
///   when that text is empty.
/// * An element with element children is an **object**, keyed by child tag name
///   lowercased.
/// * A tag appearing more than once under one parent is an **array**.
///   Repetition is the only way an XML document can say "list", so it has to
///   collapse into one rather than let the last occurrence overwrite the first.
///
/// Text mixed in alongside element children is dropped: every shape that arises
/// in practice puts a value in one or the other, and prose sitting between
/// fields is the model narrating, not data.
///
/// Note what this deliberately does **not** do. It does not know that a
/// repeated `<step>` means `steps`, or that `<terms>` wraps `<term>`. That is
/// knowledge about a schema, it differs between the two phases, and it belongs
/// with the phase - see [`items`], which reads any of those shapes given the
/// caller's own field names. solx-inquiry's version of this function encoded
/// its own schema here instead, as a match over `mode`/`inquiries`/`terms` that
/// dropped every unrecognised element; carried over unchanged, it would have
/// silently discarded every field this package uses.
fn element_value(elem: Element<'_>) -> Value {
    let mut fields: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut text = String::new();

    for child in elem.children {
        match child {
            Node::Text(t) => text.push_str(&decode_entities(t)),
            Node::Element(e) => {
                let key = e.name.to_ascii_lowercase();
                fields.entry(key).or_default().push(element_value(e));
            }
        }
    }

    if fields.is_empty() {
        let t = text.trim();
        return if t.is_empty() { Value::Null } else { Value::String(t.to_string()) };
    }

    let mut out = Map::new();
    for (key, mut values) in fields {
        let value = if values.len() == 1 { values.remove(0) } else { Value::Array(values) };
        out.insert(key, value);
    }
    Value::Object(out)
}

/// Read a list out of `value[key]`, whatever shape it arrived in.
///
/// One `format` schema asks for `{"steps": [...]}` and five different things
/// come back from real models. All five mean the same thing, so all five have
/// to read the same way:
///
/// | what the model produced | shape |
/// |---|---|
/// | the schema, honored | `{"steps": [a, b]}` |
/// | XML, repeated child | `{"steps": {"step": [a, b]}}` |
/// | XML, a single child | `{"steps": {"step": a}}` |
/// | XML, repeated with no wrapper | `{"step": [a, b]}` |
/// | one item, never wrapped | `{"steps": a}` |
///
/// `singular` is the child tag to look for - the caller's word, because only
/// the caller knows what one of its items is called.
///
/// Returns borrowed values so a caller can pick fields off each without
/// cloning the list first; an absent or null key is an empty list, never an
/// error, because "the model proposed nothing" is a normal answer.
pub fn items<'a>(value: &'a Value, key: &str, singular: &str) -> Vec<&'a Value> {
    fn unwrap<'a>(v: &'a Value, singular: &str) -> Vec<&'a Value> {
        match v {
            Value::Array(a) => a.iter().collect(),
            Value::Object(o) => match o.get(singular) {
                Some(Value::Array(a)) => a.iter().collect(),
                Some(one) => vec![one],
                // An object with no `singular` key is itself the one item.
                None => vec![v],
            },
            Value::Null => Vec::new(),
            one => vec![one],
        }
    }

    match value.get(key) {
        Some(v) => unwrap(v, singular),
        // A repeated singular tag directly under the root, with no wrapper.
        None => value.get(singular).map(|v| unwrap(v, singular)).unwrap_or_default(),
    }
}

/// Decode the five predefined XML entities, and nothing else.
///
/// A model hand-writing XML escapes `&` and `<` more often than not, and an
/// undecoded `&lt;` travels all the way into an action parameter as those four
/// characters — the steps phase puts these strings straight into a `params`
/// object, so this is a correctness problem rather than a display one.
///
/// Numeric character references (`&#60;`) are deliberately left alone. They are
/// vanishingly rare here, and a half-implemented decoder that mangles one is
/// worse than one that passes it through untouched.
///
/// Single pass, so nothing is decoded twice: `&amp;lt;` becomes the four
/// characters `&lt;`, which is what it means, not `<`.
fn decode_entities(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    const ENTITIES: [(&str, char); 5] = [
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&amp;", '&'),
        ("&quot;", '"'),
        ("&apos;", '\''),
    ];
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        match ENTITIES.iter().find(|(pat, _)| tail.starts_with(pat)) {
            Some((pat, ch)) => {
                out.push(*ch);
                rest = &tail[pat.len()..];
            }
            // Not an entity we know: a bare `&` is legal enough in practice and
            // dropping it would corrupt the text worse than keeping it.
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}
