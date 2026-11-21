//! HL7v2 essentials: delimiters, field lookup by path, and ACK generation.
//!
//! v0.1 scope. The lazy offset index and the `msg.PID[3][1]` accessors for
//! TypeScript arrive in v0.2.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum Error {
    #[error("message does not start with an MSH segment")]
    MissingMsh,
    #[error("MSH segment is too short to contain delimiters")]
    ShortMsh,
    #[error(
        "invalid HL7 path `{0}`: {1} (e.g. `PID-3.1`, `PID-3[2].1`, `PID-3[.5=MR].1`, `PID-3[.5=MR|.4=HOSP].1`, `PID-3.4.1`)"
    )]
    InvalidPath(String, &'static str),
}

/// The five HL7 delimiters declared in MSH-1 and MSH-2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Delimiters {
    pub field: char,
    pub component: char,
    pub repetition: char,
    pub escape: char,
    pub subcomponent: char,
}

/// A location in a message, using HL7 numbering: `PID-3.1` = PID, field 3, component 1.
///
/// | path | reads |
/// |---|---|
/// | `PID-3` | the field's first repetition, as written |
/// | `PID-3.1` | its first component, escapes decoded |
/// | `PID-3.4.1` | the first subcomponent of component 4 |
/// | `PID-3[2].1` | the second repetition's first component |
/// | `PID-3[.5=MR].1` | the first repetition whose component 5 is `MR` |
/// | `PID-3[.5=MR\|.4=HOSP].1` | that, else the first whose component 4 is `HOSP` |
///
/// A filter compares a component (or `.4.1`, a subcomponent) decoded, exactly; a value with `|`,
/// `]` or spaces at its ends is quoted (`[.4='A|B']`). The SDK's `msg.get` reads the same syntax
/// (`sdk/src/hl7.ts`), checked by `sdk/tests/key-paths.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub segment: String,
    pub field: usize,
    pub repetition: Repetition,
    pub component: Option<usize>,
    /// Only with a component.
    pub subcomponent: Option<usize>,
}

/// Which repetition of a field a path reads.
#[derive(Debug, Clone, PartialEq)]
pub enum Repetition {
    First,
    /// From 1.
    Index(usize),
    /// The first repetition the first filter matches; if none does, the second filter's; and so on.
    Where(Vec<Filter>),
}

/// `.5=MR`: a repetition whose component 5 (decoded) is `MR`.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub component: usize,
    pub subcomponent: Option<usize>,
    pub value: String,
}

impl FromStr for Path {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let invalid = |why| Error::InvalidPath(s.to_string(), why);
        let (segment, rest) = s.split_once('-').ok_or(invalid("no `-` between the segment and the field"))?;
        if segment.len() != 3 || !segment.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(invalid("a segment is three letters or digits"));
        }
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let field = number(&rest[..digits]).ok_or(invalid("the field is a number from 1"))?;
        let mut rest = &rest[digits..];
        let mut repetition = Repetition::First;
        if let Some(inside) = rest.strip_prefix('[') {
            let end = closing(inside).ok_or(invalid("a `[` without its `]`"))?;
            repetition = repetition_of(&inside[..end]).map_err(invalid)?;
            rest = &inside[end + 1..];
        }
        let (component, subcomponent) = if rest.is_empty() {
            (None, None)
        } else {
            let (c, s) = components(rest.strip_prefix('.').ok_or(invalid("`.` before a component"))?)
                .ok_or(invalid("components are numbers from 1, at most two deep (`.4.1`)"))?;
            (Some(c), s)
        };
        Ok(Path { segment: segment.to_ascii_uppercase(), field, repetition, component, subcomponent })
    }
}

/// A number from 1.
fn number(text: &str) -> Option<usize> {
    text.parse().ok().filter(|n| *n > 0 && text.bytes().all(|b| b.is_ascii_digit()))
}

/// `4` or `4.1`.
fn components(text: &str) -> Option<(usize, Option<usize>)> {
    match text.split_once('.') {
        Some((c, s)) => Some((number(c)?, Some(number(s)?))),
        None => Some((number(text)?, None)),
    }
}

/// Where the `]` closing a bracket is (`text` starts inside it), past quoted values.
fn closing(text: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, ']') => return Some(i),
            (None, _) => {}
        }
    }
    None
}

