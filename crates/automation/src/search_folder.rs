//! Advanced Search ▸ All PDF documents in a folder (`search_folder`, parity `view.search-folder`).
//! The search itself is [`pdfcraft_engine::folder_search`], shared with the app's Advanced Search
//! panel; this checks the arguments and confines the folder to the root.

use pdfcraft_engine::folder_search::{self, DEFAULT_MAX_FILES, DEFAULT_MAX_RESULTS, LIMITS, Limits, MAX_FILES, MAX_RESULTS, Options};
use serde_json::Value;

use crate::{Args, Automation, Result, ToolError, failed};

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
        if query.chars().count() > folder_search::MAX_QUERY_CHARS {
            return Err(ToolError::InvalidArgs(format!("query is longer than {} characters", folder_search::MAX_QUERY_CHARS)));
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
        let control = folder_search::Control::default();
        folder_search::search(&folder, folder_arg, recursive, self.root.as_deref(), &opts, max_files, max_results, limits, &control).map_err(failed)
    }
}
