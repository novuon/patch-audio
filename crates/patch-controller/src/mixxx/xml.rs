//! A minimal XML reader, just enough for a Mixxx controller preset.
//!
//! Patch has no XML dependency and does not need one: a Mixxx mapping is a
//! small, regular document — elements, attributes, text, comments, and the
//! five predefined entities. This reader handles exactly that and refuses
//! anything else rather than guessing, so a file it accepts is a file it
//! understood.
//!
//! Deliberately *not* supported, and rejected with a position: processing
//! instructions (`<?xml-stylesheet?>`, which explains why the declaration
//! `<?xml …?>` is skipped as a special case), doctypes, CDATA sections, and
//! namespace prefixes (`<ns:element>`). A mapping that uses them fails with a
//! message naming the byte, not with a silently empty profile.

/// One element.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    /// Text directly inside this element, trimmed.
    pub text: String,
    pub children: Vec<Node>,
}

impl Node {
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The first child with this name.
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|node| node.name == name)
    }

    /// Every child with this name, in document order.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> {
        self.children.iter().filter(move |node| node.name == name)
    }

    /// The first child with this name, creating nothing: `None` when the
    /// element is absent.
    pub fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name).map(|node| node.text.as_str())
    }
}

/// The most internal entity declarations a document may carry, and the most
/// text they may expand to. Entity expansion is the classic way to make a
/// parser allocate without bound, so both are capped and a document that
/// exceeds them is refused rather than truncated.
pub const MAX_ENTITIES: usize = 64;
pub const MAX_ENTITY_TEXT: usize = 8 * 1024;

/// Parses a document and returns its root element.
///
/// The whole document is read at once: a controller preset is small, and a
/// streaming reader would buy nothing while making the error positions harder
/// to report.
pub fn parse(input: &str) -> Result<Node, String> {
    let bytes: Vec<char> = input.chars().collect();
    let mut parser = Parser {
        chars: bytes,
        at: 0,
        entities: Vec::new(),
        expansion: 0,
    };
    parser.skip_misc()?;
    let root = parser.element()?;
    parser.skip_misc()?;
    if parser.at < parser.chars.len() {
        return Err(parser.error("trailing content after the root element"));
    }
    Ok(root)
}

struct Parser {
    chars: Vec<char>,
    at: usize,
    /// Internal general entities declared in the doctype, in declaration order.
    entities: Vec<(String, String)>,
    /// Characters these entities have expanded to so far.
    expansion: usize,
}

