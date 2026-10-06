//! Splitting a file into records as a stream: a file of any size is read through a fixed buffer,
//! never whole. Each record carries the byte offset just past it, which is where reading
//! resumes after a restart.
//!
//! - HL7: one message per `MSH`; batch envelopes (`FHS`, `BHS`, `BTS`, `FTS`) are dropped and
//!   segments are joined with `\r`, the HL7 standard terminator.
//! - CSV: one message per record (quoted fields may span lines). With a header, each message is
//!   the header line plus the record, so every message parses on its own.
//! - Everything else (fixed-length, text, JSON Lines): one message per line.
//!
//! Lines may end in `\n`, `\r\n` or `\r` (HL7 files often use `\r` alone). Blank lines are skipped.

use std::io::{self, SeekFrom};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncSeek, AsyncSeekExt};

/// The longest record accepted: protects memory from a huge file with no line breaks.
pub const MAX_RECORD: usize = 64 << 20;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Splitter {
    Lines,
    Csv { header: bool },
    Hl7,
}

#[derive(Debug, PartialEq)]
pub struct Record {
    pub payload: Vec<u8>,
    /// Byte offset just past this record: where to resume.
    pub end: u64,
}

/// Reads lines, tracking the byte offset.
struct Lines<R> {
    inner: R,
    offset: u64,
    /// The last line ended in `\r`: a `\n` right after it belongs to the same line break.
    after_cr: bool,
}

impl<R: AsyncBufRead + Unpin> Lines<R> {
    /// The next line, without its line break, into `line`. False at the end of the file.
    async fn next(&mut self, line: &mut Vec<u8>) -> io::Result<bool> {
        line.clear();
        let mut any = false;
        loop {
            let buf = self.inner.fill_buf().await?;
            if buf.is_empty() {
                return Ok(any);
            }
            if std::mem::take(&mut self.after_cr) && buf[0] == b'\n' {
                self.inner.consume(1);
                self.offset += 1;
                continue;
            }
            any = true;
            match buf.iter().position(|&b| b == b'\n' || b == b'\r') {
                Some(i) => {
                    line.extend_from_slice(&buf[..i]);
                    self.after_cr = buf[i] == b'\r';
                    self.inner.consume(i + 1);
                    self.offset += i as u64 + 1;
                    return Ok(true);
                }
                None => {
                    let n = buf.len();
                    if line.len() + n > MAX_RECORD {
                        return Err(io::Error::other(format!(
                            "a line is longer than {} MB; is this the right file and format?",
                            MAX_RECORD >> 20
                        )));
                    }
                    line.extend_from_slice(buf);
                    self.inner.consume(n);
                    self.offset += n as u64;
                }
            }
        }
    }
}

pub struct Records<R> {
    lines: Lines<R>,
    splitter: Splitter,
    /// CSV header line, prepended to each record.
    header: Option<Vec<u8>>,
    line: Vec<u8>,
    /// HL7: the message being collected.
    message: Option<Vec<u8>>,
}

fn is_blank(line: &[u8]) -> bool {
    line.iter().all(u8::is_ascii_whitespace)
}

fn odd_quotes(bytes: &[u8]) -> bool {
    bytes.iter().filter(|&&b| b == b'"').count() % 2 == 1
}

impl<R: AsyncBufRead + AsyncSeek + Unpin> Records<R> {
    /// Start reading at `start` (0 for a new file, or where a previous read stopped). The reader
    /// must be at the start of the file: a CSV header is always read from there first.
    pub async fn open(reader: R, splitter: Splitter, start: u64) -> io::Result<Self> {
        let mut records = Self {
            lines: Lines { inner: reader, offset: 0, after_cr: false },
            splitter,
            header: None,
            line: Vec::new(),
            message: None,
        };
        if splitter == (Splitter::Csv { header: true }) {
            records.header = records.csv_record().await?;
        }
        if start > records.lines.offset {
            records.lines.inner.seek(SeekFrom::Start(start)).await?;
            records.lines.offset = start;
            records.lines.after_cr = false;
        }
        Ok(records)
    }

    pub async fn next(&mut self) -> io::Result<Option<Record>> {
        match self.splitter {
            Splitter::Lines => loop {
                if !self.lines.next(&mut self.line).await? {
                    return Ok(None);
                }
                if !is_blank(&self.line) {
                    return Ok(Some(Record { payload: self.line.clone(), end: self.lines.offset }));
                }
            },
            Splitter::Csv { .. } => {
                let Some(record) = self.csv_record().await? else { return Ok(None) };
                let payload = match &self.header {
                    Some(h) => [h.as_slice(), b"\n", &record].concat(),
                    None => record,
                };
                Ok(Some(Record { payload, end: self.lines.offset }))
            }
            Splitter::Hl7 => self.hl7_message().await,
        }
    }

