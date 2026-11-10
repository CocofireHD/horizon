//! The in-game Wardrobe (`game/wardrobe.rs`), opened with Right Shift: a compact
//! three-column locker overlay over the world, with a slot list, an owned-item
//! grid and a live player preview.
use crate::{
    font::Patch,
    hud::{HudAtlas, HudBatch, WARDROBE_PREVIEW_SIZE, WARDROBE_PREVIEW_SLOTS, rgba},
    text::Text,
    ui,
};
use std::{collections::HashMap, sync::Arc, sync::atomic::AtomicU32};

/// The locker draws at a finer pixel grid than the game's GUI (half the
/// scale, twice the resolution). Widget bounds are still reported to agents
/// in the game's own GUI pixels, so horizon-ctl clicks stay where the
/// widgets look.
static GAME_SCALE_BITS: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

/// Set the game GUI scale the drawn coordinates should be reported in.
pub fn set_game_scale(scale: f32) {
    GAME_SCALE_BITS.store(
        scale.max(1.0).to_bits(),
        std::sync::atomic::Ordering::Relaxed,
    );
}

fn game_rect(rect: [f32; 4]) -> [f32; 4] {
    let s = f32::from_bits(GAME_SCALE_BITS.load(std::sync::atomic::Ordering::Relaxed));
    [rect[0] / s, rect[1] / s, rect[2] / s, rect[3] / s]
}

/// A slot row: the slot's name, what it wears now and how many items are owned.
#[derive(Clone, Debug)]
pub struct SlotRow {
    pub name: String,
    pub under: String,
    pub chosen: bool,
    pub count: usize,
}

/// One choice in the selected slot's list, "Nothing" included (`id` None).
#[derive(Clone, Debug)]
pub struct ItemRow {
    pub id: Option<String>,
    pub name: String,
    pub under: String,
    pub worn: bool,
    /// The account owns it (the "All" scope also shows items from the shop).
    pub owned: bool,
}

/// A card's CPU-rendered item on the neutral mannequin.
#[derive(Clone, Debug)]
pub struct PreviewArt {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

/// What the right stage shows: the current outfit, or one item selected for
/// preview (`Item(None)` is "Nothing").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PreviewItem {
    #[default]
    Outfit,
    Item(Option<String>),
}

/// What the screen shows (game/wardrobe.rs fills and translates it).
#[derive(Clone, Debug, Default)]
pub struct WardrobeView {
    pub title: String,
    pub status: String,
    /// The status asks for attention (offline, a failure, signed out).
    pub status_warn: bool,
    pub slots: Vec<SlotRow>,
    pub items: Vec<ItemRow>,
    /// "Show my cosmetics".
    pub visible: bool,
    /// The three-way others' cosmetics value, in words.
    pub others: String,
    /// Whether other players' cosmetics are shown at all.
    pub others_on: bool,
    /// Other players' name tags are shown but their models are not.
    pub others_names: bool,
    pub auras: bool,
    pub toast: Option<String>,
    pub claim: bool,
    pub retry: bool,
    pub shop: bool,
    /// The first answer hasn't come yet: the buttons that change things wait.
    pub busy: bool,
    /// First visible item row.
    pub scroll: f32,
    /// Scope filter label (All / Owned / Equipped).
    pub scope: String,
    /// Sort filter label (Name / Recent / Rarity).
    pub sort: String,
    /// Search text.
    pub search: String,
    /// The selected slot's key (`head`, `back`, ...), for the cards' previews.
    pub slot: String,
    /// Card previews, by the item's index in [`WardrobeView::items`].
    pub previews: HashMap<usize, PreviewArt>,
    /// What the right stage is showing.
    pub preview: PreviewItem,
    /// The previewed item is owned (or is "Nothing").
    pub preview_owned: bool,
    /// The previewed item is already equipped.
    pub preview_equipped: bool,
    /// The item name shown under "Loadout".
    pub preview_label: String,
}

/// What a click asks the game to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Slot(usize),
    /// Show this item on the right stage, without changing the outfit.
    Preview(Option<String>),
    /// Put the selected preview item on (or take it off).
    Equip(Option<String>),
    Visible,
    Others,
    Auras,
    Claim,
    Shop,
    Retry,
    Done,
    Scope,
    Sort,
}

const CARD_GAP: f32 = 4.0;

fn grey() -> u32 {
    rgba(160, 160, 160, 255)
}
fn warn() -> u32 {
    rgba(255, 190, 110, 255)
}
const WHITE: u32 = u32::MAX;
const GREEN: u32 = 0xFF76_E600;
const CARD: u32 = 0xFF24_1D1B;

fn modal_rect(size: (f32, f32)) -> [f32; 4] {
    let (w, h) = size;
    // A little breathing room, then the modal follows the window: it grows
    // on a large GUI instead of staying a fixed card in the middle.
    let mw = (w * 0.92).clamp(300.0, 1180.0);
    let mh = (h * 0.92).clamp(240.0, 680.0);
    [(w - mw) / 2.0, (h - mh) / 2.0, mw, mh]
}

fn inner_rect(mr: [f32; 4]) -> [f32; 4] {
    // A slim title bar across the modal, then the three columns.
    [mr[0] + 10.0, mr[1] + 28.0, mr[2] - 20.0, mr[3] - 38.0]
}

fn columns(size: (f32, f32)) -> (f32, f32, f32, [f32; 4]) {
    let mr = modal_rect(size);
    let ir = inner_rect(mr);
    // The locker: a slim category sidebar, the grid, and the loadout stage.
    // The stage stays the hero and keeps the largest share.
    let left = (ir[2] * 0.22).clamp(88.0, 190.0);
    let right = (ir[2] * 0.28).clamp(104.0, 320.0);
    let gap = 8.0;
    let center = (ir[2] - left - right - gap * 2.0).max(90.0);
    (left, center, right, ir)
}

