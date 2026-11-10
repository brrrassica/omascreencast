//! Minimal XML support for the endpoints that speak XML.
//!
//! GameStream is JSON on `/serverinfo` and `/applist`, but the pairing
//! endpoints exchange XML documents whose *element names* are part of the
//! protocol. Rather than hand-rolling string concatenation (which is where
//! escaping bugs live), this module builds and parses a tiny document model on
//! top of `quick-xml`:
//!
//! - parsing is tolerant of whitespace and attribute order,
//! - serialising escapes text and attribute values correctly,
//! - the model has no namespace support, because the protocol uses none.

use quick_xml::Writer;
use quick_xml::events::Event;

use crate::packed::PackedError;

/// A single XML element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    /// Local element name.
    pub name: String,
    /// Text content; empty for elements with children.
    pub text: String,
    /// Attributes in insertion order.
    pub attributes: Vec<(String, String)>,
    /// Child elements.
    pub children: Vec<Element>,
}

impl Element {
    /// Create an empty element with no attributes or children.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            text: String::new(),
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Create an element carrying text content.
    #[must_use]
    pub fn with_text(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::new(name)
        }
    }

    /// Add a child, returning `self` for chaining.
    #[must_use]
    pub fn child(mut self, child: Element) -> Self {
        self.children.push(child);
        self
    }

    /// Add an attribute, returning `self` for chaining.
    #[must_use]
    pub fn attr(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.push((key.into(), value.into()));
        self
    }

    /// Find the first child with the given name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.name == name)
    }

    /// Read the text of the first child with the given name.
    #[must_use]
    pub fn child_text(&self, name: &str) -> Option<&str> {
        self.find(name).map(|child| child.text.as_str())
    }

    /// Serialise the element and its subtree to a string.
    ///
    /// # Errors
    ///
    /// Returns an error only when the writer fails, which for an in-memory
    /// buffer means the document is malformed (for example a name containing
    /// `<`). The caller treats that as a programming error.
    pub fn to_xml(&self) -> Result<String, XmlError> {
        let mut writer = Writer::new(Vec::new());
        write_element(&mut writer, self)?;
        let bytes = writer.into_inner();
        String::from_utf8(bytes).map_err(|_| XmlError::NotUtf8)
    }

    /// Parse an XML document and return its root element.
    ///
    /// # Errors
    ///
    /// Returns [`XmlError`] for malformed input, an empty document, or bytes
    /// that are not UTF-8.
    pub fn parse(document: &str) -> Result<Self, XmlError> {
        let mut reader = quick_xml::Reader::from_str(document);
        reader.config_mut().trim_text(true);

        let mut stack: Vec<Element> = Vec::new();
        loop {
            match reader.read_event().map_err(|_| XmlError::Malformed)? {
                Event::Start(start) => {
                    stack.push(start_to_element(&start)?);
                }
                Event::Empty(empty) => {
                    let element = start_to_element(&empty)?;
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(element),
                        None => return Ok(element),
                    }
                }
                Event::Text(text) => {
                    if let Some(top) = stack.last_mut() {
                        let decoded = text.unescape().map_err(|_| XmlError::NotUtf8)?;
                        top.text.push_str(decoded.as_ref());
                    }
                }
                Event::CData(data) => {
                    if let Some(top) = stack.last_mut() {
                        let decoded = data.decode().map_err(|_| XmlError::NotUtf8)?;
                        top.text.push_str(decoded.as_ref());
                    }
                }
                Event::End(_) => {
                    let finished = stack.pop().ok_or(XmlError::Malformed)?;
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(finished),
                        None => return Ok(finished),
                    }
                }
                Event::Eof => {
                    // A non-empty stack means an element was opened and never
                    // closed, which is malformed rather than empty.
                    if stack.is_empty() {
                        break;
                    }
                    return Err(XmlError::Malformed);
                }
                // Declarations, comments, and processing instructions carry no
                // protocol data.
                _ => {}
            }
        }
        Err(XmlError::Empty)
    }
}

fn start_to_element(event: &quick_xml::events::BytesStart<'_>) -> Result<Element, XmlError> {
    let name = String::from_utf8(event.name().as_ref().to_vec()).map_err(|_| XmlError::NotUtf8)?;
    let mut element = Element::new(name);
    for attribute in event.attributes() {
        let attribute = attribute.map_err(|_| XmlError::Malformed)?;
        let key =
            String::from_utf8(attribute.key.as_ref().to_vec()).map_err(|_| XmlError::NotUtf8)?;
        let value = attribute
            .unescape_value()
            .map_err(|_| XmlError::Malformed)?
            .into_owned();
        element.attributes.push((key, value));
    }
    Ok(element)
}

