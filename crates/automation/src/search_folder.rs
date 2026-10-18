//! Advanced Search ▸ All PDF documents in a folder (`search_folder`, parity `view.search-folder`).
//!
//! Every `.pdf` in a folder (and, with `recursive`, its subfolders) is opened on its own and its
//! pages are searched with the same extraction and matcher as `text_find`
//! ([`PageText::find_opts`]), so a file's text matches are exactly what `text_find` reports for
//! it with the same options. Bookmark titles, comment text and attachment names/descriptions
//! (plus the page text of attached PDFs) are searched on request.
//!
//! Folders are untrusted input: the walk is iterative and depth-limited, links to folders are
//! never followed (so link loops can't recurse), links to files are followed only when they stay
//! inside the root, the number of directory entries examined and of files searched is capped,
//! and each file has a size and a time limit. A file that can't be read, isn't a PDF, needs a
//! password or panics the reader is reported in `skipped` and the search goes on.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pdfcraft_engine::Session;
use pdfcraft_render::{Annotation, Attachment, AttachmentSource, OpenError, OutlineItem, PageText, find_in_chars};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, extract_each, failed, withheld};

/// Glyphs (or characters) of context returned on each side of a match, as in the search panel.
const CONTEXT: usize = 36;
/// `max_files` default and ceiling.
const DEFAULT_MAX_FILES: usize = 1000;
const MAX_FILES: usize = 10_000;
/// `max_results` default and ceiling (matches across all files).
const DEFAULT_MAX_RESULTS: usize = 1000;
const MAX_RESULTS: usize = 100_000;
/// The longest query, in characters.
const MAX_QUERY_CHARS: usize = 1000;
/// Bookmark titles, comment texts and attachment names are searched up to this many characters.
const MAX_STRING_CHARS: usize = 100_000;
/// Bookmarks examined per file, and how deep the bookmark tree is followed.
const MAX_BOOKMARKS: usize = 100_000;
const MAX_BOOKMARK_DEPTH: usize = 64;

/// Bounds on the walk and on each file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// Subfolder levels below `folder` that `recursive` descends.
    pub max_depth: usize,
    /// Directory entries examined in all (files and folders, PDF or not).
    pub max_entries: usize,
    /// Files larger than this are skipped without being read.
    pub max_file_bytes: u64,
    /// Searching one file (with its attachments) stops after this long.
    pub file_time: Duration,
    /// PDF attachments whose pages are searched, per file.
    pub max_attachments: usize,
    /// Attached PDFs larger than this are not searched.
    pub max_attachment_bytes: usize,
}

pub(crate) const LIMITS: Limits = Limits {
    max_depth: 32,
    max_entries: 200_000,
    max_file_bytes: 512 << 20,
    file_time: Duration::from_secs(60),
    max_attachments: 32,
    max_attachment_bytes: 64 << 20,
};

/// The search options of one call.
struct Options<'a> {
    query: &'a str,
    case_sensitive: bool,
    whole_words: bool,
    bookmarks: bool,
    comments: bool,
    attachments: bool,
    password: Option<&'a str>,
}

/// A PDF the walk found: where it really is, and the path reported for it.
struct Found {
    real: PathBuf,
    shown: String,
}

#[derive(Default)]
struct Walk {
    files: Vec<Found>,
    skipped: Vec<Value>,
    truncated: Vec<&'static str>,
}

/// What searching one file gave.
enum Outcome {
    Searched(FileHits),
    Skipped(String),
}

#[derive(Default)]
struct FileHits {
    pages: usize,
    matches: Vec<Value>,
    /// Matches beyond the result budget were dropped.
    dropped: bool,
    /// The document forbids copying, so page-text matches carry no text.
    copy_denied: bool,
    /// Why the file was not searched to the end.
    incomplete: Option<String>,
    /// Attachments that could not be searched, and why.
    notes: Vec<String>,
}

impl FileHits {
    /// Keep `m` if the budget allows.
    fn push(&mut self, budget: usize, m: Value) {
        if self.matches.len() < budget {
            self.matches.push(m);
        } else {
            self.dropped = true;
        }
    }
}

impl Automation {
    pub(crate) fn search_folder(&mut self, a: &Args) -> Result<Value> {
        self.search_folder_with(a, &LIMITS)
    }