/// What's inside a field's brackets: `2`, or filters `.5=MR|.4=HOSP`.
fn repetition_of(inside: &str) -> Result<Repetition, &'static str> {
    if inside.bytes().all(|b| b.is_ascii_digit()) {
        return number(inside).map(Repetition::Index).ok_or("a repetition is a number from 1 (`[2]`)");
    }
    let mut filters = Vec::new();
    let mut rest = inside;
    loop {
        let (place, after) = rest.split_once('=').ok_or("a filter is `.component=value` (`[.5=MR]`)")?;
        let (component, subcomponent) = place
            .strip_prefix('.')
            .and_then(components)
            .ok_or("a filter names a component from 1 after a `.` (`[.5=MR]`, `[.4.1=HOSP]`)")?;
        let (value, after) = filter_value(after)?;
        filters.push(Filter { component, subcomponent, value });
        match after.strip_prefix('|') {
            Some(next) => rest = next,
            None if after.is_empty() => return Ok(Repetition::Where(filters)),
            None => return Err("after a quoted value, `|` or `]`"),
        }
    }
}

/// A filter's value and what follows it: `'A|B'` or `"A]"` quoted, else up to the next `|`.
fn filter_value(text: &str) -> Result<(String, &str), &'static str> {
    match text.chars().next() {
        Some(q @ ('\'' | '"')) => {
            let end = text[1..].find(q).ok_or("a quoted value without its closing quote")?;
            Ok((text[1..1 + end].to_string(), &text[end + 2..]))
        }
        _ => {
            let end = text.find('|').unwrap_or(text.len());
            Ok((text[..end].to_string(), &text[end..]))
        }
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.segment, self.field)?;
        match &self.repetition {
            Repetition::First => {}
            Repetition::Index(n) => write!(f, "[{n}]")?,
            Repetition::Where(filters) => {
                f.write_str("[")?;
                for (i, filter) in filters.iter().enumerate() {
                    let quote = if filter.value.contains('\'') { '"' } else { '\'' };
                    let plain = !filter.value.contains(['|', ']', '\'', '"']) && filter.value.trim() == filter.value;
                    let sep = if i == 0 { "" } else { "|" };
                    write!(f, "{sep}.{}", filter.component)?;
                    if let Some(s) = filter.subcomponent {
                        write!(f, ".{s}")?;
                    }
                    match plain {
                        true => write!(f, "={}", filter.value)?,
                        false => write!(f, "={quote}{}{quote}", filter.value)?,
                    }
                }
                f.write_str("]")?;
            }
        }
        if let Some(c) = self.component {
            write!(f, ".{c}")?;
        }
        if let Some(s) = self.subcomponent {
            write!(f, ".{s}")?;
        }
        Ok(())
    }
}

/// A value's escape sequences decoded, in the SDK's order (`decode` in `sdk/src/hl7.ts`): `\F\`,
/// `\S\`, `\T\`, `\R\`, then `\E\`.
pub fn decode<'a>(value: &'a str, d: &Delimiters) -> std::borrow::Cow<'a, str> {
    let e = d.escape;
    if !value.contains(e) {
        return value.into();
    }
    let mut out = value.to_string();
    for (code, to) in [('F', d.field), ('S', d.component), ('T', d.subcomponent), ('R', d.repetition), ('E', e)] {
        out = out.replace(&format!("{e}{code}{e}"), &to.to_string());
    }
    out.into()
}

/// Acknowledgment codes for MSA-1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AckCode {
    /// Application Accept
    Accept,
    /// Application Error: the sender should retry.
    Error,
    /// Application Reject: retrying won't help.
    Reject,
}

impl AckCode {
    pub fn as_str(self) -> &'static str {
        match self {
            AckCode::Accept => "AA",
            AckCode::Error => "AE",
            AckCode::Reject => "AR",
        }
    }
}

/// A read-only view over a raw HL7v2 message.
pub struct Message<'a> {
    raw: &'a str,
    delims: Delimiters,
}

impl<'a> Message<'a> {
    pub fn parse(raw: &'a str) -> Result<Self, Error> {
        let raw = raw.trim_start_matches(['\r', '\n']);
        if !raw.starts_with("MSH") {
            return Err(Error::MissingMsh);
        }
        let mut chars = raw[3..].chars();
        let mut next = || chars.next().ok_or(Error::ShortMsh);
        let delims = Delimiters {
            field: next()?,
            component: next()?,
            repetition: next()?,
            escape: next()?,
            subcomponent: next()?,
        };
        Ok(Self { raw, delims })
    }

    pub fn delimiters(&self) -> Delimiters {
        self.delims
    }

