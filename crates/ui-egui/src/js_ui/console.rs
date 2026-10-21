//! Bound retained console history across document events and repeated user evaluations.
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_LINES: usize = 2_000;
const MAX_LINE: usize = 16 * 1024;
const TRUNCATED: &str = "… [truncated]";

/// The JavaScript console: editable input and a bounded recent output history.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JsConsole {
    pub input: String,
    pub log: Vec<String>,
}

fn boundary(text: &str, limit: usize) -> usize {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

impl JsConsole {
    pub(crate) fn record(&mut self, prefix: &str, text: &str) {
        // Check byte lengths before cloning an untrusted result or error string.
        let prefix_end = boundary(prefix, MAX_LINE - TRUNCATED.len());
        let available = MAX_LINE - prefix_end;
        let truncated = prefix_end != prefix.len() || text.len() > available;
        let text_end = boundary(text, available - if truncated { TRUNCATED.len() } else { 0 });
        let mut line = String::with_capacity(prefix_end + text_end + if truncated { TRUNCATED.len() } else { 0 });
        line.push_str(&prefix[..prefix_end]);
        line.push_str(&text[..text_end]);
        if truncated {
            line.push_str(TRUNCATED);
        }
        self.log.push(line);

        let mut bytes = self.log.iter().fold(0usize, |total, line| total.saturating_add(line.len()));
        let mut remove = self.log.len().saturating_sub(MAX_LINES);
        for line in self.log.iter().take(remove) {
            bytes = bytes.saturating_sub(line.len());
        }
        while bytes > MAX_BYTES {
            let Some(line) = self.log.get(remove) else { break };
            bytes = bytes.saturating_sub(line.len());
            remove += 1;
        }
        self.log.drain(..remove);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_keeps_recent_lines_with_both_aggregate_limits() {
        let mut console = JsConsole::default();
        for i in 0..3_000 {
            console.record("", &i.to_string());
        }
        assert_eq!(console.log.len(), MAX_LINES);
        assert_eq!(console.log.first().map(String::as_str), Some("1000"));
        let large = "x".repeat(MAX_LINE);
        for _ in 0..1_000 {
            console.record("", &large);
        }
        assert!(console.log.len() <= MAX_LINES);
        assert!(console.log.iter().map(String::len).sum::<usize>() <= MAX_BYTES);
        assert_eq!(console.log.last(), Some(&large));
    }

    #[test]
    fn a_large_unicode_error_is_truncated_before_copying() {
        let mut console = JsConsole::default();
        console.record("Error: ", &"文".repeat(MAX_LINE));
        let line = &console.log[0];
        assert!(line.len() <= MAX_LINE);
        assert!(line.starts_with("Error: 文"));
        assert!(line.ends_with(TRUNCATED));
        console.record("", "ready");
        assert_eq!(console.log.last().map(String::as_str), Some("ready"));
    }
}