    pub(crate) fn search_folder_with(&mut self, a: &Args, limits: &Limits) -> Result<Value> {
        let folder_arg = a.str("folder")?;
        let query = a.str("query")?;
        if query.split_whitespace().next().is_none() {
            return Err(ToolError::InvalidArgs("query must contain something other than spaces".into()));
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(ToolError::InvalidArgs(format!("query is longer than {MAX_QUERY_CHARS} characters")));
        }
        let bound = |key: &str, default: usize, max: usize| -> Result<usize> {
            match a.opt_int(key)? {
                None => Ok(default),
                Some(n) if n >= 1 && n <= max as i64 => Ok(n as usize),
                Some(n) => Err(ToolError::InvalidArgs(format!("{key} must be between 1 and {max}, not {n}"))),
            }
        };
        let max_files = bound("max_files", DEFAULT_MAX_FILES, MAX_FILES)?;
        let max_results = bound("max_results", DEFAULT_MAX_RESULTS, MAX_RESULTS)?;
        let recursive = a.opt_bool("recursive")?.unwrap_or(false);
        let opts = Options {
            query,
            case_sensitive: a.opt_bool("case_sensitive")?.unwrap_or(false),
            whole_words: a.opt_bool("whole_words")?.unwrap_or(false),
            bookmarks: a.opt_bool("include_bookmarks")?.unwrap_or(false),
            comments: a.opt_bool("include_comments")?.unwrap_or(false),
            attachments: a.opt_bool("include_attachments")?.unwrap_or(false),
            password: a.opt_str("password")?,
        };
        // The folder obeys the root like every other path (ocr_recognize_files' rule): outside
        // it gets the one refusal, inside it a missing folder is reported as such.
        let folder = self.resolve(folder_arg, false)?;
        if !folder.is_dir() {
            return Err(failed(format!("{folder_arg} is not a folder")));
        }
        let mut walk = walk(&folder, folder_arg, recursive, self.root.as_deref(), max_files, limits);

        let mut results = Vec::new();
        let (mut searched, mut total) = (0usize, 0usize);
        let mut not_searched = 0usize;
        for (i, f) in walk.files.iter().enumerate() {
            let budget = max_results.saturating_sub(total);
            if budget == 0 {
                not_searched = walk.files.len().saturating_sub(i);
                break;
            }
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| search_file(&f.real, &opts, limits, budget)))
                .unwrap_or_else(|_| Outcome::Skipped("reading it failed unexpectedly".into()));
            let hits = match outcome {
                Outcome::Skipped(reason) => {
                    walk.skipped.push(json!({ "path": f.shown, "reason": reason }));
                    continue;
                }
                Outcome::Searched(h) => h,
            };
            searched = searched.saturating_add(1);
            if hits.dropped && !walk.truncated.contains(&"max_results") {
                walk.truncated.push("max_results");
            }
            if hits.matches.is_empty() && hits.incomplete.is_none() {
                continue;
            }
            total = total.saturating_add(hits.matches.len());
            let mut entry = json!({ "path": f.shown, "pages": hits.pages, "count": hits.matches.len(), "matches": hits.matches });
            if let Some(why) = hits.incomplete {
                entry["incomplete"] = json!(why);
            }
            if !hits.notes.is_empty() {
                entry["notes"] = json!(hits.notes);
            }
            results.push(withheld(entry, !hits.copy_denied));
        }
        if not_searched > 0 && !walk.truncated.contains(&"max_results") {
            walk.truncated.push("max_results");
        }
        let matched = results.iter().filter(|r| r["count"].as_u64().is_some_and(|n| n > 0)).count();
        Ok(json!({
            "query": query,
            "folder": folder_arg,
            "recursive": recursive,
            "options": {
                "whole_words": opts.whole_words, "case_sensitive": opts.case_sensitive,
                "include_bookmarks": opts.bookmarks, "include_comments": opts.comments, "include_attachments": opts.attachments,
            },
            "files_found": walk.files.len(),
            "files_searched": searched,
            "files_matched": matched,
            "files_not_searched": not_searched,
            "total_matches": total,
            "results": results,
            "skipped": walk.skipped,
            "truncated": !walk.truncated.is_empty(),
            "truncated_by": walk.truncated,
        }))
    }
}