    pub fn segments(&self) -> impl Iterator<Item = &'a str> {
        self.raw.split(['\r', '\n']).filter(|s| !s.is_empty())
    }

    /// The first segment with this name.
    pub fn segment(&self, name: &str) -> Option<&'a str> {
        let field = self.delims.field;
        self.segments().find(|s| s.get(..3) == Some(name) && s[3..].chars().next().is_none_or(|c| c == field))
    }

    /// Value at `path` as written (escapes not decoded): the repetition it names (the first by
    /// default), its component and subcomponent. Anything missing reads as `""`.
    pub fn get(&self, path: &Path) -> &'a str {
        let Some(segment) = self.segment(&path.segment) else { return "" };
        if path.segment == "MSH" && path.field == 1 {
            return &segment[3..3 + self.delims.field.len_utf8()];
        }
        // MSH-1 is the field separator itself, so MSH field n sits at split index n-1.
        let index = if path.segment == "MSH" { path.field - 1 } else { path.field };
        let Some(field) = segment.split(self.delims.field).nth(index) else { return "" };
        if path.segment == "MSH" && path.field <= 2 {
            return field; // the delimiters themselves: never split (as the SDK reads them)
        }
        let mut repetitions = field.split(self.delims.repetition);
        let repetition = match &path.repetition {
            Repetition::First => repetitions.next(),
            Repetition::Index(n) => repetitions.nth(n - 1),
            Repetition::Where(filters) => filters.iter().find_map(|f| {
                field
                    .split(self.delims.repetition)
                    .find(|r| decode(self.part(r, Some(f.component), f.subcomponent), &self.delims) == f.value.as_str())
            }),
        };
        repetition.map(|r| self.part(r, path.component, path.subcomponent)).unwrap_or("")
    }

    /// A component (and subcomponent) of one repetition; the whole of it without one.
    fn part(&self, repetition: &'a str, component: Option<usize>, subcomponent: Option<usize>) -> &'a str {
        let Some(c) = component else { return repetition };
        let component = repetition.split(self.delims.component).nth(c - 1).unwrap_or("");
        match subcomponent {
            Some(s) => component.split(self.delims.subcomponent).nth(s - 1).unwrap_or(""),
            None => component,
        }
    }

    /// Value at `path` as interface code reads it (the SDK's `msg.get`): a component's (or
    /// subcomponent's) escape sequences decoded (`\T\` → `&`), a whole field or repetition as
    /// written. What ordering and dedupe keys read, so a key means the same in config and in code.
    pub fn read(&self, path: &Path) -> std::borrow::Cow<'a, str> {
        let raw = self.get(path);
        match path.component {
            Some(_) => decode(raw, &self.delims),
            None => raw.into(),
        }
    }

    /// Shorthand for `get` with a path string. Invalid paths read as `""`.
    pub fn value(&self, path: &str) -> &'a str {
        path.parse().map(|p| self.get(&p)).unwrap_or("")
    }

    pub fn control_id(&self) -> &'a str {
        self.value("MSH-10")
    }

    /// Build an ACK for this message. `timestamp` is HL7 TS format, e.g. `20261005120000`.
    pub fn ack(&self, code: AckCode, text: &str, timestamp: &str) -> String {
        let d = self.delims;
        let f = d.field;
        let encoding: String = [d.component, d.repetition, d.escape, d.subcomponent].iter().collect();
        let trigger = self.value("MSH-9.2");
        format!(
            "MSH{f}{encoding}{f}{}{f}{}{f}{}{f}{}{f}{timestamp}{f}{f}ACK{c}{trigger}{c}ACK{f}{}{f}{}{f}{}\rMSA{f}{}{f}{}{f}{text}\r",
            self.value("MSH-5"),
            self.value("MSH-6"),
            self.value("MSH-3"),
            self.value("MSH-4"),
            self.control_id(),
            self.value("MSH-11"),
            self.value("MSH-12"),
            code.as_str(),
            self.control_id(),
            c = d.component,
        )
    }
}

