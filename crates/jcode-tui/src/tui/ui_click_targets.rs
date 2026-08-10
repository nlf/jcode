//! Hit-testing of screen coordinates against the things the transcript renders
//! as clickable: links, inline images, truncated diffs, collapsed tool rows and
//! swarm badges, plus the hover resolution that has to agree with all of them.
//!
//! This lives outside `ui.rs` because `ui.rs` is the file upstream touches most
//! often, so anything that stays in it is repeatedly dragged through merge
//! conflicts for reasons that have nothing to do with the code itself. The
//! hit-tests are a self-contained family with a narrow dependency on the copy
//! viewport snapshots, so moving them out shrinks that conflict surface without
//! changing what any of them do.

use super::messages;
use super::{
    CopyViewportData, copy_point_from_screen, copy_snapshot_for_pane, line_display_width,
    link_span_from_snapshot, link_target_from_snapshot,
};

pub(crate) fn link_target_from_screen(column: u16, row: u16) -> Option<String> {
    let point = copy_point_from_screen(column, row)?;
    // Clicking a URL you are still composing should reposition the caret, not
    // open the link; only transcript/side-pane links are click-to-open.
    if point.pane == crate::tui::CopySelectionPane::Input {
        return None;
    }
    let snapshot = copy_snapshot_for_pane(point.pane)?;
    link_target_from_snapshot(&snapshot, point)
}

/// If a screen click landed on an inline-image label line, return the image
/// id so the caller can cycle that image's size. The label line is short and
/// single purpose (there is no visible expand badge anymore), so the whole
/// line acts as the click target alongside the image body itself.
pub(crate) fn inline_image_expand_target_from_screen(column: u16, row: u16) -> Option<u64> {
    let point = copy_point_from_screen(column, row)?;
    let snapshot = copy_snapshot_for_pane(point.pane)?;
    snapshot.inline_image_id_for_label_line(point.abs_line)
}

/// If a screen click landed inside an edit tool's rendered diff, return the
/// transcript message index so the caller can show every change instead of the
/// truncated view.
///
/// Only diffs that are actually eliding something report a target
/// (`EditToolRange::expandable`), so clicking a diff already shown in full
/// leaves the click free for selection rather than redrawing the same lines.
pub(crate) fn diff_expand_target_from_screen(column: u16, row: u16) -> Option<usize> {
    let point = copy_point_from_screen(column, row)?;
    if point.pane != crate::tui::CopySelectionPane::Chat {
        return None;
    }
    let snapshot = copy_snapshot_for_pane(point.pane)?;
    let prepared = match &snapshot.data {
        CopyViewportData::ChatFrame { prepared } => prepared.clone(),
        CopyViewportData::Dense { .. } => return None,
    };
    prepared
        .edit_tool_ranges
        .iter()
        .find(|range| {
            range.expandable
                && point.abs_line >= range.start_line
                && point.abs_line < range.end_line
        })
        .map(|range| range.msg_index)
}

/// If a screen click landed on a collapsed tool row, return the transcript
/// message index so the caller can expand that tool's full command and output.
///
/// Unlike the swarm badge (which reserves a trailing token so the tldr text
/// stays selectable), the whole tool row is the target: it is a summary row
/// rather than prose, and its text is already elided, so there is little worth
/// selecting and a lot worth revealing. A plain click with no drag reaches this
/// path; press-and-drag still starts a selection, because `copy_selection`
/// consumes drags before the click handlers run.
pub(crate) fn tool_expand_target_from_screen(
    column: u16,
    row: u16,
    is_tool_message: impl Fn(usize) -> bool,
) -> Option<usize> {
    let point = copy_point_from_screen(column, row)?;
    if point.pane != crate::tui::CopySelectionPane::Chat {
        return None;
    }
    let snapshot = copy_snapshot_for_pane(point.pane)?;
    let prepared = match &snapshot.data {
        CopyViewportData::ChatFrame { prepared } => prepared.clone(),
        CopyViewportData::Dense { .. } => return None,
    };
    let msg_idx = prepared.message_index_at_line(point.abs_line)?;
    is_tool_message(msg_idx).then_some(msg_idx)
}

