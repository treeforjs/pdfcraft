//! Multiple windows: which document tabs have been moved out of the main window, into which
//! window, in what order, and which tab each window shows.
//!
//! The main window ([`WindowId::MAIN`]) always exists and holds every tab that is in no other
//! window, so a frontend keeps its own tab list and asks this model only about the windows
//! torn off it. A tab is one view of a document ([`Tab::key`]); two tabs may show the same
//! document (Window ▸ New window), and both see every edit and undo because the document is
//! shared, only the view is not.
//!
//! Every operation checks its ids and fails with a [`WindowError`] instead of panicking: tabs
//! and windows can vanish between a request and its execution (a closed tab, a closed window,
//! a stale id from an agent). A window left without tabs closes. The number of windows is
//! capped ([`MAX_WINDOWS`]), so a script cannot open windows without bound.
//!
//! The desktop app (`pdfcraft-ui-egui`) and the headless tools (`window_*` in
//! `pdfcraft-automation`) use this same model.

use crate::DocId;

/// The most windows besides the main window. Each visible window shares the application's
/// texture allowance, so this also bounds how thin each share gets.
pub const MAX_WINDOWS: usize = 7;

/// The most tabs one window holds (the main window is the frontend's own list).
pub const MAX_TABS_PER_WINDOW: usize = 256;

/// A window: the main window is 0, the others count up from 1 and are never reused while the
/// application runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct WindowId(pub u32);

impl WindowId {
    pub const MAIN: WindowId = WindowId(0);

    pub fn is_main(self) -> bool {
        self == Self::MAIN
    }
}

/// One tab: a view (`key`, unique in the application) of a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tab {
    pub key: u64,
    pub doc: DocId,
}

/// A window other than the main one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub id: WindowId,
    /// In tab-strip order; never empty.
    pub tabs: Vec<Tab>,
    /// The tab shown (always one of `tabs`).
    pub active: u64,
}

impl Window {
    pub fn active_tab(&self) -> Option<Tab> {
        self.tabs.iter().copied().find(|t| t.key == self.active)
    }
}

/// Why a window operation was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowError {
    /// No window has this id (it may have closed).
    NoSuchWindow(WindowId),
    /// No tab has this key (it may have closed).
    NoSuchTab(u64),
    /// [`MAX_WINDOWS`] windows are open already.
    TooManyWindows,
    /// The window holds [`MAX_TABS_PER_WINDOW`] tabs already.
    TooManyTabs(WindowId),
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WindowError::NoSuchWindow(w) => write!(f, "there is no window {} (it may have closed); window_list shows the open ones", w.0),
            WindowError::NoSuchTab(k) => write!(f, "there is no tab {k} (it may have closed); window_list shows the open ones"),
            WindowError::TooManyWindows => write!(f, "at most {MAX_WINDOWS} windows can be open besides the main window: close one first"),
            WindowError::TooManyTabs(w) => write!(f, "window {} already holds {MAX_TABS_PER_WINDOW} tabs", w.0),
        }
    }
}

impl std::error::Error for WindowError {}

/// The windows besides the main one (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowSet {
    windows: Vec<Window>,
    /// The last id handed out.
    last_id: u32,
}

