use slint::ComponentHandle;
use std::cell::RefCell;

/// The open windows of one Unit session, for broadcasting shared scalars.
pub struct Windows<W: ComponentHandle>(RefCell<Vec<slint::Weak<W>>>);

impl<W: ComponentHandle> Default for Windows<W> {
    fn default() -> Self {
        Windows(RefCell::default())
    }
}

impl<W: ComponentHandle> Windows<W> {
    pub fn add(&self, w: &W) {
        self.0.borrow_mut().push(w.as_weak());
    }

    pub fn each(&self, f: impl Fn(&W)) {
        self.0.borrow_mut().retain(|w| w.upgrade().is_some());
        let live: Vec<W> = self.0.borrow().iter().filter_map(|w| w.upgrade()).collect();
        for w in &live {
            f(w);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.borrow().iter().all(|w| w.upgrade().is_none())
    }
}

/// Wire a Slint callback to a method on a weakly held app:
/// `on!(state, app, on_select, |a, id| a.select(&id))`.
#[macro_export]
macro_rules! on {
    ($global:expr, $weak:expr, $setter:ident, |$a:ident $(, $arg:ident)*| $body:expr) => {{
        let me = $weak.clone();
        $global.$setter(move |$($arg),*| {
            if let Some($a) = me.upgrade() {
                $body;
            }
        });
    }};
}