/// If a screen click landed on a collapsed/expanded swarm notification's
/// `▸ expand` / `▾ collapse` badge, return the transcript message index so the
/// caller can toggle that notification. Only clicks on the trailing badge
/// token count, so the tldr text itself stays selectable.
pub(crate) fn swarm_expand_target_from_screen(column: u16, row: u16) -> Option<usize> {
    let point = copy_point_from_screen(column, row)?;
    if point.pane != crate::tui::CopySelectionPane::Chat {
        return None;
    }
    let snapshot = copy_snapshot_for_pane(point.pane)?;
    let prepared = match &snapshot.data {
        CopyViewportData::ChatFrame { prepared } => prepared.clone(),
        CopyViewportData::Dense { .. } => return None,
    };
    let text = snapshot.wrapped_plain_line(point.abs_line)?;
    let trimmed = text.trim_end();
    let badge_start = [
        messages::SWARM_EXPAND_BADGE,
        messages::SWARM_COLLAPSE_BADGE,
        messages::SWARM_DIFF_EXPAND_BADGE,
        messages::SWARM_DIFF_COLLAPSE_BADGE,
    ]
    .iter()
    .find_map(|badge| {
        let prefix = trimmed.strip_suffix(badge)?;
        Some(line_display_width(prefix))
    })?;
    if point.column < badge_start {
        return None;
    }
    prepared.message_index_at_line(point.abs_line)
}

/// If a screen click landed on the rendered body of an inline image (its
/// placeholder rows), return the image id so the caller can cycle that image's
/// size. Together with the label-line hit-test this makes the whole picture
/// clickable.
/// The hit-region is bounded by the image's rendered width (`region.width`,
/// which includes the 2-cell left border), shifted right when `centered` mode
/// horizontally centers the drawn pixels, so clicks in empty space beside a
/// narrow image stay inert.
pub(crate) fn inline_image_body_target_from_screen(
    column: u16,
    row: u16,
    centered: bool,
) -> Option<u64> {
    let point = copy_point_from_screen(column, row)?;
    let snapshot = copy_snapshot_for_pane(point.pane)?;
    let prepared = match &snapshot.data {
        CopyViewportData::ChatFrame { prepared } => prepared,
        CopyViewportData::Dense { .. } => return None,
    };
    let region = prepared.image_regions.iter().find(|region| {
        region.render == jcode_tui_messages::ImageRegionRender::Fit
            && point.abs_line >= region.abs_line_idx
            && point.abs_line < region.end_line
    })?;
    let area = snapshot.content_area;
    let rel_col = column.saturating_sub(area.x);
    // `width == 0` means unknown; treat the rows as fully occupied then.
    let width = if region.width == 0 {
        area.width
    } else {
        region.width.min(area.width)
    };
    // Centered mode draws the border at the left edge but centers the image
    // pixels; accept the full band from the border through the image's right
    // edge so both the border and the picture are clickable.
    let right_edge = if centered {
        let offset = area.width.saturating_sub(width) / 2;
        offset.saturating_add(width)
    } else {
        width
    };
    (rel_col < right_edge).then_some(region.hash)
}