impl WindowSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// The windows besides the main one, oldest first.
    pub fn windows(&self) -> &[Window] {
        &self.windows
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn get(&self, id: WindowId) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    /// Whether `id` is the main window or an open one.
    pub fn exists(&self, id: WindowId) -> bool {
        id.is_main() || self.get(id).is_some()
    }

    /// The window holding tab `key`: the main window unless it was moved out.
    pub fn window_of(&self, key: u64) -> WindowId {
        self.windows.iter().find(|w| w.tabs.iter().any(|t| t.key == key)).map_or(WindowId::MAIN, |w| w.id)
    }

    /// Whether tab `key` is in a window other than the main one.
    pub fn is_torn_off(&self, key: u64) -> bool {
        !self.window_of(key).is_main()
    }

    /// The tab a window shows; `None` for the main window (the frontend's own) and unknown ids.
    pub fn active(&self, id: WindowId) -> Option<Tab> {
        self.get(id).and_then(Window::active_tab)
    }

    /// Open a new window showing `tab`, taking it out of any window that held it.
    pub fn new_window(&mut self, tab: Tab) -> Result<WindowId, WindowError> {
        let moving_out_of_last = self.windows.iter().any(|w| w.tabs.len() == 1 && w.tabs.first().is_some_and(|t| t.key == tab.key));
        if self.windows.len() >= MAX_WINDOWS && !moving_out_of_last {
            return Err(WindowError::TooManyWindows);
        }
        self.remove_tab(tab.key);
        self.last_id = self.last_id.checked_add(1).ok_or(WindowError::TooManyWindows)?;
        let id = WindowId(self.last_id);
        self.windows.push(Window { id, tabs: vec![tab], active: tab.key });
        Ok(id)
    }

    /// Move `tab` into window `to` (the main window takes it back), after its tabs, and show
    /// it there. A window it leaves empty closes; that window is returned.
    pub fn move_tab(&mut self, tab: Tab, to: WindowId) -> Result<Option<WindowId>, WindowError> {
        if !self.exists(to) {
            return Err(WindowError::NoSuchWindow(to));
        }
        let from = self.window_of(tab.key);
        if from == to {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == to) {
                w.active = tab.key;
            }
            return Ok(None);
        }
        if let Some(w) = self.get(to)
            && w.tabs.len() >= MAX_TABS_PER_WINDOW
        {
            return Err(WindowError::TooManyTabs(to));
        }
        let closed = self.remove_tab(tab.key);
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == to) {
            w.tabs.push(tab);
            w.active = tab.key;
        }
        Ok(closed)
    }

    /// Insert a new tab (a view just opened from a window) after that window's active tab.
    pub fn add_tab(&mut self, tab: Tab, to: WindowId) -> Result<(), WindowError> {
        if to.is_main() {
            return Ok(());
        }
        let w = self.windows.iter_mut().find(|w| w.id == to).ok_or(WindowError::NoSuchWindow(to))?;
        if w.tabs.len() >= MAX_TABS_PER_WINDOW {
            return Err(WindowError::TooManyTabs(to));
        }
        let at = w.tabs.iter().position(|t| t.key == w.active).map_or(w.tabs.len(), |i| i.saturating_add(1));
        w.tabs.insert(at.min(w.tabs.len()), tab);
        w.active = tab.key;
        Ok(())
    }

    /// Show tab `key` in its window.
    pub fn set_active(&mut self, key: u64) -> Result<WindowId, WindowError> {
        if let Some(w) = self.windows.iter_mut().find(|w| w.tabs.iter().any(|t| t.key == key)) {
            w.active = key;
            return Ok(w.id);
        }
        Err(WindowError::NoSuchTab(key))
    }

    /// A tab closed (or moved back to the main window): forget it. When that leaves its window
    /// empty the window closes and is returned; otherwise the window shows the tab beside it.
    pub fn remove_tab(&mut self, key: u64) -> Option<WindowId> {
        let wi = self.windows.iter().position(|w| w.tabs.iter().any(|t| t.key == key))?;
        let w = self.windows.get_mut(wi)?;
        let ti = w.tabs.iter().position(|t| t.key == key)?;
        w.tabs.remove(ti);
        if w.tabs.is_empty() {
            let id = w.id;
            self.windows.remove(wi);
            return Some(id);
        }
        if w.active == key
            && let Some(next) = w.tabs.get(ti.min(w.tabs.len().saturating_sub(1)))
        {
            w.active = next.key;
        }
        None
    }

    /// Close window `id`, returning its tabs (the caller closes them, or keeps their documents
    /// open where another tab still shows them). The main window cannot be closed here.
    pub fn close_window(&mut self, id: WindowId) -> Result<Vec<Tab>, WindowError> {
        let i = self.windows.iter().position(|w| w.id == id).ok_or(WindowError::NoSuchWindow(id))?;
        Ok(self.windows.remove(i).tabs)
    }

    /// Forget tabs that no longer exist (`alive` says which do), closing windows left empty
    /// and fixing their active tabs. Returns the windows that closed.
    pub fn retain(&mut self, mut alive: impl FnMut(&Tab) -> bool) -> Vec<WindowId> {
        let mut closed = Vec::new();
        for w in &mut self.windows {
            w.tabs.retain(&mut alive);
            if w.tabs.is_empty() {
                closed.push(w.id);
            } else if !w.tabs.iter().any(|t| t.key == w.active)
                && let Some(first) = w.tabs.first()
            {
                w.active = first.key;
            }
        }
        self.windows.retain(|w| !w.tabs.is_empty());
        closed
    }

    /// Every tab in a window besides the main one.
    pub fn torn_off_tabs(&self) -> impl Iterator<Item = (WindowId, Tab)> + '_ {
        self.windows.iter().flat_map(|w| w.tabs.iter().map(move |t| (w.id, *t)))
    }
}

