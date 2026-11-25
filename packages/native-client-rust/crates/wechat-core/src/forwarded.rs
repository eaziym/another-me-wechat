//! Display-only rendering of appmsg subtype 19 (Combine and Forward).
//! Parse each XML layer once; message text is never searched for metadata.
use roxmltree::{Document, Node, ParsingOptions};

const MAX_XML_BYTES: usize = 1024 * 1024;
const MAX_PARSED_BYTES: usize = 2 * MAX_XML_BYTES;
const MAX_XML_NODES: u32 = 20_000;
const MAX_ITEMS: usize = 500;
const MAX_DEPTH: usize = 4;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_MEDIA: usize = 200;
const TRUNCATED: &str = "\n[Chat History truncated]";

fn parse(xml: &str) -> Option<Document<'_>> {
    if xml.len() > MAX_XML_BYTES {
        return None;
    }
    Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            ..Default::default()
        },
    )
    .ok()
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|node| node.has_tag_name(name) && node.tag_name().namespace().is_none())
}

/// Only direct text, including CDATA. XML entities have already been decoded
/// by roxmltree at this layer; do not unescape this string a second time.
fn text(node: Node<'_, '_>) -> Option<String> {
    let value: String = node
        .children()
        .filter(|node| node.is_text())
        .filter_map(|node| node.text())
        .collect();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn field(node: Node<'_, '_>, name: &str) -> Option<String> {
    text(child(node, name)?)
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Keep the legacy title-only fallback, but never return unbounded XML.
pub(super) fn fallback_title(title: &str) -> String {
    if title.is_empty() {
        return "[Chat History unavailable]".into();
    }
    if title.len() <= MAX_OUTPUT_BYTES {
        return title.to_owned();
    }
    format!(
        "{}{TRUNCATED}",
        utf8_prefix(title, MAX_OUTPUT_BYTES - TRUNCATED.len())
    )
}

/// What a fetchable record item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordMediaKind {
    Image,
    Video,
    File,
}

/// One picture, video or file inside a forwarded chat record (or a record
/// nested in it), as the record's dataitem describes its full copy. `number`
/// is the item's 1-based position among the record's media in the rendered
/// text; the stored XML never changes, so the number is stable. Dataitems also
/// name a thumbnail (`cdnthumburl`), but the CDN refused it with the photo
/// thumbnail transfer type, so it is not listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordMedia {
    pub number: u32,
    pub kind: RecordMediaKind,
    pub title: Option<String>,
    pub data_url: String,
    pub data_key: String,
    pub data_size: Option<String>,
    pub data_md5: Option<String>,
}