    /// One CSV record: lines are joined while a quoted field is open (an odd number of quotes so
    /// far, since quotes inside a field are doubled).
    async fn csv_record(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            if !self.lines.next(&mut self.line).await? {
                return Ok(None);
            }
            if is_blank(&self.line) {
                continue;
            }
            let mut record = self.line.clone();
            while odd_quotes(&record) && self.lines.next(&mut self.line).await? {
                if record.len() + self.line.len() > MAX_RECORD {
                    return Err(io::Error::other("a CSV record is longer than 64 MB (an unclosed quote?)"));
                }
                record.push(b'\n');
                record.extend_from_slice(&self.line);
            }
            return Ok(Some(record));
        }
    }

    /// One HL7 message: it ends where the next MSH (or the batch trailer) starts, so that's the
    /// resume offset.
    async fn hl7_message(&mut self) -> io::Result<Option<Record>> {
        loop {
            let start = self.lines.offset;
            if !self.lines.next(&mut self.line).await? {
                return Ok(self.message.take().map(|payload| Record { payload, end: self.lines.offset }));
            }
            let segment = self.line.trim_ascii_start();
            let id = segment.get(..3).unwrap_or(segment);
            match id {
                b"" => continue,
                b"MSH" => {
                    if let Some(done) = self.message.replace(segment.to_vec()) {
                        return Ok(Some(Record { payload: done, end: start }));
                    }
                }
                b"FHS" | b"BHS" => continue,
                b"BTS" | b"FTS" => {
                    if let Some(done) = self.message.take() {
                        return Ok(Some(Record { payload: done, end: start }));
                    }
                }
                _ => match &mut self.message {
                    Some(m) => {
                        if m.len() + segment.len() > MAX_RECORD {
                            return Err(io::Error::other("an HL7 message is longer than 64 MB"));
                        }
                        m.push(b'\r');
                        m.extend_from_slice(segment);
                    }
                    // Segments before any MSH: keep them as a message so nothing is silently lost
                    // (it will fail in the transform and be quarantined for a human to see).
                    None => self.message = Some(segment.to_vec()),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader;

    /// Every record, starting at `start`. A tiny buffer exercises records and line breaks split
    /// across reads.
    async fn split(data: &str, splitter: Splitter, start: u64) -> Vec<(String, u64)> {
        let reader = BufReader::with_capacity(3, Cursor::new(data.as_bytes().to_vec()));
        let mut records = Records::open(reader, splitter, start).await.unwrap();
        let mut out = Vec::new();
        while let Some(r) = records.next().await.unwrap() {
            out.push((String::from_utf8(r.payload).unwrap(), r.end));
        }
        out
    }

    fn payloads(records: &[(String, u64)]) -> Vec<&str> {
        records.iter().map(|(p, _)| p.as_str()).collect()
    }

    #[tokio::test]
    async fn lines_with_any_line_break() {
        let data = "one\r\ntwo\rthree\n\n  \nfour";
        let all = split(data, Splitter::Lines, 0).await;
        assert_eq!(payloads(&all), ["one", "two", "three", "four"]);
        assert_eq!(all.last().unwrap().1, data.len() as u64);
        // Resuming at each record's end gives exactly the records after it.
        for (i, (_, end)) in all.iter().enumerate() {
            assert_eq!(payloads(&split(data, Splitter::Lines, *end).await), payloads(&all[i + 1..]));
        }
    }

    #[tokio::test]
    async fn csv_records_keep_the_header_and_quoted_line_breaks() {
        let data = "mrn,note\r\n1,plain\r\n2,\"two\r\nlines, \"\"quoted\"\"\"\r\n\r\n3,last";
        let all = split(data, Splitter::Csv { header: true }, 0).await;
        assert_eq!(
            payloads(&all),
            ["mrn,note\n1,plain", "mrn,note\n2,\"two\nlines, \"\"quoted\"\"\"", "mrn,note\n3,last"]
        );
        let resumed = split(data, Splitter::Csv { header: true }, all[0].1).await;
        assert_eq!(payloads(&resumed), payloads(&all[1..]), "the header is read again on resume");

        let bare = split("a,b\nc,d\n", Splitter::Csv { header: false }, 0).await;
        assert_eq!(payloads(&bare), ["a,b", "c,d"]);
    }

    #[tokio::test]
    async fn hl7_batches_split_per_message() {
        let data = "FHS|^~\\&|LAB\rBHS|^~\\&|LAB\rMSH|^~\\&|A|1\rPID|1||100\rMSH|^~\\&|A|2\r\nPID|1||200\r\nOBX|1\rBTS|2\rFTS|1\r";
        let all = split(data, Splitter::Hl7, 0).await;
        assert_eq!(payloads(&all), ["MSH|^~\\&|A|1\rPID|1||100", "MSH|^~\\&|A|2\rPID|1||200\rOBX|1"]);
        let resumed = split(data, Splitter::Hl7, all[0].1).await;
        assert_eq!(payloads(&resumed), payloads(&all[1..]));

        let plain = split("MSH|^~\\&|A|1\nPID|1\n", Splitter::Hl7, 0).await;
        assert_eq!(payloads(&plain), ["MSH|^~\\&|A|1\rPID|1"]);
    }

    #[tokio::test]
    async fn refuses_endless_lines() {
        let data = vec![b'x'; MAX_RECORD + 10];
        let mut records = Records::open(BufReader::new(Cursor::new(data)), Splitter::Lines, 0).await.unwrap();
        assert!(records.next().await.unwrap_err().to_string().contains("longer than 64 MB"));
    }
}
