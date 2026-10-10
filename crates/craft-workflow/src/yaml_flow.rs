//! Flow-style values: `[a, b]` lists and `{k: v}` maps on one line.

use crate::yaml::{MAX_DEPTH, MAX_MAP_KEYS, MAX_SEQ_ITEMS, YValue};

pub(crate) fn parse_flow(text: &str) -> Result<YValue, String> {
    let mut flow = Flow { text, pos: 0 };
    let value = flow.value(0)?;
    let rest = text[flow.pos..].trim();
    if !rest.is_empty() && !rest.starts_with('#') {
        return Err(format!("trailing content after value: `{rest}`"));
    }
    Ok(value)
}

struct Flow<'a> {
    text: &'a str,
    pos: usize,
}

impl Flow<'_> {
    fn byte(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn ws(&mut self) {
        while self.byte() == Some(b' ') {
            self.pos += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<YValue, String> {
        if depth > MAX_DEPTH {
            return Err(format!("nesting deeper than {MAX_DEPTH} levels"));
        }
        match self.byte() {
            Some(b'[') => self.seq(depth + 1),
            Some(b'{') => self.map(depth + 1),
            _ => Ok(YValue::Str(self.scalar()?)),
        }
    }

    fn seq(&mut self, depth: usize) -> Result<YValue, String> {
        self.pos += 1;
        let mut items = Vec::new();
        self.ws();
        if self.byte() == Some(b']') {
            self.pos += 1;
            return Ok(YValue::Seq(items));
        }
        loop {
            if items.len() >= MAX_SEQ_ITEMS {
                return Err(format!("more than {MAX_SEQ_ITEMS} list items"));
            }
            items.push(self.value(depth + 1)?);
            if !self.delimiter(b']', "expected `,` or `]` in list")? {
                return Ok(YValue::Seq(items));
            }
        }
    }

    fn map(&mut self, depth: usize) -> Result<YValue, String> {
        self.pos += 1;
        let mut entries = Vec::new();
        self.ws();
        if self.byte() == Some(b'}') {
            self.pos += 1;
            return Ok(YValue::Map(entries));
        }
        loop {
            if entries.len() >= MAX_MAP_KEYS {
                return Err(format!("more than {MAX_MAP_KEYS} keys in one map"));
            }
            let key = self.key()?;
            if self.byte() != Some(b':') {
                return Err(format!("expected `:` after key `{key}`"));
            }
            self.pos += 1;
            self.ws();
            let value = self.value(depth + 1)?;
            entries.push((key, value));
            if !self.delimiter(b'}', "expected `,` or `}` in map")? {
                return Ok(YValue::Map(entries));
            }
        }
    }

    /// Consumes `,` (true) or the closing `close` byte (false); anything
    /// else is the given error.
    fn delimiter(&mut self, close: u8, error: &str) -> Result<bool, String> {
        self.ws();
        match self.byte() {
            Some(b',') => {
                self.pos += 1;
                self.ws();
                Ok(true)
            }
            Some(b) if b == close => {
                self.pos += 1;
                Ok(false)
            }
            _ => Err(error.to_string()),
        }
    }

    fn key(&mut self) -> Result<String, String> {
        let end = self.text[self.pos..]
            .find([':', ',', '}'])
            .map(|rel| self.pos + rel)
            .unwrap_or(self.text.len());
        let key = self.text[self.pos..end].trim();
        if key.is_empty() {
            return Err("empty key in flow map".to_string());
        }
        self.pos = end;
        Ok(key.to_string())
    }

    fn scalar(&mut self) -> Result<String, String> {
        match self.byte() {
            Some(quote @ (b'"' | b'\'')) => self.quoted(quote),
            _ => Ok(self.bare()),
        }
    }

    fn quoted(&mut self, quote: u8) -> Result<String, String> {
        let start = self.pos + 1;
        let end = self.text[start..]
            .find(quote as char)
            .map(|rel| start + rel)
            .ok_or_else(|| "unterminated quote".to_string())?;
        self.pos = end + 1;
        Ok(self.text[start..end].to_string())
    }

    fn bare(&mut self) -> String {
        let bytes = self.text.as_bytes();
        let mut end = self.pos;
        while let Some(&b) = bytes.get(end) {
            if matches!(b, b',' | b']' | b'}') {
                break;
            }
            end += 1;
        }
        let raw = self.text[self.pos..end].trim();
        let cut = raw.find(" #").unwrap_or(raw.len());
        self.pos = end;
        raw[..cut].trim_end().to_string()
    }
}