/// The scope and sort chip widths, shared by the layout and the drawing so
/// the search field always lands exactly between them.
fn chip_w(center_w: f32) -> (f32, f32) {
    let sort = (center_w * 0.20).clamp(34.0, 58.0);
    let scope = (((center_w - sort - 12.0) * 0.42).clamp(44.0, 90.0))
        .min((center_w - sort - 12.0).max(44.0));
    (scope, sort)
}

fn grid_cols(center_w: f32) -> usize {
    ((center_w / 88.0).round() as usize).clamp(2, 5)
}

fn card_size(center_w: f32) -> (usize, f32, f32) {
    let cols = grid_cols(center_w);
    let w = ((center_w - CARD_GAP * (cols as f32 - 1.0)) / cols as f32).max(40.0);
    let h = w * 0.60 + 20.0;
    (cols, w, h)
}

fn toolbar_h() -> f32 {
    20.0
}

/// Where the player preview goes, in GUI pixels: inside the right stage's
/// outfit box, above the compact switches.
pub fn preview_rect(size: (f32, f32)) -> [f32; 4] {
    let (left, center, right, ir) = columns(size);
    let rx = ir[0] + left + 8.0 + center + 8.0;
    let rw = right;
    let pw = (rw - 16.0).max(48.0);
    // Below the "Your Loadout" header and the outfit box header, above the
    // switch bar and the helper line.
    let ph = (ir[3] - 24.0 - 22.0 - 34.0 - 8.0).clamp(60.0, 420.0);
    let px = rx + 8.0;
    let py = ir[1] + 24.0 + 22.0;
    [px, py, px + pw, py + ph]
}

/// How many item cards fit in the grid.
pub fn item_rows(size: (f32, f32)) -> usize {
    let (_, center, _, ir) = columns(size);
    let (cols, _cw, ch) = card_size(center);
    let avail = ir[3] - toolbar_h() - 6.0 - 4.0;
    // The last row has no gap after it.
    let rows = ((avail + CARD_GAP) / (ch + CARD_GAP)).floor() as usize;
    cols * rows.max(1)
}

/// Where the grid must stop scrolling: the first card of the last page,
/// aligned to a column so the page never starts in the middle of a row.
pub fn max_scroll(size: (f32, f32), view: &WardrobeView) -> f32 {
    let visible = item_rows(size);
    let total = view.items.len();
    if total <= visible {
        return 0.0;
    }
    let cols = grid_cols(columns(size).1);
    let first = total - visible;
    (first.div_ceil(cols) * cols) as f32
}

/// The scroll after a wheel step (positive is up), kept in range.
pub fn scroll(size: (f32, f32), view: &WardrobeView, delta: f32) -> f32 {
    (view.scroll - delta).clamp(0.0, max_scroll(size, view))
}

struct SlotRect {
    index: usize,
    rect: [f32; 4],
}

struct ItemRect {
    index: usize,
    rect: [f32; 4],
}

struct ButtonRect {
    action: Action,
    rect: [f32; 4],
    active: bool,
    label: String,
    /// A switch row's state, when it is one.
    on: Option<bool>,
    /// The middle state of a three-way switch (Others: name tags only).
    partial: bool,
    /// Drawn over the player preview after it, not with the other buttons.
    overlay: bool,
}

struct Layout {
    slots: Vec<SlotRect>,
    items: Vec<ItemRect>,
    buttons: Vec<ButtonRect>,
}

