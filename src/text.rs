//! tools for manipulating text.
//! created to parse text like Nix files.

#[derive(Debug, thiserror::Error)]
pub enum SpanError {
    #[error("bounds are out of range")]
    OutOfRange,

    #[error("keyphrase '{0}' not found in span")]
    NotFound(String),

    #[error(
        "replacement text length does not match span length: expected {expected}, found {found}"
    )]
    ReplaceLengthMismatch { expected: usize, found: usize },
}

/// a simple span structure to hold start and end positions.
#[derive(Debug, Clone)]
pub struct Span<'text> {
    text: &'text str,
    start: usize,
    end: usize,
}

impl<'text> Span<'text> {
    pub fn new(text: &'text str, start: usize, end: usize) -> Result<Self, SpanError> {
        if start > end || end > text.len() {
            return Err(SpanError::OutOfRange);
        }
        Ok(Span { text, start, end })
    }

    pub fn slice(self, start_offset: usize, end_offset: usize) -> Result<Span<'text>, SpanError> {
        let new_start = self.start + start_offset;
        let new_end = self.start + end_offset;
        Span::new(self.text, new_start, new_end)
    }

    /// fastforward the `start` position to the first occurrence of `keyphrase`
    pub fn find(self, keyphrase: &str) -> Result<Span<'text>, SpanError> {
        if let Some(pos) = self.as_str().find(keyphrase) {
            let start = self.start + pos + keyphrase.len();
            let end = self.end;

            Ok(Span {
                text: self.text,
                start,
                end,
            })
        } else {
            Err(SpanError::NotFound(keyphrase.to_string()))
        }
    }

    /// given an input string, a starting delimiter character, and a starting position,
    /// find the position of the matching closing delimiter.
    pub fn get_matching_delimiters(
        self,
        open_delimiter: char,
        close_delimiter: char,
    ) -> Option<Span<'text>> {
        let mut depth: usize = 0;

        for (i, c) in self.as_str().char_indices() {
            if c == open_delimiter {
                depth += 1;
            } else if c == close_delimiter {
                depth -= 1;
                if depth == 0 {
                    return Some(Span {
                        text: self.text,
                        start: self.start,
                        end: self.start + i + c.len_utf8(),
                    });
                }
            }
        }
        None
    }

    pub fn get_inner_delimiter(self, delimiter: char) -> Option<Span<'text>> {
        let mut chars = self.as_str().char_indices();

        let start_pos = chars.find(|&(_, c)| c == delimiter)?.0 + delimiter.len_utf8();
        let end_pos = chars.skip(start_pos).find(|&(_, c)| c == delimiter)?.0;

        Some(Span {
            text: self.text,
            start: self.start + start_pos,
            end: self.start + end_pos,
        })
    }

    /// replace the contents of the span with new text
    pub fn replace(&'text self, new_text: &str) -> Result<String, SpanError> {
        if self.len() != new_text.len() {
            Err(SpanError::ReplaceLengthMismatch {
                expected: self.len(),
                found: new_text.len(),
            })
        } else {
            let mut result = String::with_capacity(self.text.len() - self.len() + new_text.len());
            result.push_str(&self.text[..self.start]);
            result.push_str(new_text);
            result.push_str(&self.text[self.end..]);
            Ok(result)
        }
    }

    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn as_str(&'text self) -> &'text str {
        &self.text[self.start..self.end]
    }
}

impl<'text> From<&'text str> for Span<'text> {
    fn from(text: &'text str) -> Self {
        Span {
            text,
            start: 0,
            end: text.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_and_as_str_agree() {
        let span = Span::from("Hello, world!");
        let sub_span = span.slice(7, 12).expect("failed to create sub span");

        assert_eq!(sub_span.len(), sub_span.as_str().len());
        assert_eq!(sub_span.as_str(), "world");
    }
}
