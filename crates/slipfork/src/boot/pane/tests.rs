use super::rows::{Slot, TaskRows};
use crate::boot::render::{Node, completed_row, outer_widths, running_row};

/// A three-line block places its phase row on top and its two output lines below it, in order.
#[test]
fn a_block_places_the_phase_row_on_top_and_output_oldest_first_downward() {
    assert_eq!(super::block_slots(31, 3), vec![29, 30, 31]);
    assert_eq!(super::block_slots(31, 1), vec![31]);
    assert_eq!(super::block_slots(2, 0), Vec::<usize>::new());
}

/// Two `Started` events in one tick own two distinct rows, spin together, and finish in place.
#[test]
fn two_started_tasks_spin_on_their_own_rows_and_finish_in_place() {
    let mut tasks = TaskRows::new(4, 10);
    assert_eq!(tasks.begin("image", "base"), Slot::Row(4));
    assert_eq!(tasks.begin("service", "front"), Slot::Row(5));
    assert_eq!(tasks.running(), ["base", "front"]);

    let widths = outer_widths(&[
        ("image".to_owned(), "base".to_owned()),
        ("service".to_owned(), "front".to_owned()),
    ]);
    let spinning: Vec<(usize, String)> = tasks
        .live()
        .enumerate()
        .map(|(offset, task)| {
            (
                task.slot,
                running_row(&task.kind, &task.name, offset + 1, 2, '⠋', 900, &widths),
            )
        })
        .collect();
    assert_eq!(spinning[0].0, 4);
    assert_eq!(spinning[1].0, 5);
    for (slot, row) in &spinning {
        assert!(row.contains('⠋'), "slot {slot} must spin: {row:?}");
        assert!(row.contains("running"), "slot {slot}: {row:?}");
    }

    let finished = completed_row(
        &Node {
            kind: "image",
            name: "base",
            state: "Cached",
            millis: 120,
            run_id: None,
            success: true,
        },
        1,
        2,
        &widths,
        false,
    );
    assert_eq!(finished, "✓ [1/2] image  base   cached  120ms");
    assert_eq!(
        tasks.complete("image", "base"),
        Some(4),
        "the finished row rewrites slot 4"
    );
    assert_eq!(tasks.running(), ["front"], "the other row keeps spinning");
    assert_eq!(
        tasks.live().next().map(|task| task.slot),
        Some(5),
        "the live row never moves"
    );
    assert_eq!(tasks.complete("service", "front"), Some(5));
    assert!(tasks.running().is_empty());
}

/// A full grid scrolls exactly one line and the task that leaves the top is gone for good.
#[test]
fn a_full_grid_scrolls_one_line_and_drops_the_top_task() {
    let mut tasks = TaskRows::new(2, 2);
    assert_eq!(tasks.begin("image", "one"), Slot::Row(2));
    assert_eq!(tasks.begin("service", "two"), Slot::Scrolled(2));
    let slots: Vec<usize> = tasks.live().map(|task| task.slot).collect();
    assert_eq!(slots, [1, 2], "every surviving task moves up one row");

    assert_eq!(tasks.begin("setup", "three"), Slot::Scrolled(2));
    let names: Vec<&str> = tasks.running();
    assert_eq!(names, ["two", "three"], "the row-1 task left the grid");
    assert_eq!(tasks.complete("one", "gone"), None);
}

/// The narrative block shrinks the grid without dropping a settled row's slot, and shifts
/// bottom-row settled allocations upward when a block opens over `region_bottom`.
#[test]
fn narrative_block_reserves_rows_by_scrolling_settled_slots_upward() {
    let mut tasks = TaskRows::new(5, 20);
    assert_eq!(tasks.begin("service", "api"), Slot::Row(5));
    assert_eq!(tasks.reserve_below(12), 0);
    assert_eq!(tasks.next_row(), 6);
    tasks.set_bottom(20);
    assert_eq!(tasks.next_row(), 6, "restored bottom keeps the next slot");

    let mut bottom_seeded = TaskRows::new(39, 39);
    assert_eq!(bottom_seeded.place(), Slot::Row(39));
    assert_eq!(bottom_seeded.reserve_below(38), 1);
    assert_eq!(bottom_seeded.next_row(), 38);
}