fn is_pdf(name: &std::ffi::OsStr) -> bool {
    Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// The PDFs under `start`, in name order, each folder's files before its subfolders. Never
/// follows a link to a folder; follows a link to a file only when it stays inside `root`.
fn walk(start: &Path, shown_start: &str, recursive: bool, root: Option<&Path>, max_files: usize, limits: &Limits) -> Walk {
    let mut out = Walk::default();
    let mut stack: Vec<(PathBuf, PathBuf, usize)> = vec![(start.to_path_buf(), PathBuf::from(shown_start), 0)];
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    let mut entries = 0usize;
    let skip = |out: &mut Walk, path: &Path, reason: String| out.skipped.push(json!({ "path": path.to_string_lossy(), "reason": reason }));
    'walk: while let Some((dir, shown, depth)) = stack.pop() {
        // Folders are reached only through real directories, so this is a second guard.
        if let Ok(real) = dir.canonicalize()
            && !seen_dirs.insert(real)
        {
            continue;
        }
        let list = match std::fs::read_dir(&dir) {
            Ok(list) => list,
            Err(e) => {
                skip(&mut out, &shown, format!("the folder could not be read: {e}"));
                continue;
            }
        };
        let mut items = Vec::new();
        let mut full = false;
        for e in list {
            entries = entries.saturating_add(1);
            if entries > limits.max_entries {
                full = true;
                break;
            }
            match e {
                Ok(e) => items.push(e),
                Err(e) => skip(&mut out, &shown, format!("an entry of the folder could not be read: {e}")),
            }
        }
        items.sort_by_key(|e| e.file_name());
        let mut subdirs = Vec::new();
        for e in items {
            let name = e.file_name();
            let (path, shown_path) = (e.path(), shown.join(&name));
            let Ok(kind) = e.file_type() else {
                skip(&mut out, &shown_path, "its type could not be read".into());
                continue;
            };
            if kind.is_dir() {
                if recursive {
                    if depth >= limits.max_depth {
                        skip(&mut out, &shown_path, format!("more than {} folders deep: not searched", limits.max_depth));
                    } else {
                        subdirs.push((path, shown_path, depth.saturating_add(1)));
                    }
                }
                continue;
            }
            if kind.is_symlink() {
                match std::fs::metadata(&path) {
                    Ok(m) if m.is_dir() => {
                        if recursive {
                            skip(&mut out, &shown_path, "a link to a folder: links to folders are not followed".into());
                        }
                        continue;
                    }
                    Ok(m) if m.is_file() && is_pdf(&name) => {}
                    Ok(_) => continue,
                    Err(_) => {
                        if is_pdf(&name) {
                            skip(&mut out, &shown_path, "a link to a file that does not exist".into());
                        }
                        continue;
                    }
                }
            } else if !kind.is_file() || !is_pdf(&name) {
                continue;
            }
            let real = match path.canonicalize() {
                Ok(real) => real,
                Err(e) => {
                    skip(&mut out, &shown_path, format!("it could not be read: {e}"));
                    continue;
                }
            };
            if let Some(root) = root
                && !real.starts_with(root)
            {
                skip(&mut out, &shown_path, format!("a link to outside the allowed directory {}: not followed", root.display()));
                continue;
            }
            if !seen_files.insert(real.clone()) {
                continue; // the same file again, through a link
            }
            if out.files.len() >= max_files {
                out.truncated.push("max_files");
                break 'walk;
            }
            out.files.push(Found { real, shown: shown_path.to_string_lossy().into_owned() });
        }
        stack.extend(subdirs.into_iter().rev());
        if full {
            out.truncated.push("max_entries");
            break;
        }
    }
    out
}

