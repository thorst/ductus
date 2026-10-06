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
    #[error("invalid HL7 path `{0}` (expected e.g. `PID-3` or `PID-3.1`)")]
    InvalidPath(String),
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
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub segment: String,
    pub field: usize,
    pub component: Option<usize>,
}

impl FromStr for Path {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let invalid = || Error::InvalidPath(s.to_string());
        let (segment, rest) = s.split_once('-').ok_or_else(invalid)?;
        if segment.len() != 3 || !segment.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(invalid());
        }
        let (field, component) = match rest.split_once('.') {
            Some((f, c)) => (f, Some(c.parse::<usize>().map_err(|_| invalid())?)),
            None => (rest, None),
        };
        let field = field.parse::<usize>().map_err(|_| invalid())?;
        if field == 0 || component == Some(0) {
            return Err(invalid());
        }
        Ok(Path { segment: segment.to_ascii_uppercase(), field, component })
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.segment, self.field)?;
        if let Some(c) = self.component {
            write!(f, ".{c}")?;
        }
        Ok(())
    }
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

    /// Value at `path`, first repetition. Anything missing reads as `""`.
    pub fn get(&self, path: &Path) -> &'a str {
        let Some(segment) = self.segment(&path.segment) else { return "" };
        // MSH-1 is the field separator itself, so MSH field n sits at split index n-1.
        let index = if path.segment == "MSH" { path.field - 1 } else { path.field };
        let Some(field) = segment.split(self.delims.field).nth(index) else { return "" };
        let first_rep = field.split(self.delims.repetition).next().unwrap_or("");
        match path.component {
            None => first_rep,
            Some(c) => first_rep.split(self.delims.component).nth(c - 1).unwrap_or(""),
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
        assert!("PID3".parse::<Path>().is_err());
        assert!("PID-0".parse::<Path>().is_err());
    }
}