/// Resolve what is clickable under a screen cell, for hover feedback.
///
/// Deliberately built from the same hit-tests the click handlers use, in the
/// same order the mouse handler tries them, so the highlight can never promise
/// a click the handlers would not honor. Returns the region to brighten.
///
/// `is_tool_message` mirrors the predicate `try_toggle_tool_expand_at` passes,
/// so a tool row that hides nothing (and is therefore inert) does not light up.
pub(crate) fn hover_target_from_screen(
    column: u16,
    row: u16,
    centered: bool,
    is_tool_message: impl Fn(usize) -> bool,
) -> Option<crate::tui::hover::HoverTarget> {
    use crate::tui::hover::{HoverKind, HoverScope, HoverTarget};

    let point = copy_point_from_screen(column, row)?;
    let snapshot = copy_snapshot_for_pane(point.pane)?;
    let area = snapshot.content_area;
    // Rows the highlight may cover, translated back from absolute transcript
    // lines to screen rows through the same scroll offset the hit-test used.
    let row_for_abs = |abs: usize| -> u16 {
        let rel = abs.saturating_sub(snapshot.scroll);
        area.y.saturating_add(rel.min(u16::MAX as usize) as u16)
    };
    // Highlighting the pane's full width would run the lift straight through
    // the side-panel border sitting in the same screen rows. Bound it to the
    // widest line the region actually drew, so the highlight covers the text
    // and nothing beyond it.
    let region_right = |top_abs: usize, bottom_abs: usize| -> u16 {
        let widest = (top_abs..bottom_abs)
            .filter_map(|abs| snapshot.wrapped_plain_line(abs))
            .map(|text| line_display_width(text.trim_end()))
            .max()
            .unwrap_or(0);
        area.x
            .saturating_add((widest as u16).min(area.width))
            .min(area.x.saturating_add(area.width))
    };
    let full_row = |top: u16,
                    bottom: u16,
                    top_abs: usize,
                    bottom_abs: usize,
                    kind: HoverKind,
                    scope: HoverScope| {
        HoverTarget {
            kind,
            top_row: top.max(area.y),
            bottom_row: bottom.min(area.y.saturating_add(area.height)),
            left_col: area.x,
            right_col: region_right(top_abs, bottom_abs),
            scope,
        }
    };

    // Links first: the mouse handler opens a URL before it expands the row
    // around it, so hovering one must advertise the link, not the row. Only
    // the URL's own cells light up: highlighting the whole line would say the
    // line is the target, when clicking anywhere else on it does something
    // else entirely (or nothing).
    if point.pane != crate::tui::CopySelectionPane::Input
        && let Some((_url, start_col, end_col)) = link_span_from_snapshot(&snapshot, point)
    {
        // The span is in wrapped-line display columns; shift it by this row's
        // left margin (centered mode indents the text) to reach screen columns.
        let left_margin = snapshot
            .left_margins
            .get(row.saturating_sub(area.y) as usize)
            .copied()
            .unwrap_or(0);
        let origin = area.x.saturating_add(left_margin);
        let right_limit = area.x.saturating_add(area.width);
        return Some(HoverTarget {
            kind: HoverKind::Link,
            top_row: row,
            bottom_row: row.saturating_add(1),
            left_col: origin.saturating_add(start_col as u16).min(right_limit),
            right_col: origin.saturating_add(end_col as u16).min(right_limit),
            scope: HoverScope::Text,
        });
    }

    if point.pane != crate::tui::CopySelectionPane::Chat {
        return None;
    }

    if let Some(prepared) = match &snapshot.data {
        CopyViewportData::ChatFrame { prepared } => Some(prepared.clone()),
        CopyViewportData::Dense { .. } => None,
    } {
        // Swarm badge: only the trailing token is clickable, so only the
        // trailing token lights up. Reuse the screen hit-test rather than
        // recomputing the badge column.
        if swarm_expand_target_from_screen(column, row).is_some() {
            let badge_left = snapshot
                .wrapped_plain_line(point.abs_line)
                .map(|text| {
                    let trimmed = text.trim_end();
                    [
                        messages::SWARM_EXPAND_BADGE,
                        messages::SWARM_COLLAPSE_BADGE,
                        messages::SWARM_DIFF_EXPAND_BADGE,
                        messages::SWARM_DIFF_COLLAPSE_BADGE,
                    ]
                    .iter()
                    .find_map(|badge| {
                        let prefix = trimmed.strip_suffix(badge)?;
                        Some(line_display_width(prefix))
                    })
                    .unwrap_or(0)
                })
                .unwrap_or(0);
            return Some(HoverTarget {
                kind: HoverKind::SwarmBadge,
                top_row: row,
                bottom_row: row.saturating_add(1),
                left_col: area.x.saturating_add(badge_left as u16),
                right_col: region_right(point.abs_line, point.abs_line + 1),
                scope: HoverScope::Text,
            });
        }

        // A truncated diff highlights its whole framed block, which is the
        // unit that expands.
        if let Some(range) = prepared.edit_tool_ranges.iter().find(|range| {
            range.expandable
                && point.abs_line >= range.start_line
                && point.abs_line < range.end_line
        }) {
            return Some(full_row(
                row_for_abs(range.start_line),
                row_for_abs(range.end_line),
                range.start_line,
                range.end_line,
                HoverKind::Diff,
                HoverScope::Frame,
            ));
        }

        if let Some(msg_idx) = prepared.message_index_at_line(point.abs_line)
            && is_tool_message(msg_idx)
        {
            // Highlight the whole rendered block, as the diff path does: the
            // click toggles the entire message, so lighting up only the line
            // under the pointer would understate what is about to change. An
            // expanded row therefore highlights its summary and its detail
            // frame together, which is exactly what collapses on click.
            let (start_line, end_line) = prepared
                .message_line_range_at_line(point.abs_line)
                .unwrap_or((point.abs_line, point.abs_line + 1));
            // A collapsed row is a single line with no frame to trace, so it
            // lifts its text; once expanded it is a framed block and only the
            // frame lifts.
            let scope = if end_line.saturating_sub(start_line) > 1 {
                HoverScope::Frame
            } else {
                HoverScope::Text
            };
            return Some(full_row(
                row_for_abs(start_line),
                row_for_abs(end_line),
                start_line,
                end_line,
                HoverKind::ToolRow,
                scope,
            ));
        }
    }

    // Inline images: the label line and the picture itself both cycle size.
    if inline_image_expand_target_from_screen(column, row).is_some()
        || inline_image_body_target_from_screen(column, row, centered).is_some()
    {
        return Some(full_row(
            row,
            row.saturating_add(1),
            point.abs_line,
            point.abs_line + 1,
            HoverKind::Image,
            HoverScope::Text,
        ));
    }

    None
}