fn write_element(writer: &mut Writer<Vec<u8>>, element: &Element) -> Result<(), XmlError> {
    let mut start = quick_xml::events::BytesStart::new(element.name.as_str());
    for (key, value) in &element.attributes {
        start.push_attribute((key.as_str(), value.as_str()));
    }
    if element.children.is_empty() {
        writer
            .write_event(Event::Start(start))
            .map_err(|_| XmlError::Write)?;
        if !element.text.is_empty() {
            writer
                .write_event(Event::Text(quick_xml::events::BytesText::new(
                    &element.text,
                )))
                .map_err(|_| XmlError::Write)?;
        }
        writer
            .write_event(Event::End(quick_xml::events::BytesEnd::new(
                element.name.as_str(),
            )))
            .map_err(|_| XmlError::Write)?;
    } else {
        writer
            .write_event(Event::Start(start))
            .map_err(|_| XmlError::Write)?;
        for child in &element.children {
            write_element(writer, child)?;
        }
        writer
            .write_event(Event::End(quick_xml::events::BytesEnd::new(
                element.name.as_str(),
            )))
            .map_err(|_| XmlError::Write)?;
    }
    Ok(())
}

/// Why an XML document could not be read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum XmlError {
    /// The document was syntactically invalid or did not end where expected.
    #[error("malformed XML document")]
    Malformed,
    /// The document contained no element.
    #[error("empty XML document")]
    Empty,
    /// The document contained bytes that are not UTF-8.
    #[error("XML document is not UTF-8")]
    NotUtf8,
    /// The writer could not emit the document.
    #[error("could not write XML document")]
    Write,
}

/// Re-export so callers can construct the shared truncation error without
/// reaching into `packed` for trivia.
pub type XmlResult<T> = Result<T, XmlError>;

/// Convenience: an error type that carries both XML and packed failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProtoError {
    /// A packed struct could not be read or written.
    #[error(transparent)]
    Packed(#[from] PackedError),
    /// An XML document could not be read or written.
    #[error(transparent)]
    Xml(#[from] XmlError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_a_simple_document() {
        let document = Element::new("root").child(Element::with_text("client-cert", "abc"));
        assert_eq!(
            document.to_xml().unwrap(),
            "<root><client-cert>abc</client-cert></root>"
        );
    }

    #[test]
    fn escapes_special_characters() {
        let document = Element::with_text("root", "a < b & c > d");
        let xml = document.to_xml().unwrap();
        assert!(xml.contains("<"));
        assert!(xml.contains("&"));
        let parsed = Element::parse(&xml).unwrap();
        assert_eq!(parsed.text, "a < b & c > d");
    }

    #[test]
    fn round_trips_nested_documents() {
        let document = Element::new("root")
            .attr("version", "1")
            .child(Element::with_text("client-cert", "PEM"))
            .child(Element::with_text("challenge", "0011AABB"));
        let xml = document.to_xml().unwrap();
        let parsed = Element::parse(&xml).unwrap();
        assert_eq!(parsed, document);
        assert_eq!(parsed.attributes[0], ("version".to_owned(), "1".to_owned()));
        assert_eq!(parsed.child_text("challenge"), Some("0011AABB"));
    }

    #[test]
    fn parses_whitespace_prefixed_documents() {
        let xml = "\n<?xml version=\"1.0\"?>\n<root>\n  <a>1</a>\n</root>\n";
        let parsed = Element::parse(xml).unwrap();
        assert_eq!(parsed.name, "root");
        assert_eq!(parsed.child_text("a"), Some("1"));
    }

    #[test]
    fn parses_self_closing_root() {
        let parsed = Element::parse("<root/>").unwrap();
        assert_eq!(parsed, Element::new("root"));
    }

    #[test]
    fn rejects_malformed_and_empty_documents() {
        assert_eq!(Element::parse("<root>"), Err(XmlError::Malformed));
        assert_eq!(Element::parse(""), Err(XmlError::Empty));
        assert_eq!(Element::parse("   "), Err(XmlError::Empty));
    }

    #[test]
    fn missing_child_lookups_return_none() {
        let document = Element::new("root");
        assert!(document.find("nope").is_none());
        assert!(document.child_text("nope").is_none());
    }
}