/// How many of `tabs` show `doc` (a document is closed with its last tab).
pub fn views_of(tabs: impl IntoIterator<Item = Tab>, doc: DocId) -> usize {
    tabs.into_iter().filter(|t| t.doc == doc).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(key: u64, doc: u64) -> Tab {
        Tab { key, doc: DocId(doc) }
    }

    #[test]
    fn a_tab_moves_out_and_back() {
        let mut s = WindowSet::new();
        assert_eq!(s.window_of(1), WindowId::MAIN);
        let w = s.new_window(tab(1, 10)).unwrap();
        assert_eq!(w, WindowId(1));
        assert_eq!(s.window_of(1), w);
        assert_eq!(s.active(w), Some(tab(1, 10)));
        // A second tab follows; then the first goes back to the main window.
        assert_eq!(s.move_tab(tab(2, 11), w).unwrap(), None);
        assert_eq!(s.get(w).unwrap().tabs, vec![tab(1, 10), tab(2, 11)]);
        assert_eq!(s.active(w), Some(tab(2, 11)));
        assert_eq!(s.move_tab(tab(1, 10), WindowId::MAIN).unwrap(), None);
        assert_eq!(s.window_of(1), WindowId::MAIN);
        // The last tab leaving closes the window.
        assert_eq!(s.move_tab(tab(2, 11), WindowId::MAIN).unwrap(), Some(w));
        assert!(s.is_empty());
        // Ids are not reused.
        assert_eq!(s.new_window(tab(1, 10)).unwrap(), WindowId(2));
    }

    #[test]
    fn moving_between_windows_and_into_a_new_one() {
        let mut s = WindowSet::new();
        let a = s.new_window(tab(1, 10)).unwrap();
        s.move_tab(tab(2, 10), a).unwrap();
        let b = s.new_window(tab(2, 10)).unwrap();
        assert_eq!(s.get(a).unwrap().tabs, vec![tab(1, 10)]);
        assert_eq!(s.active(a), Some(tab(1, 10)), "the window shows the tab beside the one that left");
        assert_eq!(s.move_tab(tab(1, 10), b).unwrap(), Some(a), "window a is left empty and closes");
        assert_eq!(s.windows().len(), 1);
        assert_eq!(s.get(b).unwrap().tabs, vec![tab(2, 10), tab(1, 10)]);
        // Two views of document 10, one window.
        assert_eq!(views_of(s.torn_off_tabs().map(|(_, t)| t), DocId(10)), 2);
        // A tab torn off the only tab of its window into a new window keeps the count.
        let mut one = WindowSet::new();
        for k in 0..MAX_WINDOWS as u64 {
            one.new_window(tab(k, k)).unwrap();
        }
        assert_eq!(one.new_window(tab(99, 1)), Err(WindowError::TooManyWindows));
        assert!(one.new_window(tab(0, 0)).is_ok(), "re-homing a window's only tab does not add a window");
        assert_eq!(one.len(), MAX_WINDOWS);
    }

    #[test]
    fn stale_and_hostile_ids_are_refused() {
        let mut s = WindowSet::new();
        assert_eq!(s.move_tab(tab(1, 1), WindowId(5)), Err(WindowError::NoSuchWindow(WindowId(5))));
        assert_eq!(s.move_tab(tab(1, 1), WindowId(u32::MAX)), Err(WindowError::NoSuchWindow(WindowId(u32::MAX))));
        assert_eq!(s.close_window(WindowId::MAIN), Err(WindowError::NoSuchWindow(WindowId::MAIN)));
        assert_eq!(s.set_active(42), Err(WindowError::NoSuchTab(42)));
        assert_eq!(s.remove_tab(42), None);
        assert_eq!(s.add_tab(tab(1, 1), WindowId(3)), Err(WindowError::NoSuchWindow(WindowId(3))));
        let w = s.new_window(tab(1, 1)).unwrap();
        assert_eq!(s.close_window(w).unwrap(), vec![tab(1, 1)]);
        assert_eq!(s.close_window(w), Err(WindowError::NoSuchWindow(w)), "closing twice");
        assert_eq!(s.move_tab(tab(1, 1), w), Err(WindowError::NoSuchWindow(w)), "moving into a closed window");
        // The id counter cannot overflow into a panic.
        let mut full = WindowSet { windows: Vec::new(), last_id: u32::MAX };
        assert_eq!(full.new_window(tab(1, 1)), Err(WindowError::TooManyWindows));
        assert!(full.is_empty());
        assert!(WindowError::NoSuchTab(3).to_string().contains("window_list"));
    }

    #[test]
    fn closed_tabs_are_forgotten() {
        let mut s = WindowSet::new();
        let a = s.new_window(tab(1, 1)).unwrap();
        s.add_tab(tab(2, 2), a).unwrap();
        s.add_tab(tab(3, 3), a).unwrap();
        // Inserted after the active tab, and shown.
        assert_eq!(s.get(a).unwrap().tabs.iter().map(|t| t.key).collect::<Vec<_>>(), vec![1, 2, 3]);
        s.set_active(1).unwrap();
        s.add_tab(tab(4, 4), a).unwrap();
        assert_eq!(s.get(a).unwrap().tabs.iter().map(|t| t.key).collect::<Vec<_>>(), vec![1, 4, 2, 3]);
        let b = s.new_window(tab(5, 5)).unwrap();
        // Tab 4 (active) and window b's only tab close.
        assert_eq!(s.retain(|t| t.key != 4 && t.key != 5), vec![b]);
        assert_eq!(s.active(a).map(|t| t.key), Some(1));
        assert_eq!(s.remove_tab(1), None);
        assert_eq!(s.active(a).map(|t| t.key), Some(2));
        assert!(s.exists(WindowId::MAIN) && !s.exists(b));
    }

    #[test]
    fn a_window_holds_a_bounded_number_of_tabs() {
        let mut s = WindowSet::new();
        let w = s.new_window(tab(0, 0)).unwrap();
        for k in 1..MAX_TABS_PER_WINDOW as u64 {
            s.add_tab(tab(k, k), w).unwrap();
        }
        assert_eq!(s.add_tab(tab(9999, 1), w), Err(WindowError::TooManyTabs(w)));
        assert_eq!(s.move_tab(tab(9999, 1), w), Err(WindowError::TooManyTabs(w)));
        assert_eq!(s.window_of(9999), WindowId::MAIN);
    }
}
