//! Stored BASIC program: ordered map from line number to raw text.
//!
//! MVP-2 keeps the source as plain ASCII text. The Spectrum-style tokenised
//! byte representation (one-byte keyword tokens + hidden 5-byte numeric
//! literals) lands later, when LIST detokenisation and `.tap` import become
//! interesting.

use std::collections::BTreeMap;

#[derive(Default)]
pub struct Program {
    lines: BTreeMap<u16, String>,
}

impl Program {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace the line at `number`. Empty `text` deletes the line.
    pub fn upsert(&mut self, number: u16, text: String) {
        if text.trim().is_empty() {
            self.lines.remove(&number);
        } else {
            self.lines.insert(number, text);
        }
    }

    pub fn delete(&mut self, number: u16) {
        self.lines.remove(&number);
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u16, &str)> {
        self.lines.iter().map(|(n, s)| (*n, s.as_str()))
    }

    /// Find the smallest line number whose value is `>= n`.
    pub fn next_at_or_after(&self, n: u16) -> Option<u16> {
        self.lines.range(n..).next().map(|(k, _)| *k)
    }

    /// Get the text of line `n`, if present.
    pub fn get(&self, n: u16) -> Option<&str> {
        self.lines.get(&n).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_orders_by_number() {
        let mut p = Program::new();
        p.upsert(20, "PRINT B".into());
        p.upsert(10, "PRINT A".into());
        p.upsert(30, "PRINT C".into());
        let collected: Vec<_> = p.iter().collect();
        assert_eq!(
            collected,
            vec![(10, "PRINT A"), (20, "PRINT B"), (30, "PRINT C")]
        );
    }

    #[test]
    fn empty_text_deletes() {
        let mut p = Program::new();
        p.upsert(10, "PRINT A".into());
        p.upsert(10, "".into());
        assert!(p.is_empty());
    }

    #[test]
    fn next_at_or_after_skips_gaps() {
        let mut p = Program::new();
        p.upsert(10, "A".into());
        p.upsert(20, "B".into());
        p.upsert(40, "C".into());
        assert_eq!(p.next_at_or_after(0), Some(10));
        assert_eq!(p.next_at_or_after(15), Some(20));
        assert_eq!(p.next_at_or_after(25), Some(40));
        assert_eq!(p.next_at_or_after(50), None);
    }
}
