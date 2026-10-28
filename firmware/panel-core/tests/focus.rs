//! T6d: the focus table, and why it exists.
//!
//! `Node.focus` (an explicit 1-based index) used to be parsed and then ignored:
//! every focus query numbered the focusable nodes by draw order. A daemon that
//! numbers its buttons 3,1,2 would highlight one button and report another --
//! exactly the failure SPEC 3.6.2 / I-2 is written against.
//!
//! These tests pin both halves of the rule:
//!   * an explicit index is a promise from the daemon and is never moved
//!   * anything without one takes the lowest free slot, in draw order
//!
//! They also pin the size bound: slot i is wire index i, so MAX_FOCUS slots is
//! the whole addressable space, and an index past it must not silently wrap.

use panel_core::ui::{Layout, MAX_FOCUS};

fn ui(nodes: &str) -> Layout {
    let line = format!("{{\"t\":\"ui\",\"scene\":0,\"ver\":1,\"nodes\":[{}]}}", nodes);
    Layout::parse(line.as_bytes()).expect("ui must parse")
}

fn rect(x: i16, y: i16, w: u16, h: u16, extra: &str) -> String {
    // parse_node refuses a rect that has neither `fill` nor `color`, so the
    // fixture supplies a fill; the point of these tests is the focus table.
    format!(
        "{{\"type\":\"rect\",\"x\":{},\"y\":{},\"w\":{},\"h\":{},\"fill\":\"#FFFFFF\"{}}}",
        x, y, w, h, extra
    )
}

fn plain(x: i16, y: i16, w: u16, h: u16) -> String {
    rect(x, y, w, h, "")
}

fn button(x: i16, y: i16, w: u16, h: u16) -> String {
    rect(x, y, w, h, ",\"focusable\":true")
}

fn button_at(x: i16, y: i16, w: u16, h: u16, index: u8) -> String {
    rect(x, y, w, h, &format!(",\"focusable\":true,\"focus\":{}", index))
}

#[test]
fn draw_order_when_nobody_assigns_an_index() {
    let layout = ui(&format!(
        "{}, {}, {}",
        button(0, 0, 10, 10),
        button(20, 0, 10, 10),
        button(40, 0, 10, 10)
    ));
    assert_eq!(layout.focus_len(), 3, "three focusable nodes, three slots");
    assert_eq!(layout.focus_index_at(1), Some(1));
    assert_eq!(layout.focus_index_at(2), Some(2));
    assert_eq!(layout.focus_index_at(3), Some(3));
    assert_eq!(layout.focus_rect_at(2), Some((20, 0, 10, 10)));
}

#[test]
fn explicit_index_wins_over_draw_order() {
    // Draw order says 1,2,3; the daemon says 3,1,2.
    let layout = ui(&format!(
        "{}, {}, {}",
        button_at(0, 0, 10, 10, 3),
        button_at(20, 0, 10, 10, 1),
        button_at(40, 0, 10, 10, 2)
    ));
    assert_eq!(layout.focus_len(), 3);
    // walking in index order must follow the daemon's numbering, not the paint order
    assert_eq!(layout.focus_index_at(1), Some(1));
    assert_eq!(layout.focus_rect_at(1), Some((20, 0, 10, 10)), "index 1 is the middle rect");
    assert_eq!(layout.focus_rect_at(2), Some((40, 0, 10, 10)), "index 2 is the last rect");
    assert_eq!(layout.focus_rect_at(3), Some((0, 0, 10, 10)), "index 3 is the first rect");
}

#[test]
fn an_unnumbered_node_takes_the_lowest_free_slot() {
    let layout = ui(&format!(
        "{}, {}, {}",
        button_at(0, 0, 10, 10, 2),
        button(20, 0, 10, 10),
        button(40, 0, 10, 10)
    ));
    assert_eq!(layout.focus_len(), 3);
    assert_eq!(layout.focus_rect_at(2), Some((0, 0, 10, 10)), "the explicit one kept slot 2");
    assert_eq!(layout.focus_rect_at(1), Some((20, 0, 10, 10)), "first unnumbered took slot 1");
    assert_eq!(layout.focus_rect_at(3), Some((40, 0, 10, 10)), "second unnumbered took slot 3");
}

#[test]
fn a_duplicate_index_does_not_overwrite_the_first_claim() {
    let layout = ui(&format!(
        "{}, {}",
        button_at(0, 0, 10, 10, 1),
        button_at(20, 0, 10, 10, 1)
    ));
    assert_eq!(layout.focus_len(), 2, "both nodes still selectable");
    assert_eq!(layout.focus_rect_at(1), Some((0, 0, 10, 10)), "first claim holds slot 1");
    assert_eq!(layout.focus_rect_at(2), Some((20, 0, 10, 10)), "the loser falls back to slot 2");
}

#[test]
fn an_out_of_range_index_falls_back_instead_of_wrapping() {
    let layout = ui(&format!(
        "{}, {}",
        button_at(0, 0, 10, 10, 200),
        button(20, 0, 10, 10)
    ));
    assert_eq!(layout.focus_len(), 2);
    assert_eq!(layout.focus_rect_at(1), Some((0, 0, 10, 10)), "index 200 is unusable, so slot 1");
    assert_eq!(layout.focus_rect_at(2), Some((20, 0, 10, 10)));
    assert_eq!(layout.focus_rect_at(200), None, "and 200 is still not addressable");
}

#[test]
fn plain_nodes_do_not_consume_slots() {
    let layout = ui(&format!(
        "{}, {}, {}",
        plain(0, 0, 10, 10),
        button(20, 0, 10, 10),
        plain(40, 0, 10, 10)
    ));
    assert_eq!(layout.focus_len(), 1);
    assert_eq!(layout.focus_index_at(1), Some(1), "the only selectable one is number 1");
    assert_eq!(layout.focus_index_at(2), None);
}

#[test]
fn the_table_ends_at_max_focus() {
    let layout = ui(&format!(
        "{}, {}",
        button_at(0, 0, 10, 10, MAX_FOCUS as u8),
        button(20, 0, 10, 10)
    ));
    assert_eq!(layout.focus_len(), 2);
    assert_eq!(
        layout.focus_rect_at(MAX_FOCUS as u8),
        Some((0, 0, 10, 10)),
        "the last addressable index still works"
    );
    assert_eq!(layout.focus_rect_at(1), Some((20, 0, 10, 10)), "and the other one took slot 1");
}