/// ACK for input that isn't parseable HL7 at all.
pub fn reject_unparseable(text: &str, timestamp: &str) -> String {
    format!("MSH|^~\\&|DUCTUS||||{timestamp}||ACK|||2.5\rMSA|AR||{text}\r")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADT: &str = "MSH|^~\\&|EPIC|HOSP|DUCTUS|HOSP|20261005120000||ADT^A08|MSG0001|P|2.5\rEVN|A08|20261005120000\rPID|1||123456^^^MRN~999^^^SSN||SMITH^JOHN||19800101|M\r";

    #[test]
    fn reads_fields_with_hl7_numbering() {
        let m = Message::parse(ADT).unwrap();
        assert_eq!(m.value("MSH-3"), "EPIC");
        assert_eq!(m.value("MSH-9.2"), "A08");
        assert_eq!(m.control_id(), "MSG0001");
        assert_eq!(m.value("PID-3.1"), "123456");
        assert_eq!(m.value("PID-5.2"), "JOHN");
        assert_eq!(m.value("PID-8"), "M");
    }

    #[test]
    fn reads_a_component_decoded_like_the_sdk() {
        let m = Message::parse("MSH|^~\\&|A|B|C|D|1||ADT^A08|1|P|2.5\rPID|1||AB\\T\\C\\E\\X^^^MRN\r").unwrap();
        let path = |p: &str| p.parse::<Path>().unwrap();
        assert_eq!(m.read(&path("PID-3.1")), "AB&C\\X");
        // A whole field reads as written, as the SDK's `msg.get("PID-3")` does.
        assert_eq!(m.read(&path("PID-3")), "AB\\T\\C\\E\\X^^^MRN");
        assert_eq!(m.read(&path("PID-3.4")), "MRN");
    }

    #[test]
    fn missing_reads_empty() {
        let m = Message::parse(ADT).unwrap();
        assert_eq!(m.value("PV1-3"), "");
        assert_eq!(m.value("PID-99"), "");
        assert_eq!(m.value("PID-5.9"), "");
    }

    #[test]
    fn builds_ack() {
        let m = Message::parse(ADT).unwrap();
        let ack = m.ack(AckCode::Accept, "", "20261005120001");
        let a = Message::parse(&ack).unwrap();
        assert_eq!(a.value("MSH-3"), "DUCTUS");
        assert_eq!(a.value("MSH-5"), "EPIC");
        assert_eq!(a.value("MSH-9.1"), "ACK");
        assert_eq!(a.value("MSA-1"), "AA");
        assert_eq!(a.value("MSA-2"), "MSG0001");
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(Message::parse("PID|1").err(), Some(Error::MissingMsh));
        for bad in [
            "PID3",
            "PID-0",
            "PID-3.0",
            "PID-3.1.2.3",
            "PID-3[0]",
            "PID-3[",
            "PID-3[5=MR]",
            "PID-3[.5]",
            "PID-3[.5=MR|]",
            "PID-3[.5='MR]",
            "PID-3[.5='MR'x]",
            "PID-3[2]x",
            "PID-3x",
            "PI-3",
            "PID-",
        ] {
            assert!(bad.parse::<Path>().is_err(), "{bad} should be refused");
        }
        let why = "PID-3[.5]".parse::<Path>().unwrap_err().to_string();
        assert!(why.contains("a filter is `.component=value`"), "{why}");
    }

    const IDS: &str = "MSH|^~\\&|A|B|C|D|1||ADT^A08|1|P|2.5\r\
        PID|1||999^^^SSA^SS~123^^^HOSP&1.2.3&ISO^MR~456^^^CLINIC^PI~A\\T\\B^^^ESC^XX\r";

    fn read(path: &str) -> String {
        Message::parse(IDS).unwrap().read(&path.parse().unwrap()).into_owned()
    }

    #[test]
    fn repetitions_by_index_and_by_filter() {
        assert_eq!(read("PID-3.1"), "999", "the first repetition by default");
        assert_eq!(read("PID-3[2].1"), "123");
        assert_eq!(read("PID-3[9].1"), "");
        assert_eq!(read("PID-3[.5=MR].1"), "123");
        assert_eq!(read("PID-3[.5=XX].1"), "A&B", "escapes decoded in the value read");
        assert_eq!(read("PID-3[.5=NONE].1"), "", "no repetition matches: empty");
        assert_eq!(read("PID-3[.5=MR]"), "123^^^HOSP&1.2.3&ISO^MR", "a repetition as written");
        assert_eq!(read("PID-3[.5=NONE|.4=CLINIC].1"), "456", "alternatives in order");
        assert_eq!(read("PID-3[.4=CLINIC|.5=MR].1"), "456", "the first alternative wins, wherever it is");
        assert_eq!(read("PID-3[.4.1=HOSP].1"), "123", "a filter on a subcomponent");
        assert_eq!(read("PID-3[.1='A&B'].5"), "XX", "a filter compares decoded values");
    }

    #[test]
    fn subcomponents() {
        assert_eq!(read("PID-3[.5=MR].4.1"), "HOSP");
        assert_eq!(read("PID-3[.5=MR].4.2"), "1.2.3");
        assert_eq!(read("PID-3[.5=MR].4.9"), "");
        assert_eq!(read("PID-3[.5=MR].4"), "HOSP&1.2.3&ISO", "a component holds its subcomponents");
        assert_eq!(read("PID-3.4.1"), "SSA", "a component without subcomponents is its first");
    }

    #[test]
    fn msh_delimiters_read_whole() {
        assert_eq!(read("MSH-1"), "|");
        assert_eq!(read("MSH-2"), "^~\\&");
    }

    #[test]
    fn paths_display_as_written() {
        for path in ["PID-3", "PID-3.1", "PID-3.4.1", "PID-3[2].1", "PID-3[.5=MR|.4.1=HOSP].1", "PID-3[.4='A|B']"] {
            assert_eq!(path.parse::<Path>().unwrap().to_string(), path);
        }
    }
}