fn aes_key(value: Option<String>) -> Option<String> {
    value.filter(|key| key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[derive(Default)]
struct Renderer {
    output: String,
    items: usize,
    parsed_bytes: usize,
    truncated: bool,
    media: Vec<RecordMedia>,
}

impl Renderer {
    fn line(&mut self, depth: usize, value: &str) {
        if self.truncated {
            return;
        }
        let line = format!(
            "{}{}{value}",
            if self.output.is_empty() { "" } else { "\n" },
            "  ".repeat(depth)
        );
        let available = (MAX_OUTPUT_BYTES - TRUNCATED.len()).saturating_sub(self.output.len());
        if line.len() > available {
            self.output.push_str(utf8_prefix(&line, available));
            self.truncate();
        } else {
            self.output.push_str(&line);
        }
    }

    fn truncate(&mut self) {
        if !self.truncated {
            self.output.push_str(TRUNCATED);
            self.truncated = true;
        }
    }

    /// recorditem and recordxml can contain escaped/CDATA XML or an actual
    /// recordinfo element. Both forms occur without changing message semantics.
    fn embedded_record(&mut self, container: Node<'_, '_>, depth: usize) -> bool {
        if let Some(record) = child(container, "recordinfo") {
            return self.record(record, depth);
        }
        let Some(xml) = text(container) else {
            return false;
        };
        if xml.len() > MAX_XML_BYTES || self.parsed_bytes + xml.len() > MAX_PARSED_BYTES {
            self.truncate();
            return true;
        }
        self.parsed_bytes += xml.len();
        let Some(document) = parse(&xml) else {
            return false;
        };
        self.record(document.root_element(), depth)
    }

    /// Number a picture (datatype 2), video (4) or file (8) that carries a
    /// CDN copy, and return its label; None leaves the item's old label.
    fn media_item(&mut self, item: Node<'_, '_>) -> Option<String> {
        let (kind, name) = match item.attribute("datatype")? {
            "2" => (RecordMediaKind::Image, "image"),
            "4" => (RecordMediaKind::Video, "video"),
            "8" => (RecordMediaKind::File, "file"),
            _ => return None,
        };
        if self.media.len() == MAX_MEDIA {
            return None;
        }
        let data_url = field(item, "cdndataurl")?;
        let data_key = aes_key(field(item, "cdndatakey"))?;
        let data_size = field(item, "datasize");
        if data_size.as_deref().and_then(|v| v.parse::<u64>().ok()) == Some(0) {
            return None;
        }
        let number = u32::try_from(self.media.len() + 1).ok()?;
        let title = field(item, "datatitle");
        let label = match &title {
            Some(title) if kind == RecordMediaKind::File => {
                format!("[{name} item={number}] {title}")
            }
            _ => format!("[{name} item={number}]"),
        };
        self.media.push(RecordMedia {
            number,
            kind,
            title,
            data_url,
            data_key,
            data_size,
            data_md5: field(item, "fullmd5"),
        });
        Some(label)
    }

    fn record(&mut self, record: Node<'_, '_>, depth: usize) -> bool {
        if !record.has_tag_name("recordinfo") || record.tag_name().namespace().is_some() {
            return false;
        }
        let Some(list) = child(record, "datalist") else {
            return false;
        };
        for item in list
            .children()
            .filter(|node| node.has_tag_name("dataitem") && node.tag_name().namespace().is_none())
        {
            if self.truncated {
                break;
            }
            if self.items == MAX_ITEMS {
                self.truncate();
                break;
            }
            self.items += 1;
            let sender = field(item, "sourcename")
                .or_else(|| field(item, "displayname"))
                .or_else(|| {
                    child(item, "dataitemsource").and_then(|source| {
                        field(source, "sourcename").or_else(|| field(source, "displayname"))
                    })
                })
                .unwrap_or_default();
            let listed = self.media.len();
            let body = match self.media_item(item) {
                Some(label) => label,
                None => field(item, "datatitle")
                    .or_else(|| field(item, "datadesc"))
                    .unwrap_or_else(|| "[media]".into()),
            };
            let nested = child(item, "recordxml");
            let body = if nested.is_some() || item.attribute("datatype") == Some("17") {
                format!("[Chat History] {body}")
            } else {
                body
            };
            self.line(
                depth,
                &if sender.is_empty() {
                    body
                } else {
                    format!("{sender}: {body}")
                },
            );
            if self.truncated {
                // An item whose marker was cut off is not offered.
                if let Some(item) = self.media.get(listed) {
                    let marker = format!(" item={}]", item.number);
                    if !self.output.contains(&marker) {
                        self.media.truncate(listed);
                    }
                }
                break;
            }
            if let Some(nested) = nested {
                if depth == MAX_DEPTH {
                    self.line(depth + 1, "[Nested chat history omitted: depth limit]");
                } else if !self.embedded_record(nested, depth + 1) {
                    self.line(depth + 1, "[Nested chat history unavailable]");
                }
            } else if item.attribute("datatype") == Some("17") {
                self.line(depth + 1, "[Nested chat history unavailable]");
            }
        }
        true
    }
}

/// None means this is not a well-formed outer subtype-19 message. Other appmsg
/// types retain their existing formatting. Missing/invalid records use a title
/// fallback, not raw XML; missing nested records are labeled explicitly.
pub(super) fn render(content: &str) -> Option<String> {
    render_with_media(content).map(|(text, _)| text)
}

/// The pictures, videos and files of a subtype-19 record, numbered as in
/// [`render`]'s text. Only items that text shows are listed.
pub(crate) fn media(content: &str) -> Option<Vec<RecordMedia>> {
    render_with_media(content).map(|(_, media)| media)
}

fn render_with_media(content: &str) -> Option<(String, Vec<RecordMedia>)> {
    let document = parse(content)?;
    let root = document.root_element();
    if !root.has_tag_name("msg") || root.tag_name().namespace().is_some() {
        return None;
    }
    let app = child(root, "appmsg")?;
    if field(app, "type")?.parse::<i32>().ok()? != 19 {
        return None;
    }
    let title = field(app, "title").unwrap_or_default();
    let Some(record) = child(app, "recorditem") else {
        return Some((fallback_title(&title), vec![]));
    };
    let mut renderer = Renderer {
        parsed_bytes: content.len(),
        ..Default::default()
    };
    renderer.line(
        0,
        &if title.is_empty() {
            "[Chat History]".into()
        } else {
            format!("[Chat History] {title}")
        },
    );
    let valid = renderer.embedded_record(record, 0);
    if !renderer.truncated && (!valid || renderer.items == 0) {
        return Some((fallback_title(&title), vec![]));
    }
    Some((renderer.output, renderer.media))
}

#[cfg(test)]
#[path = "forwarded_tests.rs"]
mod tests;
