//! [`SessionTools`]: the tool table for a frontend's own session
//! ([`pdfcraft_engine::tool_host::ToolHost`]), so the desktop app can build and run custom
//! commands without its interface crate linking this one.

use pdfcraft_engine::Session;
use pdfcraft_engine::tool_host::{ToolHost, ToolInfo};
use serde_json::Value;

use crate::{Automation, Content};

/// Runs tools on the session it is handed, confined to `root` when one is given.
#[derive(Clone, Debug, Default)]
pub struct SessionTools {
    pub root: Option<std::path::PathBuf>,
}

impl ToolHost for SessionTools {
    fn call(&self, session: &mut Session, name: &str, args: &Value) -> Result<Value, String> {
        let mut a = match &self.root {
            Some(root) => Automation::new().with_root(root.clone()).map_err(|e| format!("{}: {e}", root.display()))?,
            None => Automation::new(),
        };
        // The tool works on the caller's session, which goes back to the caller whatever happens.
        std::mem::swap(a.session_mut(), session);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.call(name, args)));
        std::mem::swap(a.session_mut(), session);
        match r {
            Ok(Ok(content)) => Ok(content
                .into_iter()
                .find_map(|c| match c {
                    Content::Json(v) => Some(v),
                    Content::Png { .. } => None,
                })
                .unwrap_or(Value::Null)),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(format!("{name} failed unexpectedly; the document was kept")),
        }
    }

    fn document_tools(&self) -> Vec<ToolInfo> {
        crate::custom_commands::wrappable_tools()
            .into_iter()
            .map(|d| ToolInfo { name: d.name.into(), title: d.title.into(), description: d.description.into(), schema: d.input_schema.clone() })
            .collect()
    }
}
