//! `Player.aiStep`'s pickup area for a guest (`guest_pickups`): the guest's box
//! grown by (1.0, 0.5, 1.0), and an item taken when its box overlaps it. The
//! guest used to reach 1.5 from its centre on x and z and a fixed -0.6..2.4 on
//! y, so it took what vanilla leaves on the ground (#20542).
use crate::game::Game;
use horizon_client::items::ItemStack;

/// Guest 12 stands at `at` (its feet) with room for apples and an apple lies at
/// `item`: whether the guest takes it.
fn takes(game: &mut Game<'_>, at: [f64; 3], item: [f64; 3]) -> bool {
    while !game.dropped.is_empty() {
        game.remove_dropped(0);
    }
    game.lan
        .as_mut()
        .unwrap()
        .hub
        .update_guest(12, |guest| {
            guest.position = at;
            guest
                .inventory
                .fill(Some(ItemStack::new("minecraft:stone", 64)));
            guest.offhand = Some(ItemStack::new("minecraft:stone", 64));
            guest.inventory[0] = None;
        })
        .unwrap();
    let level = game.level(game.dim.id()).unwrap();
    let id = game
        .level_spawn_item(
            level,
            item[0],
            item[1],
            item[2],
            ItemStack::new("minecraft:apple", 1),
            0,
        )
        .unwrap()
        .id();
    let gi = game.guest_index(12).unwrap();
    game.guest_pickups(gi);
    !game.dropped.iter().any(|dropped| dropped.id == id)
}

#[test]
#[ignore = "daily"]
fn a_guest_takes_what_meets_their_box_grown_by_one_and_a_half_blocks() {
    crate::game::native_guest_turn_tests::with_guests(|game| {
        // The issue's two holes: the guest stands at x=2.3, the item lies in the corner of
        // the hole at x=0.875. The grown box starts at 1.0 and the item's ends at 1.0: they
        // touch, so vanilla leaves it there (`AABB.intersects` is strict).
        assert!(!takes(game, [2.3, 65.0, 0.7], [0.875, 65.0, 0.875]));
        // A fortieth closer: the boxes overlap, and vanilla takes it.
        assert!(takes(game, [2.3, 65.0, 0.7], [0.9, 65.0, 0.875]));
        // 1.425 apart on both x and z at once, the furthest a centre can be: still a touch.
        assert!(!takes(game, [2.3, 65.0, 2.3], [0.875, 65.0, 0.875]));
        // The box starts 0.5 under the feet, so an item's box 0.7 below still meets it and
        // one 0.75 below only touches it.
        assert!(takes(game, [3.0, 65.0, 5.0], [3.0, 64.3, 5.0]));
        assert!(!takes(game, [3.0, 65.0, 5.0], [3.0, 64.25, 5.0]));
    });
}