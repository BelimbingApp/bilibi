//! CSV in and out (issue 133, "CSV safety"). Import reads RFC 4180 text in
//! UTF-8, with or without a byte-order mark, and refuses anything else in
//! words: other encodings, binary content, a missing or repeated header, a
//! row wider than the header, an unclosed quote. Files Bilibi writes are
//! UTF-8 with a byte-order mark and CRLF lines, and a cell a spreadsheet would
//! run as a formula (`=`, `+`, `-`, `@`, tab, carriage return) is prefixed
//! with an apostrophe so it opens as text.

/// The widest header accepted.
pub const MAX_COLUMNS: usize = 100;
/// The longest cell accepted, in bytes; contracts bound their own fields.
pub const MAX_CELL: usize = 32_768;
const BOM: &[u8] = b"\xEF\xBB\xBF";

pub struct Csv {
    pub header: Vec<String>,
    /// Data rows, each exactly as wide as the header. Blank lines are left out.
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug)]
pub enum CsvError {
    NotUtf8,
    Binary,
    Empty,
    BlankColumn(usize),
    RepeatedColumn(String),
    TooManyColumns,
    TooManyRows(usize),
    Wide(usize),
    Unclosed(usize),
    CellTooLong(usize),
}

impl CsvError {
    /// The sentence the page shows.
    pub fn message(&self) -> String {
        match self {
            Self::NotUtf8 => {
                "The file is not UTF-8 text. Save it as \"CSV UTF-8\" and choose it again.".into()
            }
            Self::Binary => "The file is not a CSV file.".into(),
            Self::Empty => "The file has no header row.".into(),
            Self::BlankColumn(n) => format!("Column {n} of the header has no name."),
            Self::RepeatedColumn(name) => format!("The header names the column \"{name}\" twice."),
            Self::TooManyColumns => format!("The file has more than {MAX_COLUMNS} columns."),
            Self::TooManyRows(max) => {
                format!("The file has more than {max} rows. Split it into smaller files.")
            }
            Self::Wide(row) => format!("Row {row} has more cells than the header."),
            Self::Unclosed(row) => format!("Row {row} opens a quote that is never closed."),
            Self::CellTooLong(row) => format!("Row {row} has a cell longer than {MAX_CELL} bytes."),
        }
    }
}

/// Reads a whole file. `max_rows` bounds the data rows.
pub fn parse(bytes: &[u8], max_rows: usize) -> Result<Csv, CsvError> {
    let bytes = bytes.strip_prefix(BOM).unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| CsvError::NotUtf8)?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
    {
        return Err(CsvError::Binary);
    }
    let mut records = Records::new(text);
    let header = loop {
        match records.next_record()? {
            None => return Err(CsvError::Empty),
            Some(cells) if cells.iter().all(|cell| cell.trim().is_empty()) => continue,
            Some(cells) => break cells,
        }
    };
    if header.len() > MAX_COLUMNS {
        return Err(CsvError::TooManyColumns);
    }
    let header: Vec<String> = header.iter().map(|name| name.trim().to_owned()).collect();
    for (index, name) in header.iter().enumerate() {
        if name.is_empty() {
            return Err(CsvError::BlankColumn(index + 1));
        }
        if header[..index]
            .iter()
            .any(|other| other.eq_ignore_ascii_case(name))
        {
            return Err(CsvError::RepeatedColumn(name.clone()));
        }
    }
    let mut rows = Vec::new();
    while let Some(mut cells) = records.next_record()? {
        if cells.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        if rows.len() == max_rows {
            return Err(CsvError::TooManyRows(max_rows));
        }
        if cells.len() > header.len() {
            // Trailing empty cells (a spreadsheet's stray commas) are not data.
            while cells.len() > header.len() && cells.last().is_some_and(String::is_empty) {
                cells.pop();
            }
            if cells.len() > header.len() {
                return Err(CsvError::Wide(rows.len() + 1));
            }
        }
        if cells.iter().any(|cell| cell.len() > MAX_CELL) {
            return Err(CsvError::CellTooLong(rows.len() + 1));
        }
        cells.resize(header.len(), String::new());
        rows.push(cells);
    }
    Ok(Csv { header, rows })
}

struct Records<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    /// Data records read so far, for messages (the header is record zero).
    read: usize,
}

impl<'a> Records<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            chars: text.chars().peekable(),
            read: 0,
        }
    }
    /// One record, or `None` at the end of the text.
    fn next_record(&mut self) -> Result<Option<Vec<String>>, CsvError> {
        if self.chars.peek().is_none() {
            return Ok(None);
        }
        let mut cells = Vec::new();
        let mut cell = String::new();
        let mut quoted = false;
        let mut started = false;
        loop {
            let Some(c) = self.chars.next() else {
                if quoted {
                    return Err(CsvError::Unclosed(self.read));
                }
                cells.push(cell);
                break;
            };
            if quoted {
                if c == '"' {
                    if self.chars.peek() == Some(&'"') {
                        self.chars.next();
                        cell.push('"');
                    } else {
                        quoted = false;
                    }
                } else {
                    cell.push(c);
                }
                continue;
            }
            match c {
                '"' if !started => {
                    quoted = true;
                    started = true;
                }
                ',' => {
                    cells.push(std::mem::take(&mut cell));
                    started = false;
                }
                '\r' if self.chars.peek() == Some(&'\n') => {}
                '\n' | '\r' => {
                    cells.push(cell);
                    break;
                }
                c => {
                    cell.push(c);
                    started = true;
                }
            }
        }
        self.read += 1;
        Ok(Some(cells))
    }
}

/// A file Bilibi hands out: byte-order mark, header, rows, CRLF lines.
pub fn write<'a>(
    header: impl IntoIterator<Item = &'a str>,
    rows: impl IntoIterator<Item = Vec<String>>,
) -> Vec<u8> {
    let mut out = Writer::new(header);
    for row in rows {
        out.row(row.iter().map(String::as_str));
    }
    out.finish()
}

/// The same file built a row at a time, so a caller can stop once it is
/// larger than it may be instead of holding every row and then the file.
pub struct Writer(String);

impl Writer {
    pub fn new<'a>(header: impl IntoIterator<Item = &'a str>) -> Self {
        let mut out = String::from("\u{FEFF}");
        line(&mut out, header);
        Self(out)
    }
    pub fn row<'a>(&mut self, cells: impl IntoIterator<Item = &'a str>) {
        line(&mut self.0, cells);
    }
    /// Bytes written so far.
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn finish(self) -> Vec<u8> {
        self.0.into_bytes()
    }
}

fn line<'a>(out: &mut String, cells: impl IntoIterator<Item = &'a str>) {
    for (index, value) in cells.into_iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let guarded;
        let value = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
            guarded = format!("'{value}");
            guarded.as_str()
        } else {
            value
        };
        if value.contains([',', '"', '\r', '\n']) || value.trim() != value {
            out.push('"');
            out.push_str(&value.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(value);
        }
    }
    out.push_str("\r\n");
}