/// Read `path` if it is small enough.
fn read_capped(path: &Path, max: u64) -> std::result::Result<Vec<u8>, String> {
    let too_big = || format!("larger than the {} MB limit", max >> 20);
    let meta = std::fs::metadata(path).map_err(|e| format!("it could not be read: {e}"))?;
    if meta.len() > max {
        return Err(too_big());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(max.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(|e| format!("it could not be read: {e}"))?;
    if bytes.len() as u64 > max {
        return Err(too_big()); // it grew since
    }
    Ok(bytes)
}

fn open_error(e: &OpenError, password_given: bool) -> String {
    match e {
        OpenError::NeedsPassword => "it is protected by a password: pass password to search it".into(),
        OpenError::WrongPassword if password_given => "it is protected by a password, and the password given does not open it".into(),
        OpenError::WrongPassword => "it is protected by a password: pass password to search it".into(),
        other => other.to_string(),
    }
}

/// An opened document's parts the search needs.
struct Opened {
    bytes: Arc<Vec<u8>>,
    password: Option<String>,
    pages: usize,
    copy: bool,
    outline: Vec<OutlineItem>,
    annotations: Vec<Annotation>,
    attachments: Vec<Attachment>,
}

/// Open `bytes` in a private session with scripts off (searching never runs a document's
/// JavaScript), keeping only what the search reads.
fn open(name: &str, bytes: Vec<u8>, password: Option<&str>) -> std::result::Result<Opened, String> {
    let mut session = Session::new();
    session.set_javascript(false);
    let id = session.open(name, None, Arc::new(bytes), password).map_err(|e| open_error(&e, password.is_some()))?;
    let doc = session.get(id).ok_or("the document vanished")?;
    Ok(Opened {
        bytes: doc.bytes.clone(),
        password: doc.password.clone(),
        pages: doc.info.pages.len(),
        copy: doc.allows_copying(),
        outline: doc.info.outline.clone(),
        annotations: doc.info.annotations.clone(),
        attachments: doc.info.attachments.clone(),
    })
}

fn search_file(path: &Path, opts: &Options, limits: &Limits, budget: usize) -> Outcome {
    let deadline = Instant::now().checked_add(limits.file_time);
    let bytes = match read_capped(path, limits.max_file_bytes) {
        Ok(b) => b,
        Err(e) => return Outcome::Skipped(e),
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let doc = match open(&name, bytes, opts.password) {
        Ok(d) => d,
        Err(e) => return Outcome::Skipped(e),
    };
    let mut hits = FileHits { pages: doc.pages, copy_denied: !doc.copy, ..Default::default() };
    let (text, dropped, incomplete) = page_matches(&doc, opts, limits, deadline, budget, |p, m| {
        m["page"] = json!(p + 1);
        m["found_in"] = json!("text");
    });
    hits.matches = text;
    hits.dropped = dropped;
    hits.incomplete = incomplete;
    if opts.bookmarks {
        bookmark_matches(&doc.outline, opts, budget, &mut hits);
    }
    if opts.comments {
        for c in doc.annotations.iter().filter(|c| !matches!(c.subtype.as_str(), "Link" | "Widget" | "Popup")) {
            let Some(contents) = c.contents.as_deref() else { continue };
            for (text, before, after) in string_matches(contents, opts) {
                let m = json!({ "page": c.page.saturating_add(1), "found_in": "comment", "subtype": c.subtype, "author": c.author, "text": text, "before": before, "after": after });
                hits.push(budget, m);
            }
        }
    }
    if opts.attachments {
        attachment_matches(&doc, opts, limits, deadline, budget, &mut hits);
    }
    Outcome::Searched(hits)
}

/// Page-text matches of `doc` in page order, at most `budget`: whether more were dropped, and
/// why the search stopped early, if it did. `tag` labels each match with its page.
fn page_matches(
    doc: &Opened,
    opts: &Options,
    limits: &Limits,
    deadline: Option<Instant>,
    budget: usize,
    tag: impl Fn(usize, &mut Value),
) -> (Vec<Value>, bool, Option<String>) {
    let pages: Vec<usize> = (0..doc.pages).collect();
    // Pages arrive out of order: keep the first `budget` matches in page order (text_find's rule).
    let mut found: BTreeMap<usize, Vec<Value>> = BTreeMap::new();
    let (mut kept, mut dropped, mut late, mut searched) = (0usize, false, false, 0usize);
    let r = extract_each(&doc.bytes, doc.password.as_deref().map(Arc::from), &pages, |p, text| {
        if deadline.is_some_and(|d| Instant::now() > d) {
            late = true;
            return Err(failed("time limit"));
        }
        searched = searched.saturating_add(1);
        let mut page = Vec::new();
        for r in text.find_opts(opts.query, opts.case_sensitive, opts.whole_words) {
            if page.len() >= budget {
                dropped = true;
                break;
            }
            let mut m = text_match(&text, r, doc.copy);
            tag(p, &mut m);
            page.push(m);
        }
        kept = kept.saturating_add(page.len());
        if !page.is_empty() {
            found.insert(p, page);
        }
        while kept > budget {
            let Some(mut last) = found.last_entry() else { break };
            let excess = kept.saturating_sub(budget).min(last.get().len());
            let keep = last.get().len().saturating_sub(excess);
            last.get_mut().truncate(keep);
            kept = kept.saturating_sub(excess);
            dropped = true;
            if keep == 0 {
                last.remove();
            }
        }
        Ok(())
    });
    let incomplete = match r {
        Ok(()) => None,
        Err(_) if late => Some(format!("stopped after {} s: {searched} of {} pages searched", limits.file_time.as_secs(), doc.pages)),
        Err(e) => Some(format!("{e}: {searched} of {} pages searched", doc.pages)),
    };
    (found.into_values().flatten().collect(), dropped, incomplete)
}

/// One page-text match as `text_find` reports it (`text`, `rects`), with context.
fn text_match(text: &PageText, r: Range<usize>, copy: bool) -> Value {
    let n = text.glyphs.len();
    let (start, end) = (r.start.min(n), r.end.min(n));
    let flat = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let before = copy.then(|| flat(text.text_of(start.saturating_sub(CONTEXT)..start)));
    let after = copy.then(|| flat(text.text_of(end..end.saturating_add(CONTEXT).min(n))));
    json!({ "text": copy.then(|| text.text_of(r.clone())), "before": before, "after": after, "rects": text.line_rects(r) })
}

/// Matches of the query in a plain string (a title, a comment, a name), searched as page text
/// is: whitespace runs are one space and each character is case-folded unless case-sensitive.
/// Returns (match, before, after) in the string's own characters.
fn string_matches(s: &str, opts: &Options) -> Vec<(String, String, String)> {
    let orig: Vec<char> = s.chars().take(MAX_STRING_CHARS).collect();
    let (mut chars, mut owner) = (Vec::with_capacity(orig.len()), Vec::with_capacity(orig.len()));
    for (i, c) in orig.iter().enumerate() {
        if c.is_whitespace() {
            if chars.last().is_some_and(|l| *l != ' ') {
                chars.push(' ');
                owner.push(i);
            }
        } else if opts.case_sensitive {
            chars.push(*c);
            owner.push(i);
        } else {
            for l in c.to_lowercase() {
                chars.push(l);
                owner.push(i);
            }
        }
    }
    let flat =
        |r: Range<usize>| orig.get(r).map(|c| c.iter().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
    find_in_chars(&chars, opts.query, opts.case_sensitive, opts.whole_words)
        .into_iter()
        .filter_map(|r| {
            let a = *owner.get(r.start)?;
            let b = owner.get(r.end.checked_sub(1)?)?.checked_add(1)?;
            Some((flat(a..b), flat(a.saturating_sub(CONTEXT)..a), flat(b..b.saturating_add(CONTEXT).min(orig.len()))))
        })
        .collect()
}

/// Bookmark titles, depth-first in outline order.
fn bookmark_matches(outline: &[OutlineItem], opts: &Options, budget: usize, hits: &mut FileHits) {
    let mut stack: Vec<(&OutlineItem, Vec<usize>)> = outline.iter().enumerate().rev().map(|(i, b)| (b, vec![i.saturating_add(1)])).collect();
    let mut seen = 0usize;
    while let Some((b, path)) = stack.pop() {
        seen = seen.saturating_add(1);
        if seen > MAX_BOOKMARKS {
            hits.notes.push(format!("only the first {MAX_BOOKMARKS} bookmarks were searched"));
            break;
        }
        for (text, before, after) in string_matches(&b.title, opts) {
            let page = b.page.map(|p| p.saturating_add(1));
            hits.push(budget, json!({ "page": page, "found_in": "bookmark", "bookmark": path, "text": text, "before": before, "after": after }));
        }
        if path.len() < MAX_BOOKMARK_DEPTH {
            for (i, c) in b.children.iter().enumerate().rev() {
                let mut p = path.clone();
                p.push(i.saturating_add(1));
                stack.push((c, p));
            }
        }
    }
}

/// Attachment names and descriptions, and the page text of attached PDFs (one level deep).
fn attachment_matches(doc: &Opened, opts: &Options, limits: &Limits, deadline: Option<Instant>, budget: usize, hits: &mut FileHits) {
    let mut pdfs = 0usize;
    for att in &doc.attachments {
        let page = match att.source {
            AttachmentSource::Annotation { page, .. } => Some(page.saturating_add(1)),
            AttachmentSource::Document { .. } => None,
        };
        let fields = [("name", Some(att.name.as_str())), ("description", att.description.as_deref())];
        for (field, value) in fields {
            let Some(value) = value else { continue };
            for (text, before, after) in string_matches(value, opts) {
                let m = json!({ "page": page, "found_in": "attachment", "attachment": att.name, "field": field, "text": text, "before": before, "after": after });
                hits.push(budget, m);
            }
        }
        if !is_pdf(std::ffi::OsStr::new(&att.name)) {
            continue;
        }
        if hits.matches.len() >= budget {
            hits.dropped = true;
            continue;
        }
        if deadline.is_some_and(|d| Instant::now() > d) {
            hits.notes.push(format!("{}: not searched, the file's time limit ran out", att.name));
            continue;
        }
        pdfs = pdfs.saturating_add(1);
        if pdfs > limits.max_attachments {
            hits.notes.push(format!("{}: not searched, only {} attached PDFs are searched per file", att.name, limits.max_attachments));
            continue;
        }
        if att.size.is_some_and(|s| s > limits.max_attachment_bytes) {
            hits.notes.push(format!("{}: not searched, larger than {} MB", att.name, limits.max_attachment_bytes >> 20));
            continue;
        }
        let data = match pdfcraft_render::attachment_data(&doc.bytes, doc.password.as_deref(), att) {
            Ok(d) if d.len() > limits.max_attachment_bytes => {
                hits.notes.push(format!("{}: not searched, larger than {} MB", att.name, limits.max_attachment_bytes >> 20));
                continue;
            }
            Ok(d) => d,
            Err(e) => {
                hits.notes.push(format!("{}: could not be read: {e}", att.name));
                continue;
            }
        };
        let inner = match open(&att.name, data, None) {
            Ok(d) => d,
            Err(e) => {
                hits.notes.push(format!("{}: not searched: {e}", att.name));
                continue;
            }
        };
        let room = budget.saturating_sub(hits.matches.len());
        let (found, dropped, incomplete) = page_matches(&inner, opts, limits, deadline, room, |p, m| {
            m["page"] = json!(p + 1);
            m["found_in"] = json!("attachment_text");
            m["attachment"] = json!(att.name);
            if !inner.copy {
                m["text_withheld"] = json!(true);
            }
        });
        hits.matches.extend(found);
        hits.dropped |= dropped;
        if let Some(why) = incomplete {
            hits.notes.push(format!("{}: {why}", att.name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(query: &str) -> Options<'_> {
        Options { query, case_sensitive: false, whole_words: false, bookmarks: false, comments: false, attachments: false, password: None }
    }

    #[test]
    fn strings_match_like_page_text() {
        let o = opts("big  CAT");
        let m = string_matches("A big\n cat and a BIG cat", &o);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].0, "big cat");
        assert_eq!(m[0].1, "A");
        assert_eq!(m[1].0, "BIG cat");
        let whole = Options { whole_words: true, ..opts("cat") };
        assert_eq!(string_matches("cats, cat.", &whole).len(), 1);
        let case = Options { case_sensitive: true, ..opts("Cat") };
        assert_eq!(string_matches("cat Cat", &case).len(), 1);
        // Odd input: empty strings, only spaces, case folding that changes length.
        assert!(string_matches("", &o).is_empty());
        assert!(string_matches("   ", &opts("x")).is_empty());
        assert_eq!(string_matches("İstanbul", &opts("i̇stanbul")).len(), 1);
    }

    /// The size and time caps skip or cut short a file instead of reading or searching on.
    #[test]
    fn size_and_time_caps_apply() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-search-folder-caps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let blank = Session::new().create_blank(200.0, 200.0, 3).unwrap();
        std::fs::write(dir.join("a.pdf"), blank.as_slice()).unwrap();
        let small = Limits { max_file_bytes: 10, ..LIMITS };
        let Outcome::Skipped(why) = search_file(&dir.join("a.pdf"), &opts("x"), &small, 10) else { panic!("searched a file over the cap") };
        assert!(why.contains("larger than"), "{why}");
        let instant = Limits { file_time: Duration::ZERO, ..LIMITS };
        let Outcome::Searched(h) = search_file(&dir.join("a.pdf"), &opts("x"), &instant, 10) else { panic!("skipped") };
        assert!(h.incomplete.as_deref().is_some_and(|w| w.contains("stopped after")), "{:?}", h.incomplete);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn walk_caps_depth_and_entries() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-search-folder-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b/c")).unwrap();
        for p in ["x.pdf", "a/x.pdf", "a/b/x.pdf", "a/b/c/x.pdf"] {
            std::fs::write(dir.join(p), b"%PDF").unwrap();
        }
        let shallow = Limits { max_depth: 1, ..LIMITS };
        let w = walk(&dir, "d", true, None, 100, &shallow);
        assert_eq!(w.files.len(), 2, "x.pdf and a/x.pdf");
        assert!(w.skipped.iter().any(|s| s["reason"].as_str().unwrap().contains("folders deep")), "{:?}", w.skipped);
        let few = Limits { max_entries: 2, ..LIMITS };
        let w = walk(&dir, "d", true, None, 100, &few);
        assert_eq!(w.truncated, vec!["max_entries"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