fn layout(size: (f32, f32), view: &WardrobeView) -> Layout {
    let mr = modal_rect(size);
    let ir = inner_rect(mr);
    let (left_w, center_w, right_w, _) = columns(size);
    let left_x = ir[0];
    let center_x = ir[0] + left_w + 8.0;
    let right_x = center_x + center_w + 8.0;

    // Slot rows in the sidebar, under the "Cosmetics" group header and above
    // the shop button.
    let mut slots = Vec::new();
    let side_top = ir[1] + 24.0;
    let shop_h = if view.shop { 26.0 } else { 0.0 };
    let side_bottom = ir[1] + ir[3] - shop_h - 4.0;
    for index in 0..view.slots.len() {
        let y = side_top + index as f32 * 22.0;
        if y + 20.0 > side_bottom {
            break;
        }
        slots.push(SlotRect {
            index,
            rect: [left_x + 6.0, y, left_w - 12.0, 20.0],
        });
    }

    let toolbar_y = ir[1] + 2.0;
    let (cols, card_w, card_h) = card_size(center_w);
    // Any part of a row scrolled moves the whole grid on to the next row.
    let requested = view.scroll.floor() as usize;
    let first = requested.div_ceil(cols) * cols;
    let visible = item_rows(size);
    let grid_top = toolbar_y + toolbar_h() + 6.0;
    let mut items = Vec::new();
    for offset in 0..visible {
        let index = first + offset;
        if index >= view.items.len() {
            break;
        }
        let row = offset / cols;
        let col = offset % cols;
        let x = center_x + col as f32 * (card_w + CARD_GAP);
        let y = grid_top + row as f32 * (card_h + CARD_GAP);
        items.push(ItemRect {
            index,
            rect: [x, y, card_w, card_h],
        });
    }

    let mut buttons = Vec::new();
    macro_rules! push {
        ($action:expr, $rect:expr, $active:expr, $label:expr $(,)?) => {
            buttons.push(ButtonRect {
                action: $action,
                rect: $rect,
                active: $active,
                label: $label,
                on: None,
                partial: false,
                overlay: false,
            })
        };
    }

    // Scope and sort chips in the catalogue toolbar.
    let (scope_w, sort_w) = chip_w(center_w);
    push!(
        Action::Scope,
        [center_x, toolbar_y, scope_w, toolbar_h()],
        !view.busy,
        view.scope.clone(),
    );
    push!(
        Action::Sort,
        [center_x + center_w - sort_w, toolbar_y, sort_w, toolbar_h(),],
        !view.busy,
        view.sort.clone(),
    );

    // Compact switch cells under the preview.
    let bar_y = ir[1] + ir[3] - 28.0;
    let cell_gap = 4.0;
    let cell_w = ((right_w - 16.0 - cell_gap * 2.0) / 3.0).max(30.0);
    let toggles = [
        (
            Action::Visible,
            crate::lang::get_or("horizon.wardrobe.mine_short", "Mine"),
        ),
        (
            Action::Others,
            crate::lang::get_or("horizon.wardrobe.others_short", "Others"),
        ),
        (
            Action::Auras,
            crate::lang::get_or("horizon.wardrobe.auras_short", "Auras"),
        ),
    ];
    let switch_start = buttons.len();
    for (i, (action, label)) in toggles.into_iter().enumerate() {
        push!(
            action,
            [
                right_x + 8.0 + i as f32 * (cell_w + cell_gap),
                bar_y,
                cell_w,
                26.0,
            ],
            !view.busy,
            label,
        );
    }
    let toggle_states = [
        (view.visible, false),
        (view.others_on, view.others_names),
        (view.auras, false),
    ];
    for (button, (on, partial)) in buttons[switch_start..].iter_mut().zip(toggle_states) {
        button.on = Some(on);
        button.partial = partial;
    }

    // The purple shop button at the bottom of the sidebar.
    if view.shop {
        push!(
            Action::Shop,
            [left_x + 6.0, ir[1] + ir[3] - 26.0, left_w - 12.0, 22.0],
            !view.busy,
            crate::lang::get_or("horizon.wardrobe.shop", "Shop Cosmetics"),
        );
    }
    if view.claim {
        push!(
            Action::Claim,
            [mr[0] + mr[2] - 154.0, mr[1] + 3.0, 56.0, 20.0],
            !view.busy,
            crate::lang::get_or("horizon.wardrobe.claim", "Claim"),
        );
    } else if view.retry {
        push!(
            Action::Retry,
            [mr[0] + mr[2] - 154.0, mr[1] + 3.0, 56.0, 20.0],
            !view.busy,
            crate::lang::get_or("horizon.wardrobe.retry", "Retry"),
        );
    }
    push!(
        Action::Done,
        [mr[0] + mr[2] - 30.0, mr[1] + 3.0, 22.0, 20.0],
        true,
        "×".to_owned(),
    );

    // Equip / take off the previewed item, over the bottom of the stage.
    if let PreviewItem::Item(id) = &view.preview {
        let [px, _, px1, py1] = preview_rect(size);
        let label = if !view.preview_owned {
            crate::lang::get_or("horizon.wardrobe.locked", "Locked")
        } else if view.preview_equipped {
            crate::lang::get_or("horizon.wardrobe.equipped", "Equipped")
        } else if id.is_none() {
            crate::lang::get_or("horizon.wardrobe.take_off", "Take off")
        } else {
            crate::lang::get_or("horizon.wardrobe.equip", "Equip")
        };
        let button_w = ((px1 - px) * 0.58).clamp(64.0, 110.0);
        push!(
            Action::Equip(id.clone()),
            [px + (px1 - px - button_w) / 2.0, py1 - 25.0, button_w, 18.0],
            view.preview_owned && !view.preview_equipped && !view.busy,
            label,
        );
        buttons.last_mut().unwrap().overlay = true;
    }

    Layout {
        slots,
        items,
        buttons,
    }
}

/// `text` cut to `width`, with "..." when it doesn't fit.
fn fit(atlas: &HudAtlas, text: &str, width: f32) -> String {
    if width <= 0.0 {
        return String::new();
    }
    if HudBatch::text_width(atlas, text) <= width {
        return text.to_owned();
    }
    let mut cut = String::new();
    for c in text.chars() {
        let mut next = cut.clone();
        next.push(c);
        next.push_str("...");
        if HudBatch::text_width(atlas, &next) > width {
            break;
        }
        cut.push(c);
    }
    cut.push_str("...");
    cut
}

