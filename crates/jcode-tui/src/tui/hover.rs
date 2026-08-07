//! Hover feedback for clickable transcript regions.
//!
//! The transcript has grown several click targets (tool rows, truncated diffs,
//! reasoning stubs, swarm badges, links, inline images) with nothing to
//! distinguish them from ordinary text. A user cannot tell what is clickable
//! without clicking, which is exactly the guess-and-check the affordance is
//! supposed to remove.
//!
//! This module tracks the cell the pointer is over and what (if anything) is
//! clickable there, so the renderer can brighten that region. It is deliberately
//! a *read* of the same hit-testing the click handlers use, rather than a
//! parallel notion of what is clickable: if the two ever disagreed, the
//! highlight would be a lie.

#[cfg(not(test))]
use std::sync::Mutex;

/// What the pointer is currently over, and which rows the highlight covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HoverTarget {
    /// The kind of click target under the pointer.
    pub kind: HoverKind,
    /// Screen rows the highlight spans, inclusive-exclusive. A tool row is a
    /// single row; a diff covers its whole rendered block.
    pub top_row: u16,
    pub bottom_row: u16,
    /// Screen columns the highlight spans, inclusive-exclusive. Targets that
    /// own a whole line report the full pane width; a trailing badge reports
    /// just its own cells, so the prose beside it stays visually inert.
    pub left_col: u16,
    pub right_col: u16,
    /// Which cells inside the region actually brighten.
    pub scope: HoverScope,
}

/// Which cells of a hovered region receive the lift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoverScope {
    /// Every cell with content. Right for a short region that *is* the target:
    /// a single row, a badge, a URL.
    Text,
    /// Only the box-drawing glyphs framing the region, leaving its content
    /// alone. A multi-line block is mostly the text the user is reading, and
    /// lifting all of it swamps the screen and reads as a selection; tracing
    /// the frame says "this whole block is one clickable thing" without
    /// touching what is being read.
    Frame,
}

/// Box-drawing characters that make up a rendered block's frame.
///
/// Deliberately narrow: these are the glyphs the transcript's own framed
/// blocks draw (`┌─ detail`, `│ …`, `└─`), so a stray box character inside
/// tool output is the only false positive, and lighting one cell of it is
/// harmless.
pub(crate) fn is_frame_glyph(symbol: &str) -> bool {
    matches!(
        symbol,
        "│" | "─" | "┌" | "┐" | "└" | "┘" | "├" | "┤" | "┬" | "┴" | "┼"
            | "╭" | "╮" | "╰" | "╯" | "╷" | "╵"
    )
}

/// Which flavor of clickable region the pointer is over.
///
/// Kept separate from the highlight geometry because the status hint differs:
/// telling the user *what* clicking will do is most of the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoverKind {
    /// A tool summary row that can reveal its command and output.
    ToolRow,
    /// A truncated diff that can show every change.
    Diff,
    /// A swarm notification's expand/collapse badge.
    SwarmBadge,
    /// A URL that opens on click.
    Link,
    /// An inline image whose size cycles on click.
    Image,
}

impl HoverKind {
    /// Short hint describing what a click here does.
    pub(crate) fn hint(self) -> &'static str {
        match self {
            HoverKind::ToolRow => "click to expand",
            HoverKind::Diff => "click to show all changes",
            HoverKind::SwarmBadge => "click to expand",
            HoverKind::Link => "click to open",
            HoverKind::Image => "click to resize",
        }
    }
}

#[cfg(not(test))]
static HOVER: Mutex<Option<HoverTarget>> = Mutex::new(None);

#[cfg(test)]
thread_local! {
    /// Tests run in parallel in one process, so a global hover cell would let
    /// one test observe another's pointer. Mirror the expand-state module and
    /// keep test hover thread-local.
    static TEST_HOVER: std::cell::RefCell<Option<HoverTarget>> =
        const { std::cell::RefCell::new(None) };
}

/// Record the current hover target. Returns `true` when it changed, so the
/// caller can repaint only when the highlight actually moves: pointer motion
/// generates an event per cell, and repainting every one of them would spend
/// a frame on a highlight that did not move.
pub(crate) fn set_hover(target: Option<HoverTarget>) -> bool {
    #[cfg(test)]
    {
        return TEST_HOVER.with(|cell| {
            let mut cell = cell.borrow_mut();
            if *cell == target {
                false
            } else {
                *cell = target;
                true
            }
        });
    }
    #[cfg(not(test))]
    {
        let mut guard = match HOVER.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if *guard == target {
            false
        } else {
            *guard = target;
            true
        }
    }
}

/// The current hover target, if any.
pub(crate) fn hover() -> Option<HoverTarget> {
    #[cfg(test)]
    {
        return TEST_HOVER.with(|cell| *cell.borrow());
    }
    #[cfg(not(test))]
    {
        match HOVER.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

/// Clear any hover highlight. Called when the pointer leaves a clickable
/// region or the transcript re-renders under it.
pub(crate) fn clear_hover() -> bool {
    set_hover(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(kind: HoverKind, row: u16) -> HoverTarget {
        HoverTarget {
            kind,
            top_row: row,
            bottom_row: row + 1,
            left_col: 0,
            right_col: 40,
            scope: HoverScope::Text,
        }
    }

    #[test]
    fn setting_the_same_target_twice_reports_no_change() {
        clear_hover();
        assert!(set_hover(Some(target(HoverKind::ToolRow, 3))));
        assert!(
            !set_hover(Some(target(HoverKind::ToolRow, 3))),
            "an unchanged hover must not ask for a repaint"
        );
        assert!(set_hover(Some(target(HoverKind::ToolRow, 4))));
        assert!(set_hover(None));
        assert!(!set_hover(None));
    }

    #[test]
    fn hover_round_trips() {
        clear_hover();
        assert_eq!(hover(), None);
        set_hover(Some(target(HoverKind::Link, 7)));
        assert_eq!(hover().map(|h| h.kind), Some(HoverKind::Link));
        clear_hover();
        assert_eq!(hover(), None);
    }
}
