//! Which collapsed transcript regions the user has clicked open.
//!
//! Collapsed rows (tool summaries, diffs, hidden reasoning) are rendered deep
//! inside `ui_messages`, by functions that are widely called and take no
//! app-state parameter. Rather than thread an `&ExpandState` through every one
//! of them, expansion lives in process state and is read at render time, the
//! same way `show_tool_call_details` already reads its config flag.
//!
//! Expansions are ephemeral by design: they are a reading gesture, not a
//! session property, so nothing is persisted and a reload starts collapsed.

use std::collections::HashSet;
#[cfg(not(test))]
use std::sync::{Mutex, OnceLock};

/// Which collapsed thing a click wants to reveal.
///
/// A single transcript row can own more than one collapsible region (a tool row
/// carries its command and output, and an edit tool also carries a diff), so
/// expansion is keyed by `(message index, kind)` rather than by message alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ExpandKind {
    /// The tool row's full command/args and its untruncated output.
    ToolDetail,
    /// An edit tool's diff body.
    Diff,
    /// A reasoning/thinking block hidden by `display.reasoning_display`.
    Reasoning,
}

#[cfg(not(test))]
static EXPANDED_REGIONS: OnceLock<Mutex<HashSet<(usize, ExpandKind)>>> = OnceLock::new();

#[cfg(not(test))]
fn expanded_regions() -> &'static Mutex<HashSet<(usize, ExpandKind)>> {
    EXPANDED_REGIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

#[cfg(test)]
thread_local! {
    static TEST_EXPANDED_REGIONS: std::cell::RefCell<HashSet<(usize, ExpandKind)>> =
        std::cell::RefCell::new(HashSet::new());
}

/// Whether message `msg_idx`'s `kind` region is currently expanded.
pub(crate) fn is_expanded(msg_idx: usize, kind: ExpandKind) -> bool {
    #[cfg(test)]
    {
        TEST_EXPANDED_REGIONS.with(|set| set.borrow().contains(&(msg_idx, kind)))
    }
    #[cfg(not(test))]
    {
        let guard = match expanded_regions().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.contains(&(msg_idx, kind))
    }
}

/// Flip message `msg_idx`'s `kind` region. Returns the state it ended up in.
pub(crate) fn toggle_expanded(msg_idx: usize, kind: ExpandKind) -> bool {
    #[cfg(test)]
    {
        TEST_EXPANDED_REGIONS.with(|set| {
            let mut set = set.borrow_mut();
            if set.remove(&(msg_idx, kind)) {
                false
            } else {
                set.insert((msg_idx, kind));
                true
            }
        })
    }
    #[cfg(not(test))]
    {
        let mut guard = match expanded_regions().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.remove(&(msg_idx, kind)) {
            false
        } else {
            guard.insert((msg_idx, kind));
            true
        }
    }
}

/// Drop every expansion.
///
/// Message indices are positional, so a cleared or replaced transcript would
/// otherwise reopen whatever unrelated rows happen to land on the same indices.
pub(crate) fn clear_expanded_regions() {
    #[cfg(test)]
    {
        TEST_EXPANDED_REGIONS.with(|set| set.borrow_mut().clear());
    }
    #[cfg(not(test))]
    {
        let mut guard = match expanded_regions().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_start_collapsed() {
        clear_expanded_regions();
        assert!(!is_expanded(0, ExpandKind::ToolDetail));
    }

    #[test]
    fn toggle_flips_and_reports_the_resulting_state() {
        clear_expanded_regions();
        assert!(toggle_expanded(3, ExpandKind::ToolDetail));
        assert!(is_expanded(3, ExpandKind::ToolDetail));
        assert!(!toggle_expanded(3, ExpandKind::ToolDetail));
        assert!(!is_expanded(3, ExpandKind::ToolDetail));
    }

    /// The whole point of keying by `(index, kind)`: one row's tool output and
    /// its diff expand independently, and expanding one message must not
    /// expand its neighbours. The previous single-global-line badge could not
    /// express either of these.
    #[test]
    fn regions_are_independent_per_message_and_kind() {
        clear_expanded_regions();
        toggle_expanded(3, ExpandKind::ToolDetail);

        assert!(!is_expanded(3, ExpandKind::Diff), "same row, other kind");
        assert!(!is_expanded(4, ExpandKind::ToolDetail), "other row");

        toggle_expanded(3, ExpandKind::Diff);
        toggle_expanded(4, ExpandKind::ToolDetail);
        assert!(is_expanded(3, ExpandKind::ToolDetail));
        assert!(is_expanded(3, ExpandKind::Diff));
        assert!(is_expanded(4, ExpandKind::ToolDetail));

        // Collapsing one leaves the others alone.
        toggle_expanded(3, ExpandKind::Diff);
        assert!(is_expanded(3, ExpandKind::ToolDetail));
        assert!(!is_expanded(3, ExpandKind::Diff));
        assert!(is_expanded(4, ExpandKind::ToolDetail));
    }

    #[test]
    fn clearing_collapses_everything() {
        clear_expanded_regions();
        toggle_expanded(1, ExpandKind::ToolDetail);
        toggle_expanded(2, ExpandKind::Reasoning);
        clear_expanded_regions();
        assert!(!is_expanded(1, ExpandKind::ToolDetail));
        assert!(!is_expanded(2, ExpandKind::Reasoning));
    }
}