/// A card label at most two lines, each cut by [`fit`]; `scale` is the text
/// scale the caller draws it at.
fn wrap_two(atlas: &HudAtlas, text: &str, width: f32, scale: f32) -> Vec<String> {
    let room = width / scale;
    if HudBatch::text_width(atlas, text) <= room {
        return vec![text.to_owned()];
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= 1 {
        return vec![fit(atlas, text, room)];
    }
    let mut first = String::new();
    let mut split = words.len();
    for (i, word) in words.iter().enumerate() {
        let candidate = if first.is_empty() {
            (*word).to_owned()
        } else {
            format!("{first} {word}")
        };
        if !first.is_empty() && HudBatch::text_width(atlas, &candidate) > room {
            split = i;
            break;
        }
        first = candidate;
    }
    let first = fit(atlas, &first, room);
    if split >= words.len() {
        return vec![first];
    }
    let rest = fit(atlas, &words[split..].join(" "), room);
    vec![first, rest]
}

fn chip(
    batch: &mut HudBatch,
    atlas: &HudAtlas,
    rect: [f32; 4],
    text: &str,
    active: bool,
    hover: bool,
) {
    let [x, y, w, h] = rect;
    let fill = if !active {
        rgba(255, 255, 255, 8)
    } else if hover {
        rgba(255, 255, 255, 24)
    } else {
        rgba(255, 255, 255, 12)
    };
    batch.fill(atlas, x, y, w, h, fill);
    let border = rgba(255, 255, 255, 18);
    batch.fill(atlas, x, y, w, 1.0, border);
    batch.fill(atlas, x, y + h - 1.0, w, 1.0, border);
    batch.fill(atlas, x, y, 1.0, h, border);
    batch.fill(atlas, x + w - 1.0, y, 1.0, h, border);
    let label = fit(atlas, text, w - 16.0);
    let tw = HudBatch::text_width(atlas, &label);
    batch.text(
        atlas,
        &label,
        x + (w - 8.0 - tw) / 2.0,
        y + (h - 8.0) / 2.0,
        if active { WHITE } else { grey() },
        false,
    );
    let cx = x + w - 9.0;
    let cy = y + h / 2.0;
    batch.fill(atlas, cx, cy - 1.0, 4.0, 1.0, grey());
    batch.fill(atlas, cx + 1.0, cy, 2.0, 1.0, grey());
    crate::widgets::record(text, game_rect(rect), active);
    crate::widgets::widget_cursor(hover, active);
}

fn pill(
    batch: &mut HudBatch,
    atlas: &HudAtlas,
    rect: [f32; 4],
    text: &str,
    active: bool,
    hover: bool,
    accent: Option<u32>,
) {
    let [x, y, w, h] = rect;
    let fill = if !active {
        rgba(255, 255, 255, 8)
    } else if let Some(accent) = accent {
        accent
    } else if hover {
        rgba(255, 255, 255, 28)
    } else {
        rgba(255, 255, 255, 13)
    };
    batch.fill(atlas, x, y, w, h, fill);
    let color = if active {
        WHITE
    } else {
        rgba(160, 160, 160, 255)
    };
    let shown = fit(atlas, text, w - 8.0);
    let tw = HudBatch::text_width(atlas, &shown);
    batch.text(
        atlas,
        &shown,
        x + (w - tw) / 2.0,
        y + (h - 8.0) / 2.0,
        color,
        false,
    );
    crate::widgets::record(text, game_rect(rect), active);
    crate::widgets::widget_cursor(hover, active);
}

/// A small neutral mannequin on a pedestal, tinted for the slot a card is
/// previewing: enough to read a locker tile without a second 3D pass per card.
fn card_mannequin(batch: &mut HudBatch, atlas: &HudAtlas, rect: [f32; 4], slot: &str, worn: bool) {
    let [x, y, w, h] = rect;
    let body = rgba(136, 140, 148, 255);
    let shade = rgba(104, 108, 116, 255);
    let accent = if worn {
        GREEN
    } else {
        rgba(198, 190, 235, 255)
    };
    let cx = x + w / 2.0;
    let head = (h * 0.16).max(5.0);
    let top = y + h * 0.18;
    let bw = (w * 0.20).max(6.0);
    let bh = (h * 0.30).max(7.0);
    // Pedestal.
    batch.fill(atlas, x + w * 0.16, y + h - 5.0, w * 0.68, 3.0, shade);
    // Head, body, arms and legs.
    batch.fill(atlas, cx - head / 2.0, top, head, head, body);
    batch.fill(atlas, cx - bw / 2.0, top + head + 1.0, bw, bh, body);
    batch.fill(
        atlas,
        cx - bw / 2.0 - 2.0,
        top + head + 2.0,
        2.0,
        bh * 0.72,
        shade,
    );
    batch.fill(
        atlas,
        cx + bw / 2.0,
        top + head + 2.0,
        2.0,
        bh * 0.72,
        shade,
    );
    batch.fill(
        atlas,
        cx - bw / 2.0,
        top + head + bh + 1.0,
        2.0,
        h * 0.20,
        shade,
    );
    batch.fill(
        atlas,
        cx + bw / 2.0 - 2.0,
        top + head + bh + 1.0,
        2.0,
        h * 0.20,
        shade,
    );
    // What sits on the mannequin.
    match slot {
        "head" => batch.fill(
            atlas,
            cx - head / 2.0 - 1.0,
            top - 2.0,
            head + 2.0,
            2.0,
            accent,
        ),
        "back" => batch.fill(
            atlas,
            cx + bw / 2.0 - 1.0,
            top + head + 1.0,
            3.0,
            bh,
            accent,
        ),
        "torso" => batch.fill(atlas, cx - bw / 2.0, top + head + 1.0, bw, bh, accent),
        "shoulder" => {
            batch.fill(atlas, cx + bw / 2.0 - 1.0, top + head, 4.0, 4.0, accent);
        }
        "name_tag" => batch.fill(
            atlas,
            cx - bw / 2.0,
            top + head + bh * 0.45,
            bw,
            2.0,
            accent,
        ),
        _ => {}
    }
}

pub fn draw(
    batch: &mut HudBatch,
    atlas: &HudAtlas,
    cursor: (f32, f32),
    view: &WardrobeView,
    in_world: bool,
) {
    let size = (batch.width, batch.height);
    let (w, h) = size;
    // Dim the world behind the modal.
    batch.fill(atlas, 0.0, 0.0, w, h, rgba(0, 0, 0, 130));
    let mr = modal_rect(size);
    // Modal panel.
    batch.fill(atlas, mr[0], mr[1], mr[2], mr[3], rgba(17, 18, 21, 235));
    let ir = inner_rect(mr);
    let (left_w, center_w, right_w, _) = columns(size);
    let center_x = ir[0] + left_w + 8.0;
    let right_x = center_x + center_w + 8.0;

    // Modal title bar.
    batch.fill(atlas, mr[0], mr[1], mr[2], 24.0, rgba(23, 24, 29, 255));
    batch.fill(
        atlas,
        mr[0],
        mr[1] + 23.0,
        mr[2],
        1.0,
        rgba(255, 255, 255, 20),
    );
    let title = fit(atlas, &view.title, mr[2] * 0.32);
    batch.text(atlas, &title, mr[0] + 10.0, mr[1] + 7.0, WHITE, true);
    if !view.status.is_empty() {
        let status = fit(atlas, &view.status, mr[2] * 0.36);
        let sw = HudBatch::text_width(atlas, &status);
        let color = if view.status_warn { warn() } else { grey() };
        batch.text(
            atlas,
            &status,
            mr[0] + (mr[2] - sw) / 2.0,
            mr[1] + 7.0,
            color,
            false,
        );
    }

    // Sidebar panel: the "Cosmetics" group header, then the slot rows.
    batch.fill(atlas, ir[0], ir[1], left_w, ir[3], rgba(20, 21, 26, 255));
    let total: usize = view.slots.iter().map(|s| s.count).sum();
    let header = format!(
        "{} ({total})",
        crate::lang::get_or("horizon.wardrobe.cosmetics", "Cosmetics")
    );
    batch.text(
        atlas,
        &fit(atlas, &header, left_w - 16.0),
        ir[0] + 8.0,
        ir[1] + 6.0,
        WHITE,
        true,
    );
    batch.fill(
        atlas,
        ir[0],
        ir[1] + 20.0,
        left_w,
        1.0,
        rgba(255, 255, 255, 14),
    );
    let layout = layout(size, view);

    // Slot rows: name, count badge, green edge on the chosen one.
    for slot in &layout.slots {
        let found = &view.slots[slot.index];
        let hover = ui::hovered(
            cursor,
            slot.rect[0],
            slot.rect[1],
            slot.rect[2],
            slot.rect[3],
        );
        let [x, y, w, h] = slot.rect;
        let fill = if found.chosen {
            rgba(255, 255, 255, 26)
        } else if hover {
            rgba(255, 255, 255, 10)
        } else {
            rgba(0, 0, 0, 0)
        };
        batch.fill(atlas, x, y, w, h, fill);
        if found.chosen {
            batch.fill(atlas, x, y, 2.0, h, GREEN);
        }
        let label = fit(atlas, &found.name, w - 30.0);
        batch.text(
            atlas,
            &label,
            x + 7.0,
            y + 6.0,
            if found.chosen { WHITE } else { grey() },
            false,
        );
        let count = found.count.to_string();
        let cw = HudBatch::text_width(atlas, &count);
        batch.text(
            atlas,
            &count,
            x + w - cw - 7.0,
            y + 6.0,
            if found.chosen { WHITE } else { grey() },
            false,
        );
        crate::widgets::record(&found.name, game_rect(slot.rect), true);
        crate::widgets::target(game_rect(slot.rect));
        crate::widgets::widget_cursor(hover, true);
    }

    // Catalogue panel behind the grid, and its toolbar: scope chip, search
    // field, sort chip.
    batch.fill(
        atlas,
        center_x,
        ir[1],
        center_w,
        ir[3],
        rgba(23, 24, 29, 255),
    );
    let toolbar_y = ir[1] + 2.0;
    let (scope_w, sort_w) = chip_w(center_w);
    let search_x = center_x + scope_w + 4.0;
    let search_w = (center_w - scope_w - sort_w - 8.0).max(0.0);
    if search_w >= 24.0 {
        batch.fill(
            atlas,
            search_x,
            toolbar_y,
            search_w,
            toolbar_h(),
            rgba(0, 0, 0, 110),
        );
        batch.fill(
            atlas,
            search_x,
            toolbar_y,
            search_w,
            1.0,
            rgba(255, 255, 255, 18),
        );
        batch.fill(
            atlas,
            search_x,
            toolbar_y + toolbar_h() - 1.0,
            search_w,
            1.0,
            rgba(255, 255, 255, 18),
        );
        let search_text = if view.search.is_empty() {
            crate::lang::get_or("horizon.wardrobe.search", "Search...")
        } else {
            view.search.clone()
        };
        batch.text(
            atlas,
            &fit(atlas, &search_text, search_w - 8.0),
            search_x + 5.0,
            toolbar_y + 6.0,
            grey(),
            false,
        );
    }

    // Loadout stage: header, then the outfit box the preview sits in.
    batch.fill(atlas, right_x, ir[1], right_w, ir[3], rgba(20, 21, 26, 255));
    batch.text(
        atlas,
        &fit(
            atlas,
            &crate::lang::get_or("horizon.wardrobe.loadout", "Your Loadout"),
            right_w - 16.0,
        ),
        right_x + 8.0,
        ir[1] + 6.0,
        WHITE,
        true,
    );
    batch.fill(
        atlas,
        right_x,
        ir[1] + 20.0,
        right_w,
        1.0,
        rgba(255, 255, 255, 14),
    );
    let stage = preview_rect(size);
    // The outfit box around the preview: a header strip with the preview's
    // name, then the darker picture area below it.
    let strip_y = stage[1] - 26.0;
    batch.fill(
        atlas,
        stage[0] - 8.0,
        strip_y,
        stage[2] - stage[0] + 16.0,
        stage[3] - stage[1] + 34.0,
        rgba(15, 16, 19, 255),
    );
    batch.fill(
        atlas,
        stage[0] - 8.0,
        strip_y,
        stage[2] - stage[0] + 16.0,
        18.0,
        rgba(28, 30, 36, 255),
    );
    let outfit = fit(atlas, &view.preview_label, stage[2] - stage[0]);
    let ow = HudBatch::text_width(atlas, &outfit);
    batch.text(
        atlas,
        &outfit,
        (stage[0] + stage[2]) / 2.0 - ow / 2.0,
        strip_y + 5.0,
        WHITE,
        false,
    );
    batch.fill(
        atlas,
        stage[0] - 4.0,
        stage[1] - 4.0,
        stage[2] - stage[0] + 8.0,
        stage[3] - stage[1] + 8.0,
        rgba(0, 0, 0, 80),
    );

    // Item cards.
    for item in &layout.items {
        let found = &view.items[item.index];
        let hover = ui::hovered(
            cursor,
            item.rect[0],
            item.rect[1],
            item.rect[2],
            item.rect[3],
        );
        let [x, y, w, h] = item.rect;
        batch.fill(atlas, x, y, w, h, CARD);
        let preview_h = (w * 0.60).max(30.0);
        // Equipped cards wear the dark-green stage with the green bar.
        batch.fill(
            atlas,
            x + 4.0,
            y + 4.0,
            w - 8.0,
            preview_h,
            if found.worn {
                rgba(32, 84, 41, 255)
            } else {
                rgba(30, 32, 38, 255)
            },
        );
        let sprite = format!(
            "horizon/wardrobe-preview{}",
            item.index % WARDROBE_PREVIEW_SLOTS
        );
        if let (Some(art), Some(rect)) =
            (view.previews.get(&item.index), atlas.sprites.get(&sprite))
        {
            batch.patches.push(Patch {
                x: (rect.u0 * atlas.width as f32).round() as u32,
                y: (rect.v0 * atlas.height as f32).round() as u32,
                width: art.width.min(WARDROBE_PREVIEW_SIZE),
                height: art.height.min(WARDROBE_PREVIEW_SIZE),
                rgba: art.rgba.as_ref().clone(),
            });
            let scale = (preview_h / art.height as f32).min((w - 8.0) / art.width as f32);
            let dw = art.width as f32 * scale;
            let dh = art.height as f32 * scale;
            batch.rect_uv(
                x + 4.0 + (w - 8.0 - dw) / 2.0,
                y + 4.0 + (preview_h - dh) / 2.0,
                dw,
                dh,
                *rect,
                u32::MAX,
            );
        } else {
            card_mannequin(
                batch,
                atlas,
                [x + 4.0, y + 4.0, w - 8.0, preview_h],
                &view.slot,
                found.worn,
            );
        }
        if hover {
            batch.fill(atlas, x, y, w, h, rgba(255, 255, 255, 14));
        }
        if !found.owned {
            let badge = crate::lang::get_or("horizon.wardrobe.locked", "Locked");
            let bw = HudBatch::text_width(atlas, &badge) + 8.0;
            batch.fill(atlas, x + 4.0, y + 6.0, bw, 12.0, rgba(0, 0, 0, 150));
            batch.text(atlas, &badge, x + 8.0, y + 7.0, grey(), false);
        }
        // The equipped bar across the bottom of the picture.
        if found.worn {
            let bar_h = 12.0;
            batch.fill(
                atlas,
                x + 4.0,
                y + 4.0 + preview_h - bar_h,
                w - 8.0,
                bar_h,
                GREEN,
            );
            let badge = if w >= 78.0 {
                crate::lang::get_or("horizon.wardrobe.equipped_badge", "✔ Equipped")
            } else {
                crate::lang::get_or("horizon.wardrobe.equipped_short", "✔")
            };
            let bw = HudBatch::text_width(atlas, &badge);
            batch.text(
                atlas,
                &badge,
                x + 4.0 + (w - 8.0 - bw) / 2.0,
                y + 4.0 + preview_h - bar_h + 2.0,
                rgba(6, 30, 8, 255),
                false,
            );
        }
        let name_scale = 0.8;
        let lines = wrap_two(atlas, &found.name, w - 8.0, name_scale);
        for (i, line) in lines.iter().take(2).enumerate() {
            let lw = HudBatch::text_width(atlas, line) * name_scale;
            ui::text_scaled(
                batch,
                atlas,
                &Text::literal(line),
                x + (w - lw) / 2.0,
                y + preview_h + 5.0 + i as f32 * 8.0,
                name_scale,
                WHITE,
            );
        }
        let previewing = match &view.preview {
            PreviewItem::Item(id) => found.id.as_ref() == id.as_ref(),
            PreviewItem::Outfit => false,
        };
        if previewing {
            let edge = rgba(255, 255, 255, 170);
            batch.fill(atlas, x, y, w, 1.0, edge);
            batch.fill(atlas, x, y + h - 1.0, w, 1.0, edge);
            batch.fill(atlas, x, y, 1.0, h, edge);
            batch.fill(atlas, x + w - 1.0, y, 1.0, h, edge);
        }
        crate::widgets::record(&found.name, game_rect(item.rect), true);
        crate::widgets::target(game_rect(item.rect));
        crate::widgets::widget_cursor(hover, true);
    }

    if view.items.is_empty() {
        let empty = crate::lang::get_or("horizon.wardrobe.empty", "No items to show");
        let text = fit(atlas, &empty, center_w - 16.0);
        let tw = HudBatch::text_width(atlas, &text);
        batch.text(
            atlas,
            &text,
            center_x + (center_w - tw) / 2.0,
            ir[1] + ir[3] / 2.0,
            grey(),
            false,
        );
    }

    // Scrollbar for the grid.
    let max = max_scroll(size, view);
    if max > 0.0 {
        let top = ir[1] + 2.0 + toolbar_h() + 6.0;
        let h = ir[1] + ir[3] - top - 4.0;
        let x = center_x + center_w - 4.0;
        let bar = (h * (item_rows(size) as f32 / view.items.len() as f32)).max(8.0);
        let at = top + (h - bar) * (view.scroll / max);
        batch.fill(atlas, x, top, 2.0, h, rgba(0, 0, 0, 100));
        batch.fill(atlas, x, at, 2.0, bar, rgba(255, 255, 255, 120));
    }

    // Right stage toggles (the stage backdrop is drawn with the header).
    for button in &layout.buttons {
        if button.overlay {
            continue;
        }
        let hover = button.active
            && ui::hovered(
                cursor,
                button.rect[0],
                button.rect[1],
                button.rect[2],
                button.rect[3],
            );
        if matches!(button.action, Action::Scope | Action::Sort) {
            chip(
                batch,
                atlas,
                button.rect,
                &button.label,
                button.active,
                hover,
            );
        } else if let Some(on) = button.on {
            // A compact switch cell: label above, switch below.
            let [x, y, w, h] = button.rect;
            if hover {
                batch.fill(atlas, x, y, w, h, rgba(255, 255, 255, 10));
            }
            let label = fit(atlas, &button.label, (w - 4.0) / 0.7);
            let tw = HudBatch::text_width(atlas, &label) * 0.7;
            ui::text_scaled(
                batch,
                atlas,
                &Text::literal(&label),
                x + (w - tw) / 2.0,
                y + 3.0,
                0.7,
                if button.active { WHITE } else { grey() },
            );
            let sx = x + (w - 22.0) / 2.0;
            let sy = y + 15.0;
            batch.fill(
                atlas,
                sx,
                sy,
                22.0,
                9.0,
                if on && button.active {
                    GREEN
                } else {
                    rgba(255, 255, 255, 52)
                },
            );
            let knob_x = if !on {
                sx + 2.0
            } else if button.partial {
                sx + 7.0
            } else {
                sx + 12.0
            };
            batch.fill(
                atlas,
                knob_x,
                sy + 2.0,
                8.0,
                5.0,
                if button.active { WHITE } else { grey() },
            );
            if button.action == Action::Others && hover {
                ui::tooltip(batch, atlas, &[Text::literal(&view.others)], cursor);
            }
            crate::widgets::record(&button.label, game_rect(button.rect), button.active);
            crate::widgets::widget_cursor(hover, button.active);
        } else if button.action == Action::Done {
            // Close button: a small dark chip.
            batch.fill(
                atlas,
                button.rect[0],
                button.rect[1],
                button.rect[2],
                button.rect[3],
                if hover {
                    rgba(255, 255, 255, 34)
                } else {
                    rgba(255, 255, 255, 18)
                },
            );
            let t = fit(atlas, &button.label, button.rect[2] - 4.0);
            let tw = HudBatch::text_width(atlas, &t);
            batch.text(
                atlas,
                &t,
                button.rect[0] + (button.rect[2] - tw) / 2.0,
                button.rect[1] + 4.0,
                WHITE,
                false,
            );
            crate::widgets::declare_action(
                crate::widgets::Action::Done,
                game_rect(button.rect),
                button.active,
            );
        } else {
            let accent = (button.action == Action::Shop).then_some(rgba(109, 90, 240, 255));
            pill(
                batch,
                atlas,
                button.rect,
                &button.label,
                button.active,
                hover,
                accent,
            );
        }
    }

    let _ = in_world;
}

/// The equip/take-off pill over the player preview, drawn after the player.
pub fn draw_preview_actions(
    batch: &mut HudBatch,
    atlas: &HudAtlas,
    cursor: (f32, f32),
    view: &WardrobeView,
) {
    let layout = layout((batch.width, batch.height), view);
    let Some(button) = layout.buttons.iter().find(|button| button.overlay) else {
        return;
    };
    let hover = button.active
        && ui::hovered(
            cursor,
            button.rect[0],
            button.rect[1],
            button.rect[2],
            button.rect[3],
        );
    if button.active {
        let equip = crate::lang::get_or("horizon.wardrobe.equip", "Equip");
        let accent = (button.label == equip).then_some(GREEN);
        pill(
            batch,
            atlas,
            button.rect,
            &button.label,
            true,
            hover,
            accent,
        );
    } else {
        let [x, y, w, h] = button.rect;
        batch.fill(atlas, x, y, w, h, rgba(0, 0, 0, 170));
        let equipped = crate::lang::get_or("horizon.wardrobe.equipped", "Equipped");
        let color = if button.label == equipped {
            GREEN
        } else {
            grey()
        };
        let label = fit(atlas, &button.label, w - 8.0);
        let tw = HudBatch::text_width(atlas, &label);
        batch.text(
            atlas,
            &label,
            x + (w - tw) / 2.0,
            y + (h - 8.0) / 2.0,
            color,
            false,
        );
        crate::widgets::record(&button.label, game_rect(button.rect), false);
    }
}

/// A short line like "Equipped Founder Visor." over the picture, drawn after the player so it
/// stays readable.
pub fn draw_toast(batch: &mut HudBatch, atlas: &HudAtlas, view: &WardrobeView) {
    let Some(toast) = &view.toast else {
        return;
    };
    let size = (batch.width, batch.height);
    let [x0, _, x1, y1] = preview_rect(size);
    let toast = fit(atlas, toast, (x1 - x0).max(40.0) - 12.0);
    let tw = HudBatch::text_width(atlas, &toast);
    let bw = tw + 12.0;
    let bx = (x0 + x1) / 2.0 - bw / 2.0;
    let by = y1 - 16.0;
    batch.fill(atlas, bx, by, bw, 12.0, rgba(0, 0, 0, 160));
    batch.text(
        atlas,
        &toast,
        (x0 + x1) / 2.0 - tw / 2.0,
        by + 2.0,
        WHITE,
        false,
    );
}

/// The action under a click, if there is one.
pub fn click(size: (f32, f32), cursor: (f32, f32), view: &WardrobeView) -> Option<Action> {
    let layout = layout(size, view);
    let over = |r: [f32; 4]| ui::hovered(cursor, r[0], r[1], r[2], r[3]);
    for slot in &layout.slots {
        if over(slot.rect) {
            return Some(Action::Slot(slot.index));
        }
    }
    for item in &layout.items {
        if over(item.rect) {
            return Some(Action::Preview(view.items[item.index].id.clone()));
        }
    }
    for button in &layout.buttons {
        if button.active && over(button.rect) {
            return Some(button.action.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atlas() -> HudAtlas {
        HudAtlas::build(std::path::Path::new("/nonexistent/horizon-test-assets")).unwrap()
    }

    #[test]
    fn a_long_row_is_cut_to_its_column() {
        let atlas = atlas();
        let text = fit(&atlas, "A very long cosmetic name that does not fit", 40.0);
        assert!(text.ends_with("..."));
        assert!(HudBatch::text_width(&atlas, &text) <= 40.0);
        assert_eq!(fit(&atlas, "Hat", 40.0), "Hat");
    }

    fn view() -> WardrobeView {
        WardrobeView {
            title: "Wardrobe".into(),
            slots: (0..5)
                .map(|i| SlotRow {
                    name: format!("Slot {i}"),
                    under: "Nothing".into(),
                    chosen: i == 0,
                    count: i + 1,
                })
                .collect(),
            items: (0..12)
                .map(|i| ItemRow {
                    id: (i > 0).then(|| format!("item{i}")),
                    name: format!("Item {i}"),
                    under: String::new(),
                    worn: i == 1,
                    owned: true,
                })
                .collect(),
            visible: true,
            others: "Shown".into(),
            shop: true,
            scope: "All".into(),
            sort: "Name".into(),
            ..WardrobeView::default()
        }
    }

    #[test]
    fn clicks_pick_the_slot_item_and_buttons_they_are_drawn_on() {
        let size = (1280.0, 720.0);
        let view = view();
        let layout = layout(size, &view);
        let slot = &layout.slots[2];
        assert_eq!(
            click(size, (slot.rect[0] + 2.0, slot.rect[1] + 2.0), &view),
            Some(Action::Slot(2))
        );
        let item = &layout.items[1];
        assert_eq!(
            click(size, (item.rect[0] + 2.0, item.rect[1] + 2.0), &view),
            Some(Action::Preview(Some("item1".into())),)
        );
        let nothing = &layout.items[0];
        assert_eq!(
            click(size, (nothing.rect[0] + 2.0, nothing.rect[1] + 2.0), &view),
            Some(Action::Preview(None))
        );
        for button in &layout.buttons {
            if button.action == Action::Done {
                assert_eq!(
                    click(size, (button.rect[0] + 2.0, button.rect[1] + 2.0), &view),
                    Some(Action::Done)
                );
            }
        }
    }

    #[test]
    fn a_locked_card_is_previewed_instead_of_opening_the_shop() {
        let size = (1280.0, 720.0);
        let mut view = view();
        view.items[2].owned = false;
        let layout = layout(size, &view);
        let card = layout
            .items
            .iter()
            .find(|item| item.index == 2)
            .expect("the locked card is drawn");
        assert_eq!(
            click(size, (card.rect[0] + 2.0, card.rect[1] + 2.0), &view),
            Some(Action::Preview(Some("item2".into())))
        );
    }

    #[test]
    fn an_owned_preview_gets_an_equip_button_over_the_stage() {
        let size = (1280.0, 720.0);
        let mut view = view();
        view.preview = PreviewItem::Item(Some("item1".into()));
        view.preview_owned = true;
        view.preview_label = "Item 1".into();
        let layout = layout(size, &view);
        let equip = layout
            .buttons
            .iter()
            .find(|button| button.action == Action::Equip(Some("item1".into())))
            .expect("the equip button is laid out");
        assert!(equip.overlay);
        assert_eq!(
            click(size, (equip.rect[0] + 2.0, equip.rect[1] + 2.0), &view),
            Some(Action::Equip(Some("item1".into())))
        );
    }

    #[test]
    fn the_small_gui_keeps_every_panel_and_button_on_screen() {
        let size = (427.0, 240.0);
        let view = view();
        let mr = modal_rect(size);
        let (left, center, right, ir) = columns(size);
        assert!(left + center + right + 10.0 <= ir[2] + 1.0);
        let layout = layout(size, &view);
        for button in &layout.buttons {
            let [x, y, w, h] = button.rect;
            assert!(x >= mr[0] && x + w <= mr[0] + mr[2], "{:?}", button.rect);
            assert!(y >= mr[1] && y + h <= mr[1] + mr[3], "{:?}", button.rect);
        }
    }

    #[test]
    fn the_list_scrolls_only_as_far_as_its_last_row() {
        let size = (1280.0, 720.0);
        let mut view = view();
        let rows = item_rows(size);
        assert!(rows > 0);
        assert_eq!(
            max_scroll(size, &view),
            (12usize.saturating_sub(rows)) as f32
        );
        view.scroll = 100.0;
        assert_eq!(scroll(size, &view, 0.0), max_scroll(size, &view));
        view.scroll = 2.0;
        let page = layout(size, &view);
        // first drawn card aligned to the grid.
        assert_eq!(page.items[0].index % grid_cols(columns(size).1), 0);
        view.scroll = max_scroll(size, &view);
        let last_page = layout(size, &view);
        assert_eq!(
            last_page.items.last().map(|item| item.index),
            Some(view.items.len() - 1)
        );
    }

    #[test]
    fn the_preview_sits_between_the_two_lists() {
        let [x0, y0, x1, y1] = preview_rect((1280.0, 720.0));
        assert!(x0 > 0.0 && x1 < 1280.0 && x0 < x1);
        assert!(y0 > 0.0 && y1 < 720.0 && y0 < y1);
        let [x0, _, x1, _] = preview_rect((320.0, 240.0));
        assert!(x1 - x0 >= 48.0);
    }
}
