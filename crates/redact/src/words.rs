//! Search & Redact word lists (the viewer's Find Text ▸ Multiple words or phrases): every match of
//! any word or phrase in a list, typed one per line or imported from a text file. Like the
//! patterns, the matcher works on a page's text as characters and returns character ranges.

use std::ops::Range;

/// Most entries in one list.
pub const MAX_WORDS: usize = 1000;
/// Most characters in one entry.
pub const MAX_WORD_CHARS: usize = 256;
/// Largest word-list file read (one entry per line).
pub const MAX_LIST_BYTES: usize = 1 << 20;

/// Words and phrases to find, each with its whitespace collapsed to single spaces (search text has
/// one space between words and lines).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WordList {
    terms: Vec<Vec<char>>,
    case_sensitive: bool,
    whole_words: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WordListError {
    #[error("the list has no words or phrases")]
    Empty,
    #[error("the list has more than {MAX_WORDS} words or phrases")]
    TooMany,
    #[error("\"{0}…\" is longer than {MAX_WORD_CHARS} characters")]
    TooLong(String),
    #[error("the word list is larger than 1 MiB")]
    TooLarge,
}

impl WordList {
    /// The entries of `words`, blank ones skipped and repeats (ignoring case unless
    /// `case_sensitive`) kept once, in order.
    pub fn new<'a>(words: impl IntoIterator<Item = &'a str>, case_sensitive: bool, whole_words: bool) -> Result<Self, WordListError> {
        let mut terms: Vec<Vec<char>> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for w in words {
            let w = w.trim_start_matches('\u{FEFF}');
            let mut term: Vec<char> = Vec::new();
            for part in w.split_whitespace() {
                if !term.is_empty() {
                    term.push(' ');
                }
                term.extend(part.chars());
                if term.len() > MAX_WORD_CHARS {
                    return Err(WordListError::TooLong(w.trim().chars().take(24).collect()));
                }
            }
            if term.is_empty() {
                continue;
            }
            let key: String = if case_sensitive { term.iter().collect() } else { term.iter().collect::<String>().to_lowercase() };
            if !seen.insert(key) {
                continue;
            }
            if terms.len() == MAX_WORDS {
                return Err(WordListError::TooMany);
            }
            terms.push(term);
        }
        if terms.is_empty() {
            return Err(WordListError::Empty);
        }
        Ok(Self { terms, case_sensitive, whole_words })
    }

    /// One entry per line of `text` (a typed list or a UTF-8 text file).
    pub fn parse(text: &str, case_sensitive: bool, whole_words: bool) -> Result<Self, WordListError> {
        if text.len() > MAX_LIST_BYTES {
            return Err(WordListError::TooLarge);
        }
        Self::new(text.lines(), case_sensitive, whole_words)
    }

    /// The entries, as matched.
    pub fn words(&self) -> impl Iterator<Item = String> + '_ {
        self.terms.iter().map(|t| t.iter().collect())
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Character ranges of `text` matching any entry: at each place the longest entry that
    /// matches, and matches never overlap.
    pub fn find(&self, text: &[char]) -> Vec<Range<usize>> {
        let same = |a: char, b: char| a == b || (!self.case_sensitive && a.to_lowercase().eq(b.to_lowercase()));
        let word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
        let mut out = Vec::new();
        let mut i = 0;
        while i < text.len() {
            let starts_word = !self.whole_words || !word(i.checked_sub(1).and_then(|p| text.get(p)));
            let longest = if starts_word {
                self.terms
                    .iter()
                    .filter(|t| {
                        let Some(here) = text.get(i..i.saturating_add(t.len())) else { return false };
                        here.iter().zip(t.iter()).all(|(a, b)| same(*a, *b)) && (!self.whole_words || !word(text.get(i.saturating_add(t.len()))))
                    })
                    .map(Vec::len)
                    .max()
            } else {
                None
            };
            match longest {
                Some(n) if n > 0 => {
                    out.push(i..i + n);
                    i += n;
                }
                _ => i += 1,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn found(list: &WordList, s: &str) -> Vec<String> {
        let c = chars(s);
        list.find(&c).into_iter().map(|r| c[r].iter().collect()).collect()
    }

    #[test]
    fn every_listed_word_and_phrase_is_found() {
        let list = WordList::parse("Acme Corp\n\n  alice  \nproject   falcon\r\nalice\n", false, false).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(found(&list, "ACME corp hired Alice for Project Falcon."), ["ACME corp", "Alice", "Project Falcon"]);
    }

    #[test]
    fn the_longest_entry_wins_and_matches_never_overlap() {
        let list = WordList::new(["Smith", "John Smith", "Smithson"], false, false).unwrap();
        assert_eq!(found(&list, "Smithson met John Smith"), ["Smithson", "John Smith"]);
        assert_eq!(found(&list, "SMITH, Smith"), ["SMITH", "Smith"]);
    }

    #[test]
    fn case_and_whole_word_options() {
        let exact = WordList::new(["Alice"], true, false).unwrap();
        assert_eq!(found(&exact, "alice Alice Alicent"), ["Alice", "Alice"]);
        let whole = WordList::new(["alice"], false, true).unwrap();
        assert_eq!(found(&whole, "Alice, Alicent and malice"), ["Alice"]);
    }

    #[test]
    fn bad_lists_are_refused() {
        assert_eq!(WordList::parse(" \n\n", false, false), Err(WordListError::Empty));
        let many: String = (0..=MAX_WORDS).map(|i| format!("w{i}\n")).collect();
        assert_eq!(WordList::parse(&many, false, false), Err(WordListError::TooMany));
        let long = "x".repeat(MAX_WORD_CHARS + 1);
        assert!(matches!(WordList::new([long.as_str()], false, false), Err(WordListError::TooLong(_))));
        assert_eq!(WordList::parse(&"a".repeat(MAX_LIST_BYTES + 1), false, false), Err(WordListError::TooLarge));
        // A byte-order mark from a text editor isn't part of the first word.
        assert_eq!(WordList::parse("\u{FEFF}secret", false, false).unwrap().words().collect::<Vec<_>>(), ["secret"]);
        assert!(WordList::default().find(&chars("anything")).is_empty());
    }
}