impl Parser {
    fn error(&self, what: &str) -> String {
        format!("{what} at character {}", self.at + 1)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn starts_with(&self, text: &str) -> bool {
        let len = text.chars().count();
        if len == 0 {
            return true;
        }
        let Some(rest) = self.chars.get(self.at..self.at.saturating_add(len)) else {
            return false;
        };
        rest.iter()
            .zip(text.chars())
            .all(|(have, want)| *have == want)
    }

    fn eat(&mut self, text: &str) -> bool {
        if self.starts_with(text) {
            self.at += text.chars().count();
            return true;
        }
        false
    }

    /// Whitespace, comments and the XML declaration.
    fn skip_misc(&mut self) -> Result<(), String> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.at += 1;
            }
            if self.starts_with("<?xml") {
                let end = self
                    .find("?>")
                    .ok_or_else(|| self.error("unterminated XML declaration"))?;
                self.at = end + 2;
                continue;
            }
            if self.starts_with("<!--") {
                let end = self
                    .find("-->")
                    .ok_or_else(|| self.error("unterminated comment"))?;
                self.at = end + 3;
                continue;
            }
            if self.starts_with("<!DOCTYPE") {
                self.doctype()?;
                continue;
            }
            return Ok(());
        }
    }

    /// A doctype, with its internal subset of entity declarations.
    ///
    /// Only internal general entities are read: `SYSTEM`/`PUBLIC` external
    /// entities and parameter entities are refused, because following a
    /// network or file reference is not something a mapping file may ask a
    /// settings-adjacent tool to do.
    fn doctype(&mut self) -> Result<(), String> {
        self.at += "<!DOCTYPE".chars().count();
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            match c {
                '[' => {
                    depth += 1;
                    self.at += 1;
                    if depth == 1 {
                        self.internal_subset()?;
                        depth = 0;
                        continue;
                    }
                    return Err(self.error("nested doctype subsets are not supported"));
                }
                '>' => {
                    self.at += 1;
                    return Ok(());
                }
                c if c.is_whitespace() => {
                    self.at += 1;
                }
                _ => {
                    // The external id (SYSTEM "…" / PUBLIC "…" "…") is allowed
                    // and ignored, but a reference to resolve is not.
                    let word = self.name()?;
                    if word.eq_ignore_ascii_case("SYSTEM") || word.eq_ignore_ascii_case("PUBLIC") {
                        return Err(self.error(
                            "external doctype references are not followed; an internal subset \
                             of entity declarations is supported",
                        ));
                    }
                }
            }
        }
        Err(self.error("unterminated doctype"))
    }

    /// `<!ENTITY name "value">` … `]>`
    fn internal_subset(&mut self) -> Result<(), String> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.at += 1;
            }
            if self.eat("]") {
                return Ok(());
            }
            if self.peek().is_none() {
                return Err(self.error("unterminated doctype subset"));
            }
            if self.eat("<!--") {
                let end = self
                    .find("-->")
                    .ok_or_else(|| self.error("unterminated comment"))?;
                self.at = end + 3;
                continue;
            }
            if !self.eat("<!ENTITY") {
                return Err(self.error("only ENTITY declarations are supported in a doctype"));
            }
            while self.peek().is_some_and(char::is_whitespace) {
                self.at += 1;
            }
            if self.peek() == Some('%') {
                return Err(self.error("parameter entities are not supported"));
            }
            let name = self.name()?;
            while self.peek().is_some_and(char::is_whitespace) {
                self.at += 1;
            }
            let value = self.quoted()?;
            while self.peek().is_some_and(char::is_whitespace) {
                self.at += 1;
            }
            if !self.eat(">") {
                return Err(self.error("unterminated entity declaration"));
            }
            if self.entities.len() == MAX_ENTITIES {
                return Err(self.error(&format!(
                    "at most {MAX_ENTITIES} entity declarations are supported"
                )));
            }
            // A declaration may not reference another entity: that is where
            // unbounded expansion comes from, and no controller mapping needs
            // it.
            if value.contains('&') {
                return Err(self.error("an entity declaration may not reference another entity"));
            }
            self.expansion += value.chars().count();
            if self.expansion > MAX_ENTITY_TEXT {
                return Err(self.error("entity declarations expand to too much text"));
            }
            self.entities.push((name, value));
        }
    }

    fn find(&self, needle: &str) -> Option<usize> {
        let len = needle.chars().count();
        if len == 0 {
            return Some(self.at);
        }
        (self.at..=self.chars.len().saturating_sub(len)).find(|start| {
            self.chars[*start..*start + len]
                .iter()
                .zip(needle.chars())
                .all(|(have, want)| *have == want)
        })
    }

    fn name(&mut self) -> Result<String, String> {
        let start = self.at;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == ':' {
                self.at += 1;
            } else {
                break;
            }
        }
        if start == self.at {
            return Err(self.error("expected an element or attribute name"));
        }
        let name: String = self.chars[start..self.at].iter().collect();
        if name.contains(':') {
            return Err(self.error("namespace prefixes are not supported"));
        }
        Ok(name)
    }

    fn element(&mut self) -> Result<Node, String> {
        if !self.eat("<") {
            return Err(self.error("expected '<'"));
        }
        if self.starts_with("!") {
            return Err(self.error("doctypes and CDATA sections are not supported"));
        }
        let name = self.name()?;
        let mut attributes = Vec::new();
        loop {
            self.skip_misc_inside_tag()?;
            if self.eat("/>") {
                return Ok(Node {
                    name,
                    attributes,
                    text: String::new(),
                    children: Vec::new(),
                });
            }
            if self.eat(">") {
                break;
            }
            let key = self.name()?;
            self.skip_misc_inside_tag()?;
            if !self.eat("=") {
                return Err(self.error(&format!("attribute '{key}' has no value")));
            }
            self.skip_misc_inside_tag()?;
            let value = self.quoted()?;
            if attributes.iter().any(|(existing, _)| *existing == key) {
                return Err(self.error(&format!("duplicate attribute '{key}'")));
            }
            attributes.push((key, value));
        }
        let mut children = Vec::new();
        let mut text = String::new();
        loop {
            if self.at >= self.chars.len() {
                return Err(self.error(&format!("element '{name}' is never closed")));
            }
            if self.starts_with("</") {
                self.at += 2;
                let closing = self.name()?;
                if closing != name {
                    return Err(
                        self.error(&format!("element '{name}' is closed by '</{closing}>'"))
                    );
                }
                self.skip_misc_inside_tag()?;
                if !self.eat(">") {
                    return Err(self.error("expected '>' closing the end tag"));
                }
                text = text.trim().to_string();
                return Ok(Node {
                    name,
                    attributes,
                    text,
                    children,
                });
            }
            if self.starts_with("<!--") {
                let end = self
                    .find("-->")
                    .ok_or_else(|| self.error("unterminated comment"))?;
                self.at = end + 3;
                continue;
            }
            if self.starts_with("<!") {
                return Err(self.error("doctypes and CDATA sections are not supported"));
            }
            if self.peek() == Some('<') {
                children.push(self.element()?);
                continue;
            }
            let chunk = self.text_chunk()?;
            text.push_str(&chunk);
        }
    }

    fn skip_misc_inside_tag(&mut self) -> Result<(), String> {
        while self.peek().is_some_and(char::is_whitespace) {
            self.at += 1;
        }
        if self.starts_with("<!--") {
            let end = self
                .find("-->")
                .ok_or_else(|| self.error("unterminated comment"))?;
            self.at = end + 3;
            return self.skip_misc_inside_tag();
        }
        Ok(())
    }

    /// One entity reference, `&…;`, decoded. Shared by element text and by
    /// attribute values so the two cannot disagree about what an entity is.
    fn entity(&mut self) -> Result<char, String> {
        let end = self
            .find(";")
            .ok_or_else(|| self.error("unterminated entity reference"))?;
        let entity: String = self.chars[self.at + 1..end].iter().collect();
        let decoded = match entity.as_str() {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            other if self.entities.iter().any(|(name, _)| name == other) => {
                // A declared entity may hold several characters; the caller
                // pushes one, so the rest is queued by expanding in place.
                let value = self
                    .entities
                    .iter()
                    .find(|(name, _)| name == other)
                    .map(|(_, value)| value.clone())
                    .expect("just checked");
                let mut chars: Vec<char> = value.chars().collect();
                let first = chars.remove(0);
                // The rest is read as ordinary text, inserted after the entity
                // reference so it is consumed exactly once, in order.
                self.expansion += chars.len() + 1;
                if self.expansion > MAX_ENTITY_TEXT * 8 {
                    return Err(self.error("entities expand to too much text"));
                }
                for (offset, c) in chars.into_iter().enumerate() {
                    self.chars.insert(end + 1 + offset, c);
                }
                first
            }
            other => {
                let code = if let Some(hex) = other.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(decimal) = other.strip_prefix('#') {
                    decimal.parse().ok()
                } else {
                    return Err(self.error(&format!("unknown entity '&{other};'")));
                };
                code.and_then(char::from_u32)
                    .ok_or_else(|| self.error("entity is not a character"))?
            }
        };
        self.at = end + 1;
        Ok(decoded)
    }

    /// Element text up to the next `<`, with entities resolved.
    fn text_chunk(&mut self) -> Result<String, String> {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '<' {
                break;
            }
            if c == '&' {
                out.push(self.entity()?);
                continue;
            }
            out.push(c);
            self.at += 1;
        }
        Ok(out)
    }

    fn quoted(&mut self) -> Result<String, String> {
        let Some(quote) = self.peek().filter(|c| *c == '"' || *c == '\'') else {
            return Err(self.error("expected a quoted attribute value"));
        };
        self.at += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.error("unterminated attribute value"));
            };
            if c == quote {
                self.at += 1;
                return Ok(out);
            }
            if c == '&' {
                let decodable = self.entity()?;
                out.push(decodable);
                continue;
            }
            if c == '<' {
                return Err(self.error("'<' inside an attribute value"));
            }
            out.push(c);
            self.at += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_attributes_and_text() {
        let root = parse(
            r#"<?xml version="1.0" encoding="utf-8"?>
<!-- a mapping -->
<MixxxControllerPreset schemaVersion="1">
  <info><name>Numark &amp; Co</name></info>
  <controller id="Numark MixTrack Pro MIDI 1">
    <controls>
      <control><status>0xb0</status><midino>0xc</midino><key>headMix</key>
        <options><normal/></options></control>
      <control><status>0x90</status><midino>0x24</midino><key>play</key></control>
    </controls>
  </controller>
</MixxxControllerPreset>"#,
        )
        .unwrap();
        assert_eq!(root.name, "MixxxControllerPreset");
        assert_eq!(root.attribute("schemaVersion"), Some("1"));
        assert_eq!(root.text_of("info").unwrap(), "");
        let info = root.child("info").unwrap();
        assert_eq!(info.text_of("name").unwrap(), "Numark & Co");
        let controller = root.child("controller").unwrap();
        assert_eq!(
            controller.attribute("id"),
            Some("Numark MixTrack Pro MIDI 1")
        );
        let controls: Vec<&Node> = controller
            .child("controls")
            .unwrap()
            .children_named("control")
            .collect();
        assert_eq!(controls.len(), 2);
        assert_eq!(controls[0].text_of("status").unwrap(), "0xb0");
        assert_eq!(controls[0].text_of("midino").unwrap(), "0xc");
        assert_eq!(controls[0].text_of("key").unwrap(), "headMix");
        assert!(
            controls[0]
                .child("options")
                .unwrap()
                .child("normal")
                .is_some()
        );
        // An empty element is present with empty text, not absent.
        assert_eq!(controls[1].text_of("key").unwrap(), "play");
    }

    #[test]
    fn self_closing_attributes_and_entities() {
        let root = parse(
            r#"<root a="1" b='two &amp; three &lt;four&gt;'>
                 <empty/><file filename="x.js" functionprefix="P"/></root>"#,
        )
        .unwrap();
        assert_eq!(root.attribute("a"), Some("1"));
        assert_eq!(root.attribute("b"), Some("two & three <four>"));
        assert_eq!(root.children.len(), 2);
        assert!(root.children[0].children.is_empty());
        assert_eq!(root.children[1].attribute("filename"), Some("x.js"));
        assert_eq!(root.children[1].attribute("functionprefix"), Some("P"));
    }

    #[test]
    fn comments_and_numeric_entities_are_handled() {
        let root = parse("<r><!-- note --><t>a&#65;&#x42;</t></r>").unwrap();
        assert_eq!(root.text_of("t").unwrap(), "aAB");
    }

    #[test]
    fn malformed_documents_are_refused_with_a_position() {
        for (bad, expected) in [
            ("<a><b></a>", "is closed by"),
            ("<a>", "never closed"),
            ("<a x=\"1\">", "never closed"),
            ("<a x=1>", "quoted attribute value"),
            ("<a/></a>", "trailing content"),
            ("<a><![CDATA[x]]></a>", "not supported"),
            ("<ns:a/>", "namespace prefixes"),
            ("<a>&nope;</a>", "unknown entity"),
            ("<a x=\"1\" x=\"2\"/>", "duplicate attribute"),
            ("<a x>", "has no value"),
        ] {
            let error = parse(bad).unwrap_err();
            assert!(
                error.contains(expected),
                "{bad:?} gave {error:?}, wanted {expected:?}"
            );
            assert!(
                error.contains("character"),
                "the error names a position: {error:?}"
            );
        }
    }

    #[test]
    fn a_real_mapping_head_parses_with_its_own_dialect() {
        // The declarations and root names Mixxx actually writes.
        for root_name in ["MixxxMIDIPreset", "MixxxControllerPreset"] {
            let document = format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<{root_name} mixxxVersion="2.6" schemaVersion="1">
  <info>
    <name>Behringer BCR2000</name>
    <author>Christian</author>
    <description>Mixxx controller preset for a B-Control BCR2000 controller.</description>
  </info>
  <controller id="BCR2000">
    <scriptfiles>
      <file filename="midi-components-0.0.js"/>
      <file filename="Behringer-Extension-scripts.js" />
    </scriptfiles>
  </controller>
</{root_name}>"#
            );
            let root = parse(&document).unwrap();
            assert_eq!(root.name, root_name);
            assert_eq!(
                root.child("info").unwrap().text_of("author"),
                Some("Christian")
            );
            let files: Vec<&str> = root
                .child("controller")
                .unwrap()
                .child("scriptfiles")
                .unwrap()
                .children_named("file")
                .filter_map(|f| f.attribute("filename"))
                .collect();
            assert_eq!(
                files,
                ["midi-components-0.0.js", "Behringer-Extension-scripts.js"]
            );
        }
    }
}
