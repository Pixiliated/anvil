/// Byte range into a source file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }
    pub fn to(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }
    /// Zero-width span at the start of this one, for insertions.
    pub fn at_start(self) -> Span {
        Span { start: self.start, end: self.start }
    }
    pub fn at_end(self) -> Span {
        Span { start: self.end, end: self.end }
    }
}

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub name: String,
    pub text: String,
    line_starts: Vec<usize>,
}

impl SourceFile {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> SourceFile {
        let text = text.into();
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        SourceFile { name: name.into(), text, line_starts }
    }

    /// 1-based line and column (column counts characters, not bytes).
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let start = self.line_starts[line];
        let col = self.text[start..offset.min(self.text.len())].chars().count() + 1;
        (line + 1, col)
    }

    /// Byte offset of a 1-based line and character column; None past the end of that line.
    pub fn offset_of(&self, line: usize, col: usize) -> Option<usize> {
        let start = *self.line_starts.get(line.checked_sub(1)?)?;
        let text = self.line_text(line);
        if col == 0 {
            return None;
        }
        if col == text.chars().count() + 1 {
            return Some(start + text.len());
        }
        text.char_indices().nth(col - 1).map(|(i, _)| start + i)
    }

    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line - 1];
        let end = self.line_starts.get(line).copied().unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    pub fn slice(&self, span: Span) -> &str {
        &self.text[span.start..span.end]
    }

    /// "file:line:col", the location format used in panics and diagnostics.
    pub fn site(&self, span: Span) -> String {
        let (l, c) = self.line_col(span.start);
        format!("{}:{}:{}", self.name, l, c)
    }
}
