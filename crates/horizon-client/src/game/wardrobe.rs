//! The in-game Wardrobe, opened with Right Shift ([docs/cosmetics.md]): the same catalogue and
//! outfit as the launcher's Wardrobe page, the account's items per slot, a live preview of the
//! player wearing what is chosen, and the switches the launcher and the game's settings share.
//!
//! A change shows at once: the game's cosmetics cache is told what the player now wears
//! (`horizon_cosmetics::Cosmetics::set_worn`), so the pause-free world and the wardrobe's own
//! picture update before the service answers. The service is spoken to on a thread of its own,
//! and what couldn't be sent waits on disk (`horizon_cosmetics::wardrobe::Saved`).
use super::{Game, Screen};
use horizon_client::{
    hud::{HudBatch, WARDROBE_PREVIEW_SIZE},
    lang,
    wardrobe_screen::{self, Action, ItemRow, PreviewArt, PreviewItem, SlotRow, WardrobeView},
};
use horizon_cosmetics::{
    Item, Texture, Worn,
    prefs::{self, Others},
    preview,
    wardrobe::{Change, Error, Me, SHOP_URL, Saved, Service, Session},
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};
use winit::keyboard::KeyCode;

/// A slot row's key and English name, in the service and the draw order.
const SLOTS: [(&str, &str, &str); 5] = [
    ("head", "horizon.wardrobe.head", "Head"),
    ("back", "horizon.wardrobe.back", "Back"),
    ("torso", "horizon.wardrobe.torso", "Shirt"),
    ("shoulder", "horizon.wardrobe.shoulder", "Shoulder"),
    ("name_tag", "horizon.wardrobe.name_tag", "Name tag"),
];
/// How long a line like "Equipped Founder Visor." stays.
const TOAST: Duration = Duration::from_secs(4);

fn tr(key: &str, english: &str) -> String {
    lang::get_or(key, english)
}

fn game_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_default()
}

/// Where the wardrobe keeps each account's last answer: the game folder, beside `options.txt`.
fn wardrobe_dir() -> PathBuf {
    game_dir().join("wardrobe")
}

/// Where item files live: the same cache the game's cosmetics reader fills.
fn item_cache() -> PathBuf {
    game_dir().join("assets/cosmetics")
}

/// What the right stage shows: the current outfit or one item selected from
/// the grid. `Item(None)` is "Nothing".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Preview {
    #[default]
    Outfit,
    Item(Option<String>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Status {
    /// Never opened this run.
    #[default]
    Idle,
    Loading,
    Ready,
    Offline,
    Failed,
    SignedOut,
    Account,
}

/// Whether two changes write the same thing: one slot, or visibility.
fn same_target(a: &Change, b: &Change) -> bool {
    match (a, b) {
        (Change::Equip { slot: a, .. }, Change::Equip { slot: b, .. }) => a == b,
        (Change::Visible(_), Change::Visible(_)) => true,
        _ => false,
    }
}

/// Whether the service's `me` already shows what `change` asks for.
fn satisfied(change: &Change, me: &Me) -> bool {
    match change {
        Change::Equip { slot, item } => me.equipped.get(slot) == item.as_ref(),
        Change::Visible(on) => me.visible == *on,
    }
}

/// A trip's answer with the changes made while it was on its way applied over it: those are
/// newer than everything the service said. `answered` is whether `me` is a fresh service answer
/// (then a change it already shows is dropped) or the copy from disk (then every change waits).
fn merge_answer(local: &Saved, answered: bool, mut answer: Saved) -> Saved {
    for change in &local.pending {
        if answered && answer.me.as_ref().is_some_and(|me| satisfied(change, me)) {
            continue;
        }
        if let Some(me) = &mut answer.me {
            me.apply(change);
        }
        match answer.pending.iter_mut().find(|c| same_target(c, change)) {
            Some(waiting) => *waiting = change.clone(),
            None => answer.pending.push(change.clone()),
        }
    }
    answer
}

/// What a background trip to the service brings back.
struct Trip {
    saved: Saved,
    items: HashMap<String, Arc<Item>>,
    session: Option<Arc<Session>>,
    result: std::result::Result<(), Error>,
}

#[derive(Default)]
pub(super) struct Wardrobe {
    uuid: Option<String>,
    saved: Saved,
    items: HashMap<String, Arc<Item>>,
    session: Option<Arc<Session>>,
    status: Status,
    /// The chosen slot, an index of [`SLOTS`].
    slot: usize,
    scroll: f32,
    /// Scope filter index: All, Owned, Equipped.
    scope: usize,
    /// Sort filter index: Name, Recent, Rarity.
    sort: usize,
    /// The search text, typed while the wardrobe is open.
    search: String,
    /// CPU-rendered card previews, by item id (`none:<slot>` for the mannequin).
    previews: HashMap<String, PreviewArt>,
    /// The player's skin copied out of the atlas once per atlas revision.
    preview_skin: Option<(u64, String, Texture, bool)>,
    /// The card selected for the right stage; the outfit until a card is clicked.
    preview: Preview,
    /// A locked item's file fetched for a preview, when it was not cached.
    preview_trip: Option<Receiver<(String, Arc<Item>)>>,
    toast: Option<(String, Instant)>,
    trip: Option<Receiver<Trip>>,
    busy: bool,
    /// A failure's details, for the log rather than a translated line.
    details: Option<String>,
    /// Show the wardrobe's outfit on this window's own model, in single player too.
    local_own: bool,
    /// The cosmetics cache must be told the outfit again.
    dirty: bool,
}

impl Game<'_> {
    /// Right Shift opens the wardrobe, or closes it when it is open. Only with cosmetics on and
    /// only over the world; a Right Shift bound to a game key keeps its job.
    pub(super) fn wardrobe_toggle_key(&mut self, key: KeyCode, raw: KeyCode) -> bool {
        if !horizon_cosmetics::enabled() || raw != KeyCode::ShiftRight || key != KeyCode::ShiftRight
        {
            return false;
        }
        match self.screen {
            Screen::Playing if self.title.is_none() && self.player.is_some() => {
                self.open_wardrobe();
                true
            }
            Screen::Wardrobe => {
                self.close_wardrobe();
                true
            }
            _ => false,
        }
    }

    fn open_wardrobe(&mut self) {
        self.screen = Screen::Wardrobe;
        self.capture(false);
        self.release_use_for_keybind_screen();
        self.wardrobe.search.clear();
        self.wardrobe.scroll = 0.0;
        self.wardrobe.preview = Preview::Outfit;
        // Open on the whole catalogue, as the launcher's Wardrobe does; owned
        // items are marked and "Owned" is one click away.
        self.wardrobe.scope = 0;
        self.wardrobe_start_refresh(true);
    }

    pub(super) fn close_wardrobe(&mut self) {
        self.screen = Screen::Playing;
        self.capture(true);
    }

    /// Start (or retry) reading the catalogue and the account's answer. `catalogue` also asks
    /// the service for the catalogue; a change only needs the account's answer.
    fn wardrobe_start_refresh(&mut self, catalogue: bool) {
        if self.wardrobe.busy {
            return;
        }
        let Some(auth) = horizon_client::auth::current() else {
            self.wardrobe.uuid = None;
            self.wardrobe.status = Status::SignedOut;
            self.wardrobe.busy = false;
            return;
        };
        let uuid = format!("{:032x}", auth.uuid);
        if self.wardrobe.uuid.as_deref() != Some(uuid.as_str()) {
            self.wardrobe.saved = Saved::load(&wardrobe_dir(), &uuid);
            self.wardrobe.items.clear();
            self.wardrobe.session = None;
        }
        self.wardrobe.uuid = Some(uuid.clone());
        self.wardrobe.status = Status::Loading;
        self.wardrobe.busy = true;
        self.wardrobe.details = None;
        let trip = self.wardrobe.saved.clone();
        let session = self.wardrobe.session.clone();
        let known = self.wardrobe.items.keys().cloned().collect();
        self.wardrobe.trip = Some(spawn(uuid, auth, trip, session, catalogue, known));
        self.wardrobe.dirty = true;
        self.wardrobe_apply_local();
    }

    /// Send what waits after a change. A refresh signs in if the service's session ended.
    fn wardrobe_kick(&mut self) {
        if self.wardrobe.busy || self.wardrobe.trip.is_some() {
            return;
        }
        let Some(uuid) = self.wardrobe.uuid.clone() else {
            return;
        };
        let Some(auth) = horizon_client::auth::current() else {
            self.wardrobe.status = Status::SignedOut;
            return;
        };
        self.wardrobe.busy = true;
        self.wardrobe.details = None;
        let saved = self.wardrobe.saved.clone();
        let session = self.wardrobe.session.clone();
        let known = self.wardrobe.items.keys().cloned().collect();
        self.wardrobe.trip = Some(spawn(uuid, auth, saved, session, false, known));
    }

    /// Take a finished background trip, if one came.
    pub(super) fn tick_wardrobe(&mut self) {
        let fetched = self.wardrobe.preview_trip.as_ref().map(Receiver::try_recv);
        match fetched {
            Some(Ok((id, item))) => {
                self.wardrobe.preview_trip = None;
                self.wardrobe.items.insert(id.clone(), item);
                self.wardrobe.previews.remove(&id);
            }
            Some(Err(TryRecvError::Disconnected)) => self.wardrobe.preview_trip = None,
            Some(Err(TryRecvError::Empty)) | None => {}
        }
        let received = self.wardrobe.trip.as_ref().map(Receiver::try_recv);
        match received {
            Some(Ok(trip)) => {
                self.wardrobe.trip = None;
                self.wardrobe_apply_trip(trip);
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.wardrobe.trip = None;
                self.wardrobe.busy = false;
                self.wardrobe.status = Status::Failed;
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }
    }

    fn wardrobe_apply_trip(&mut self, trip: Trip) {
        // A change made while the trip was on its way is newer than everything it brings back:
        // keep it, apply it over the answer, and send it next.
        let local = self.wardrobe.saved.clone();
        let answered = trip.result.is_ok();
        let w = &mut self.wardrobe;
        w.busy = false;
        w.session = trip.session;
        w.saved = merge_answer(&local, answered, trip.saved);
        w.items.extend(trip.items);
        match trip.result {
            Ok(()) => {
                w.status = Status::Ready;
                w.details = None;
            }
            Err(Error::Offline) => w.status = Status::Offline,
            Err(Error::SignedOut) => w.status = Status::SignedOut,
            Err(Error::Account(e)) => {
                w.status = Status::Account;
                w.details = Some(e);
            }
            Err(e) => {
                w.status = Status::Failed;
                w.details = Some(format!("{e:?}"));
            }
        }
        if let Some(details) = &self.wardrobe.details {
            log::warn!("Wardrobe: {details}");
        }
        if let Some(uuid) = self.wardrobe.uuid.clone()
            && let Err(e) = self.wardrobe.saved.save(&wardrobe_dir(), &uuid)
        {
            log::warn!("Wardrobe: the outfit is not saved: {e}");
        }
        self.wardrobe_tell_the_game();
        self.wardrobe.dirty = true;
        self.wardrobe_apply_local();
        if answered && !self.wardrobe.saved.pending.is_empty() {
            self.wardrobe_kick();
        }
    }

    /// Keep what the game's other cosmetics switches read in step with the service's answer:
    /// ownership, what "Show my cosmetics" was confirmed to, and the wish itself.
    fn wardrobe_tell_the_game(&mut self) {
        let (Some(uuid), Some(me)) = (self.wardrobe.uuid.clone(), self.wardrobe.saved.me.as_ref())
        else {
            return;
        };
        let home = game_dir();
        if let Err(e) = prefs::set_owned(&home, &uuid, !me.owned.is_empty()) {
            log::warn!("Wardrobe: ownership is not saved: {e}");
        }
        let visible_pending = self
            .wardrobe
            .saved
            .pending
            .iter()
            .any(|c| matches!(c, Change::Visible(_)));
        if visible_pending {
            return;
        }
        // The game's own switch was changed while this answer was on its way: its wish wins
        // until the service confirms that one.
        if prefs::unsent(&home, &uuid).is_some() {
            return;
        }
        if let Err(e) = prefs::set_confirmed(&home, &uuid, me.visible) {
            log::warn!("Wardrobe: the confirmation is not saved: {e}");
        }
        if self.settings.extras.cosmetics_mine != me.visible {
            self.settings.extras.cosmetics_mine = me.visible;
            self.settings.extras.save(&home);
        }
    }

    /// Put the account's outfit into the cosmetics cache now, for this window's own model and
    /// for the wardrobe's picture, before the service answers.
    pub(super) fn wardrobe_apply_local(&mut self) {
        if !horizon_cosmetics::enabled() || !self.wardrobe.dirty {
            return;
        }
        let Some(uuid) = self.wardrobe.uuid.clone() else {
            self.wardrobe.dirty = false;
            return;
        };
        let Some(me) = self.wardrobe.saved.me.as_ref() else {
            self.wardrobe.dirty = false;
            return;
        };
        let mut items: Vec<Arc<Item>> = Vec::new();
        for (slot, ..) in SLOTS {
            if let Some(id) = me.equipped.get(slot)
                && let Some(item) = self.wardrobe.items.get(id)
            {
                items.push(item.clone());
            }
        }
        // The outfit is known but its files haven't arrived (yet): keep what was shown rather
        // than hide the player's cosmetics for a frame.
        if items.is_empty() && !me.equipped.is_empty() {
            self.wardrobe.dirty = false;
            self.wardrobe.local_own = true;
            return;
        }
        let worn: Option<Worn> = if items.is_empty() {
            None
        } else {
            Some(items.into())
        };
        let account = u128::from_str_radix(&uuid, 16).ok();
        let profile = self.remote.as_ref().map(|r| r.profile());
        // No cosmetics client yet: this runs again from `cosmetics_frame` once it exists.
        let Some(client) = &self.cosmetics.client else {
            return;
        };
        if let Some(account) = account {
            client.set_worn(account, worn.clone());
        }
        if let Some(profile) = profile
            && Some(profile) != account
        {
            client.set_worn(profile, worn);
        }
        self.wardrobe.local_own = true;
        self.wardrobe.dirty = false;
    }

    /// The UUID the service knows this window's own model by, when the wardrobe showed it in
    /// single player where the server's offline profile is all the game has.
    pub(super) fn wardrobe_own_uuid(&self) -> Option<u128> {
        if self.wardrobe.local_own {
            horizon_client::auth::current().map(|s| s.uuid)
        } else {
            None
        }
    }

    /// The player's own 64×64 skin from the atlas, so a card preview is the
    /// real character wearing the item rather than a generic mannequin. The
    /// atlas is copied once per revision, not once per frame.
    fn wardrobe_preview_skin(&mut self) -> (Texture, bool) {
        let Some(model) = self.player_model.as_ref() else {
            return (neutral_skin(), false);
        };
        let slim = model.slim;
        let default_name = if slim {
            "entity/player/slim/steve"
        } else {
            "entity/player/wide/steve"
        };
        // Downloaded skins live in GPU-only atlas slots, so their CPU copy is
        // blank. The resource pack's default skin is visible and makes a clean
        // mannequin; the live preview on the right still uses the real skin.
        let name = match model.skin.as_deref() {
            Some(name) if !name.starts_with("horizon/skin") => name,
            _ => default_name,
        };
        let revision = self.gui.atlas.revision;
        if let Some((cached_revision, cached_name, texture, cached_slim)) =
            &self.wardrobe.preview_skin
            && *cached_revision == revision
            && cached_name == name
        {
            return (texture.clone(), *cached_slim);
        }
        let atlas = &self.gui.atlas;
        let name = if atlas.sprites.contains_key(name) {
            name
        } else {
            default_name
        };
        let Some(rect) = atlas.sprites.get(name) else {
            let skin = neutral_skin();
            self.wardrobe.preview_skin = Some((revision, name.to_owned(), skin.clone(), slim));
            return (skin, slim);
        };
        let pixels = atlas.pixels();
        let x0 = (rect.u0 * atlas.width as f32).round() as u32;
        let y0 = (rect.v0 * atlas.height as f32).round() as u32;
        let (texel_w, texel_h) = rect.texels((atlas.width, atlas.height));
        let copy_w = texel_w.min(64);
        let copy_h = texel_h.min(64);
        let mut rgba = vec![0; 64 * 64 * 4];
        for y in 0..copy_h {
            let from = (((y0 + y) * atlas.width + x0) * 4) as usize;
            let to = (y * 64 * 4) as usize;
            rgba[to..to + (copy_w * 4) as usize]
                .copy_from_slice(&pixels[from..from + (copy_w * 4) as usize]);
        }
        let skin = if rgba.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0) {
            Texture {
                width: 64,
                height: 64,
                rgba,
            }
        } else {
            neutral_skin()
        };
        self.wardrobe.preview_skin = Some((revision, name.to_owned(), skin.clone(), slim));
        (skin, slim)
    }

    /// What the wardrobe draws, translated.
    fn wardrobe_view(&mut self) -> WardrobeView {
        let (preview_skin, preview_slim) = self.wardrobe_preview_skin();
        let w = &self.wardrobe;
        let code = lang::code();
        let me = w.saved.me.as_ref();
        let catalogue = w.saved.catalogue.as_ref();
        let entries = catalogue.map_or(&[][..], |c| &c.items);
        let name_of = |id: &str| {
            entries
                .iter()
                .find(|e| e.id == id)
                .map_or_else(|| id.to_owned(), |e| e.name(&code).to_owned())
        };
        let owns = |slot: &str| {
            me.is_some_and(|me| {
                entries
                    .iter()
                    .any(|e| e.slot == slot && me.owned.contains(&e.id))
            })
        };
        let equipped = |slot: &str| me.and_then(|me| me.equipped.get(slot));
        let slots: Vec<SlotRow> = SLOTS
            .iter()
            .enumerate()
            .map(|(i, (slot, key, english))| SlotRow {
                name: tr(key, english),
                under: match equipped(slot) {
                    Some(id) => name_of(id),
                    None if owns(slot) => {
                        tr("horizon.wardrobe.nothing_equipped", "Nothing equipped")
                    }
                    None => tr("horizon.wardrobe.none_owned", "None owned yet"),
                },
                chosen: i == w.slot,
                count: entries
                    .iter()
                    .filter(|e| e.slot == *slot && me.is_some_and(|me| me.owned.contains(&e.id)))
                    .count(),
            })
            .collect();
        let (slot, ..) = SLOTS[w.slot.min(SLOTS.len() - 1)];
        let mut items = Vec::new();
        let worn = equipped(slot).cloned();
        if me.is_some() {
            let query = w.search.trim().to_lowercase();
            // "Nothing" is always available, so an equipped item can be taken off.
            if query.is_empty() {
                items.push(ItemRow {
                    id: None,
                    name: tr("horizon.wardrobe.nothing", "Nothing"),
                    under: tr("horizon.wardrobe.take_off", "Take it off"),
                    worn: worn.is_none(),
                    owned: true,
                });
            }
            if let Some(me) = me {
                let founder = &me.claim.items;
                let mut shown: Vec<&horizon_cosmetics::wardrobe::Entry> =
                    entries.iter().filter(|e| e.slot == slot).collect();
                // Scope filter: all items, only owned, or the equipped one.
                if !w.scope.is_multiple_of(3) {
                    shown.retain(|e| me.owned.contains(&e.id));
                }
                if w.scope % 3 == 2 {
                    shown.retain(|e| worn.as_deref() == Some(e.id.as_str()));
                }
                // Search filter.
                if !query.is_empty() {
                    shown.retain(|e| e.name(&code).to_lowercase().contains(&query));
                }
                // Sort filter.
                match w.sort % 3 {
                    1 => shown.reverse(),
                    // The founder set is the rarest this client knows about.
                    2 => shown.sort_by(|a, b| {
                        founder
                            .contains(&b.id)
                            .cmp(&founder.contains(&a.id))
                            .then_with(|| a.name(&code).cmp(b.name(&code)))
                    }),
                    _ => shown.sort_by(|a, b| a.name(&code).cmp(b.name(&code))),
                }
                for entry in shown {
                    let on = worn.as_deref() == Some(entry.id.as_str());
                    let owned = me.owned.contains(&entry.id);
                    items.push(ItemRow {
                        id: Some(entry.id.clone()),
                        name: entry.name(&code).to_owned(),
                        under: if !owned {
                            tr("horizon.wardrobe.locked", "Locked")
                        } else if on {
                            tr("horizon.wardrobe.equipped", "Equipped")
                        } else if founder.contains(&entry.id) {
                            tr("horizon.wardrobe.founder_set", "Founder set")
                        } else {
                            tr("horizon.wardrobe.yours", "Yours")
                        },
                        worn: on,
                        owned,
                    });
                }
            }
        }
        let mut status = match w.status {
            Status::Idle => String::new(),
            Status::Loading => tr("horizon.wardrobe.loading", "Loading your Wardrobe..."),
            Status::Ready => String::new(),
            Status::Offline => tr(
                "horizon.wardrobe.offline",
                "Offline: changes stay here and sync later.",
            ),
            Status::Failed => tr(
                "horizon.wardrobe.failed",
                "The cosmetics service didn't answer. Try again.",
            ),
            Status::SignedOut => tr("horizon.wardrobe.signed_out", "Sign in via launcher."),
            Status::Account => tr(
                "horizon.wardrobe.account",
                "Account check failed. Try again.",
            ),
        };
        if !w.saved.pending.is_empty() {
            let waiting = tr("horizon.wardrobe.pending", "Waiting to sync: %s").replacen(
                "%s",
                &w.saved.pending.len().to_string(),
                1,
            );
            status = if status.is_empty() {
                waiting
            } else {
                format!("{status} {waiting}")
            };
        }
        let (preview, preview_owned, preview_equipped, preview_label) = match &w.preview {
            Preview::Outfit => (
                PreviewItem::Outfit,
                true,
                false,
                tr("horizon.wardrobe.current_outfit", "Current outfit"),
            ),
            Preview::Item(id) => {
                let owned = match id.as_deref() {
                    Some(id) => me.is_some_and(|me| me.owned.iter().any(|owned| owned == id)),
                    None => true,
                };
                let on = equipped(slot) == id.as_ref();
                let label = id
                    .as_deref()
                    .map_or_else(|| tr("horizon.wardrobe.nothing", "Nothing"), name_of);
                (PreviewItem::Item(id.clone()), owned, on, label)
            }
        };
        let mut view = WardrobeView {
            title: tr("horizon.wardrobe.title", "Wardrobe"),
            status,
            status_warn: matches!(
                w.status,
                Status::Offline | Status::Failed | Status::SignedOut | Status::Account
            ),
            slots,
            items,
            visible: self.settings.extras.cosmetics_mine,
            others: match self.settings.extras.effective_others() {
                Others::Shown => tr("horizon.wardrobe.shown", "Shown"),
                Others::NameTags => tr("horizon.wardrobe.name_tags", "Name tags only"),
                Others::Hidden => tr("horizon.wardrobe.hidden", "Hidden"),
            },
            others_on: self.settings.extras.effective_others() != Others::Hidden,
            others_names: self.settings.extras.effective_others() == Others::NameTags,
            auras: self.settings.extras.cosmetics_auras,
            toast: w
                .toast
                .as_ref()
                .filter(|(_, at)| at.elapsed() < TOAST)
                .map(|(s, _)| s.clone()),
            claim: me.is_some_and(|me| me.claim.here()),
            retry: matches!(w.status, Status::Offline | Status::Failed | Status::Account),
            shop: true,
            busy: w.busy,
            scroll: w.scroll,
            scope: match w.scope % 3 {
                0 => tr("horizon.wardrobe.scope_all", "All"),
                1 => tr("horizon.wardrobe.scope_owned", "Owned"),
                _ => tr("horizon.wardrobe.scope_equipped", "Equip"),
            },
            sort: match w.sort % 3 {
                0 => tr("horizon.wardrobe.sort_name", "A-Z"),
                1 => tr("horizon.wardrobe.sort_recent", "New"),
                _ => tr("horizon.wardrobe.sort_rarity", "Rare"),
            },
            search: w.search.clone(),
            slot: slot.to_owned(),
            previews: HashMap::new(),
            preview,
            preview_owned,
            preview_equipped,
            preview_label,
        };
        // A card shows the item itself on the neutral mannequin. The same
        // item is rendered once and kept until the wardrobe is closed.
        for (index, row) in view.items.iter().enumerate() {
            let known = row
                .id
                .as_ref()
                .filter(|id| self.wardrobe.items.contains_key(*id));
            let key = known.cloned().unwrap_or_else(|| format!("none:{slot}"));
            let art = if let Some(art) = self.wardrobe.previews.get(&key) {
                art.clone()
            } else {
                let item =
                    known.and_then(|id| self.wardrobe.items.get(id).map(|item| item.as_ref()));
                let art = render_preview(item, slot, &preview_skin, preview_slim);
                self.wardrobe.previews.insert(key, art.clone());
                art
            };
            view.previews.insert(index, art);
        }
        view.scroll = view
            .scroll
            .min(wardrobe_screen::max_scroll(self.wardrobe_gui_size(), &view));
        view
    }

    /// The locker draws at half the game's GUI scale (twice the pixel grid),
    /// so it reads fine and calm where the vanilla HUD is chunky. The zoom is
    /// the ratio between the two scales: how much bigger the locker's design
    /// space is than the game's GUI. 1 when the GUI scale is already small.
    fn wardrobe_zoom(&self) -> f32 {
        let size = self
            .renderer
            .as_ref()
            .map_or([1280, 720], |r| [r.config.width, r.config.height]);
        let scale = HudBatch::new(size).scale;
        if scale >= 2.0 { 2.0 } else { 1.0 }
    }

    /// The GUI size the locker draws in: the window at half the game's GUI
    /// scale, exactly what the frame's batch carries while the wardrobe is
    /// open.
    fn wardrobe_gui_size(&self) -> (f32, f32) {
        let size = self
            .renderer
            .as_ref()
            .map_or([1280, 720], |r| [r.config.width, r.config.height]);
        let want = (HudBatch::new(size).scale / 2.0).max(1.0);
        (
            (size[0] as f32 / want).ceil(),
            (size[1] as f32 / want).ceil(),
        )
    }

    pub(super) fn draw_wardrobe_screen(&mut self, batch: &mut HudBatch) {
        self.tick_wardrobe();
        self.flush_skin_slots();
        let view = self.wardrobe_view();
        horizon_client::widgets::begin_screen(|| {
            horizon_client::widgets::ScreenInfo::new("wardrobe", view.title.clone())
        });
        let zoom = self.wardrobe_zoom();
        // Widget bounds go back to the game's GUI pixels for horizon-ctl.
        wardrobe_screen::set_game_scale(zoom);
        let cursor = (self.cursor.0 * zoom, self.cursor.1 * zoom);
        wardrobe_screen::draw(batch, &self.gui.atlas, cursor, &view, true);
        // The player, wearing what is chosen, in the middle.
        let size = (batch.width, batch.height);
        let [x0, y0, x1, y1] = wardrobe_screen::preview_rect(size);
        let picture = ((x1 - x0).min((y1 - y0) / 2.15)).clamp(24.0, 150.0 * zoom);
        let mine = self.settings.extras.cosmetics_mine;
        // The picture shows the outfit even while "Show my cosmetics" hides it in the world,
        // as the launcher's Wardrobe does.
        self.settings.extras.cosmetics_mine = true;
        let preview_worn = self.wardrobe_preview_worn();
        self.draw_player_preview_worn(
            batch,
            x0,
            y0,
            x1,
            y1,
            picture,
            0.0625,
            preview_worn.as_ref(),
        );
        self.settings.extras.cosmetics_mine = mine;
        wardrobe_screen::draw_preview_actions(batch, &self.gui.atlas, cursor, &view);
        wardrobe_screen::draw_toast(batch, &self.gui.atlas, &view);
        wardrobe_screen::set_game_scale(1.0);
    }

    pub(super) fn wardrobe_click(&mut self) {
        let view = self.wardrobe_view();
        let zoom = self.wardrobe_zoom();
        let cursor = (self.cursor.0 * zoom, self.cursor.1 * zoom);
        let Some(action) = wardrobe_screen::click(self.wardrobe_gui_size(), cursor, &view) else {
            return;
        };
        self.audio.play_button_click();
        match action {
            Action::Slot(slot) => {
                self.wardrobe.slot = slot;
                self.wardrobe.scroll = 0.0;
                self.wardrobe.preview = Preview::Outfit;
            }
            Action::Preview(id) => {
                self.wardrobe.preview = Preview::Item(id.clone());
                self.wardrobe_preview_fetch(id.as_deref());
            }
            Action::Equip(id) => {
                let (slot, ..) = SLOTS[self.wardrobe.slot.min(SLOTS.len() - 1)];
                let said = match &id {
                    Some(id) => tr("horizon.wardrobe.equipped_toast", "Equipped %s.").replacen(
                        "%s",
                        &self.wardrobe_name(id),
                        1,
                    ),
                    None => tr("horizon.wardrobe.removed_toast", "Taken off."),
                };
                self.wardrobe_change(
                    Change::Equip {
                        slot: slot.to_owned(),
                        item: id,
                    },
                    said,
                );
            }
            Action::Visible => {
                let on = !self.settings.extras.cosmetics_mine;
                let said = if on {
                    tr(
                        "horizon.wardrobe.visible_on",
                        "Other players see your cosmetics again.",
                    )
                } else {
                    tr(
                        "horizon.wardrobe.visible_off",
                        "Other players see you without your cosmetics.",
                    )
                };
                self.settings.extras.cosmetics_mine = on;
                self.save_extras();
                self.wardrobe_change(Change::Visible(on), said);
            }
            Action::Others => {
                let next = self.settings.extras.effective_others().next();
                self.settings.extras.cosmetics_others = Some(next);
                self.save_extras();
            }
            Action::Auras => {
                self.settings.extras.cosmetics_auras = !self.settings.extras.cosmetics_auras;
                self.save_extras();
            }
            Action::Scope => {
                self.wardrobe.scope = (self.wardrobe.scope + 1) % 3;
                self.wardrobe.scroll = 0.0;
            }
            Action::Sort => {
                self.wardrobe.sort = (self.wardrobe.sort + 1) % 3;
                self.wardrobe.scroll = 0.0;
            }
            Action::Claim => self.wardrobe_claim(),
            Action::Shop => super::packs::open_uri(SHOP_URL),
            Action::Retry => self.wardrobe_start_refresh(true),
            Action::Done => self.close_wardrobe(),
        }
    }

    pub(super) fn wardrobe_scroll(&mut self, delta: f32) {
        let view = self.wardrobe_view();
        self.wardrobe.scroll = wardrobe_screen::scroll(self.wardrobe_gui_size(), &view, delta);
    }

    /// A locked item's file is not in the cache yet: fetch it for the preview,
    /// without sending any outfit change to the service.
    fn wardrobe_preview_fetch(&mut self, id: Option<&str>) {
        let Some(id) = id else {
            return;
        };
        if self.wardrobe.items.contains_key(id) {
            return;
        }
        let Some(entry) = self
            .wardrobe
            .saved
            .catalogue
            .as_ref()
            .and_then(|catalogue| catalogue.items.iter().find(|entry| entry.id == id))
            .cloned()
        else {
            return;
        };
        let (tx, rx) = channel();
        self.wardrobe.preview_trip = Some(rx);
        let cache = item_cache();
        let id = id.to_owned();
        let _ = std::thread::Builder::new()
            .name("Wardrobe preview".into())
            .spawn(move || {
                let service = Service::live();
                if let Ok(item) = service.item_cached(&entry, &cache) {
                    let _ = tx.send((id, Arc::new(item)));
                }
            });
    }

    /// The outfit the right stage draws: what is equipped, with the previewed
    /// card swapped into its slot (or taken out for "Nothing").
    fn wardrobe_preview_worn(&self) -> Option<Worn> {
        let me = self.wardrobe.saved.me.as_ref()?;
        let (slot, ..) = SLOTS[self.wardrobe.slot.min(SLOTS.len() - 1)];
        let mut equipped = me.equipped.clone();
        match &self.wardrobe.preview {
            Preview::Outfit => {}
            Preview::Item(id) => match id {
                Some(id) => {
                    equipped.insert(slot.to_owned(), id.clone());
                }
                None => {
                    equipped.remove(slot);
                }
            },
        }
        let mut items: Vec<Arc<Item>> = Vec::new();
        for (slot, ..) in SLOTS {
            if let Some(id) = equipped.get(slot)
                && let Some(item) = self.wardrobe.items.get(id)
            {
                items.push(item.clone());
            }
        }
        Some(Arc::from(items))
    }

    /// The search box takes the characters a key typed, as other screens' boxes do.
    pub(super) fn wardrobe_type(&mut self, text: Option<&str>) {
        let Some(text) = text else {
            return;
        };
        let room = 40usize.saturating_sub(self.wardrobe.search.chars().count());
        if room == 0 {
            return;
        }
        self.wardrobe
            .search
            .extend(text.chars().filter(|c| !c.is_control()).take(room));
        self.wardrobe.scroll = 0.0;
    }

    /// Backspace in the search box.
    pub(super) fn wardrobe_backspace(&mut self) {
        if self.wardrobe.search.pop().is_some() {
            self.wardrobe.scroll = 0.0;
        }
    }

    fn wardrobe_name(&self, id: &str) -> String {
        let code = lang::code();
        self.wardrobe
            .saved
            .catalogue
            .as_ref()
            .and_then(|c| c.items.iter().find(|e| e.id == id))
            .map_or_else(|| id.to_owned(), |e| e.name(&code).to_owned())
    }

    fn save_extras(&self) {
        self.settings.extras.save(&game_dir());
    }

    /// Wear `id` in `slot`, or nothing there, at once here and on disk, then at the service.
    fn wardrobe_change(&mut self, change: Change, said: String) {
        let Some(uuid) = self.wardrobe.uuid.clone() else {
            return;
        };
        self.wardrobe.saved.change(change);
        if let Err(e) = self.wardrobe.saved.save(&wardrobe_dir(), &uuid) {
            log::warn!("Wardrobe: the outfit is not saved: {e}");
        }
        self.wardrobe.toast = Some((said, Instant::now()));
        self.wardrobe.dirty = true;
        self.wardrobe_apply_local();
        self.wardrobe_kick();
    }

    fn wardrobe_claim(&mut self) {
        if self.wardrobe.busy {
            return;
        }
        let Some(uuid) = self.wardrobe.uuid.clone() else {
            return;
        };
        let Some(auth) = horizon_client::auth::current() else {
            self.wardrobe.status = Status::SignedOut;
            return;
        };
        self.wardrobe.busy = true;
        let saved = self.wardrobe.saved.clone();
        let session = self.wardrobe.session.clone();
        let known: Vec<String> = self.wardrobe.items.keys().cloned().collect();
        let (tx, rx) = channel();
        self.wardrobe.trip = Some(rx);
        let result = std::thread::Builder::new()
            .name("Wardrobe".into())
            .spawn(move || {
                let service = Service::live();
                let mut trip = Trip {
                    saved,
                    items: HashMap::new(),
                    session,
                    result: Ok(()),
                };
                trip.result = claim(&service, &auth, &uuid, &mut trip, &known);
                let _ = tx.send(trip);
            });
        if result.is_err() {
            self.wardrobe.trip = None;
            self.wardrobe.busy = false;
        }
    }
}

/// An item on the wardrobe card's neutral mannequin, rendered on the CPU once.
/// Head wear and name tags are cropped to the upper body so they stay readable
/// in a small card; back wear keeps the whole body.
fn render_preview(item: Option<&Item>, slot: &str, skin: &Texture, slim: bool) -> PreviewArt {
    let body = preview::player_model(slim);
    let mut draws = vec![preview::Draw {
        root: &body,
        base: glam::Mat4::IDENTITY,
        texture: skin,
        poses: Vec::new(),
    }];
    if let Some(item) = item {
        draws.extend(preview::Draw::item(item, &body, 0.0));
    }
    // Render at twice the card size, then take the part that matters.
    let source = WARDROBE_PREVIEW_SIZE * 2;
    let image = preview::render(&draws, source, source, 24.0, 8.0);
    let (crop_x, crop_y, crop) = if item.is_none() {
        (0, 0, source)
    } else {
        match slot {
            "head" | "name_tag" => (source / 4, 0, source / 2),
            "shoulder" => (source / 4, source / 12, source / 2),
            "torso" => (source / 8, source / 6, source * 2 / 3),
            _ => (0, 0, source),
        }
    };
    let mut rgba = resize_crop(&image, crop_x, crop_y, crop);
    if let Some(item) = item
        && item.slot == horizon_cosmetics::Slot::NameTag
        && let Some(tag) = &item.tag
    {
        tag_line(&mut rgba, tag.line);
    }
    PreviewArt {
        width: WARDROBE_PREVIEW_SIZE,
        height: WARDROBE_PREVIEW_SIZE,
        rgba: Arc::new(rgba),
    }
}

/// The `size`-wide square at (`x`, `y`) scaled to the card's preview size.
fn resize_crop(image: &preview::Image, x: u32, y: u32, size: u32) -> Vec<u8> {
    let out = WARDROBE_PREVIEW_SIZE;
    let mut rgba = vec![0; (out * out * 4) as usize];
    for oy in 0..out {
        for ox in 0..out {
            let sx = (x + ox * size / out).min(image.width - 1);
            let sy = (y + oy * size / out).min(image.height - 1);
            let from = ((sy * image.width + sx) * 4) as usize;
            let to = ((oy * out + ox) * 4) as usize;
            rgba[to..to + 4].copy_from_slice(&image.rgba[from..from + 4]);
        }
    }
    rgba
}

/// A name tag has no model: show its line under the mannequin's head instead.
fn tag_line(rgba: &mut [u8], color: [u8; 3]) {
    let out = WARDROBE_PREVIEW_SIZE;
    let (y, x0, x1) = (out * 3 / 4, out / 5, out * 4 / 5);
    for yy in y..(y + 2).min(out) {
        for xx in x0..x1 {
            let i = ((yy * out + xx) * 4) as usize;
            rgba[i..i + 3].copy_from_slice(&color);
            rgba[i + 3] = 255;
        }
    }
}

/// The flat grey 64×64 skin the card previews wear, so the cosmetic is the
/// only coloured thing in the tile.
fn neutral_skin() -> Texture {
    let mut rgba = vec![0; 64 * 64 * 4];
    let mut fill = |x0: usize, x1: usize, y0: usize, y1: usize, color: [u8; 4]| {
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * 64 + x) * 4;
                rgba[i..][..4].copy_from_slice(&color);
            }
        }
    };
    let skin = [198, 154, 122, 255];
    let shirt = [54, 122, 144, 255];
    let sleeve = [68, 136, 158, 255];
    let pants = [48, 60, 92, 255];
    let shoe = [36, 38, 44, 255];
    let eye = [44, 34, 30, 255];
    // Head, body, arms and legs in the 64x64 player-skin layout.
    fill(0, 32, 0, 16, skin);
    fill(16, 40, 16, 32, shirt);
    fill(40, 56, 16, 32, sleeve);
    fill(32, 48, 48, 64, sleeve);
    fill(0, 16, 16, 32, pants);
    fill(16, 32, 48, 64, pants);
    fill(0, 16, 28, 32, shoe);
    fill(16, 32, 60, 64, shoe);
    // A simple face on the front of the head.
    fill(9, 11, 12, 14, eye);
    fill(13, 15, 12, 14, eye);
    fill(11, 13, 14, 15, eye);
    Texture {
        width: 64,
        height: 64,
        rgba,
    }
}

/// A trip's thread: ask for the catalogue (when `catalogue`), send what waits, read the
/// account's answer and fetch the files of what it owns and wears.
fn spawn(
    uuid: String,
    auth: Arc<horizon_client::auth::AuthSession>,
    saved: Saved,
    session: Option<Arc<Session>>,
    catalogue: bool,
    known: Vec<String>,
) -> Receiver<Trip> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new()
        .name("Wardrobe".into())
        .spawn(move || {
            let service = Service::live();
            let mut trip = Trip {
                saved,
                items: HashMap::new(),
                session,
                result: Ok(()),
            };
            if catalogue {
                match service.catalogue() {
                    Ok(catalogue) => trip.saved.catalogue = Some(catalogue),
                    Err(e) => {
                        trip.result = Err(e);
                    }
                }
            }
            if trip.result.is_ok() {
                trip.result = service.sync_with_session(
                    &auth.name,
                    &uuid,
                    &mut trip.session,
                    &mut trip.saved,
                    |id| horizon_client::auth::join_server(&auth, id).map_err(|e| format!("{e:?}")),
                );
            }
            trip.items = item_files(&service, &trip.saved, &known);
            let _ = tx.send(trip);
        });
    rx
}

/// Claim the founder set where the service offers it here (Paddle doesn't serve the player's
/// country): the account's answer after it, with what still waits applied on top.
fn claim(
    service: &Service,
    auth: &horizon_client::auth::AuthSession,
    uuid: &str,
    trip: &mut Trip,
    known: &[String],
) -> std::result::Result<(), Error> {
    let session = match trip.session.clone().filter(|s| s.for_account(uuid)) {
        Some(session) => session,
        None => {
            let signed = Arc::new(service.sign_in(&auth.name, |id| {
                horizon_client::auth::join_server(auth, id).map_err(|e| format!("{e:?}"))
            })?);
            trip.session = Some(signed.clone());
            signed
        }
    };
    let mut me = service.claim(&session)?;
    for change in &trip.saved.pending {
        me.apply(change);
    }
    trip.saved.me = Some(me);
    trip.items = item_files(service, &trip.saved, known);
    Ok(())
}

/// The item files of what the account owns and wears, kept or fetched. A file that doesn't read
/// is left out, never drawn.
fn item_files(service: &Service, saved: &Saved, known: &[String]) -> HashMap<String, Arc<Item>> {
    let Some(catalogue) = &saved.catalogue else {
        return HashMap::new();
    };
    let Some(me) = &saved.me else {
        return HashMap::new();
    };
    let cache = item_cache();
    catalogue
        .items
        .iter()
        .filter(|e| e.theme.is_none() && !known.contains(&e.id))
        .filter(|e| me.owned.contains(&e.id) || me.equipped.values().any(|id| id == &e.id))
        .filter_map(|e| {
            service
                .item_cached(e, &cache)
                .ok()
                .map(|item| (e.id.clone(), Arc::new(item)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_cosmetics::Slot;

    #[test]
    fn the_slot_rows_are_the_five_the_service_knows() {
        let slots: Vec<&str> = SLOTS.iter().map(|(slot, ..)| *slot).collect();
        assert_eq!(slots, ["head", "back", "torso", "shoulder", "name_tag"]);
        // Every slot's name is a translation key, never a bare English word.
        assert!(
            SLOTS
                .iter()
                .all(|(_, key, _)| key.starts_with("horizon.wardrobe."))
        );
    }

    fn wear(slot: &str, item: &str) -> Change {
        Change::Equip {
            slot: slot.into(),
            item: Some(item.into()),
        }
    }

    #[test]
    fn a_change_made_while_the_service_answers_is_not_lost() {
        let local = Saved {
            pending: vec![wear("head", "cap")],
            ..Saved::default()
        };
        // The answer was asked before the change: it shows the old hat and has nothing waiting.
        let answer = Saved {
            me: Some(Me {
                equipped: [("head".to_owned(), "hat".to_owned())]
                    .into_iter()
                    .collect(),
                ..Me::default()
            }),
            ..Saved::default()
        };
        let merged = merge_answer(&local, true, answer);
        assert_eq!(merged.pending, [wear("head", "cap")]);
        assert_eq!(
            merged.me.and_then(|me| me.equipped.get("head").cloned()),
            Some("cap".to_owned())
        );
    }

    #[test]
    fn a_change_the_answer_already_shows_is_dropped() {
        let local = Saved {
            pending: vec![wear("head", "hat")],
            ..Saved::default()
        };
        let answer = Saved {
            me: Some(Me {
                equipped: [("head".to_owned(), "hat".to_owned())]
                    .into_iter()
                    .collect(),
                ..Me::default()
            }),
            ..Saved::default()
        };
        assert!(merge_answer(&local, true, answer).pending.is_empty());
        // Without a fresh answer, what the disk said may already have the change applied:
        // it still has to be sent.
        let disk = Saved {
            me: Some(Me {
                equipped: [("head".to_owned(), "hat".to_owned())]
                    .into_iter()
                    .collect(),
                ..Me::default()
            }),
            ..Saved::default()
        };
        assert_eq!(
            merge_answer(&local, false, disk).pending,
            [wear("head", "hat")]
        );
    }

    #[test]
    fn card_previews_draw_the_mannequin_and_the_item() {
        const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAKUlEQVR4nO3OIQEAAAACIP+f1hkWWEB6FgEBAQEBAQEBAQEBAQEBgXdgl/rw4unIZ5cAAAAASUVORK5CYII=";
        let json = format!(
            r#"{{"format":1,"id":"cap","slot":"head","bone":"head","attach":{{"offset":[0,-8,0]}},"texture":{{"png":"{PNG}"}},"parts":[{{"name":"c","cubes":[{{"origin":[-4,-4,-4],"size":[8,4,8],"uv":[0,0]}}]}}]}}"#
        );
        let item = Item::parse(json.as_bytes()).unwrap();
        let skin = neutral_skin();
        let bare = render_preview(None, "head", &skin, false);
        let worn = render_preview(Some(&item), "head", &skin, false);
        let visible = |art: &PreviewArt| art.rgba.chunks(4).filter(|px| px[3] > 0).count();
        assert!(visible(&bare) > 0, "the mannequin is drawn");
        assert!(visible(&worn) > visible(&bare), "the item is drawn on it");
    }

    #[test]
    fn the_game_and_the_service_agree_on_the_slot_names() {
        // An item file's `slot` is the same key the service's owned list and the wardrobe use.
        const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        for (slot, slot_value) in [
            ("head", Slot::Head),
            ("back", Slot::Back),
            ("torso", Slot::Torso),
            ("shoulder", Slot::Shoulder),
            ("name_tag", Slot::NameTag),
        ] {
            let tag = if slot == "name_tag" {
                r##","tag":{"line":"#ffffff"}"##
            } else {
                ""
            };
            let json = format!(
                r#"{{"format":1,"id":"a","slot":"{slot}","bone":"head","texture":{{"png":"{PNG}"}},"parts":[]{tag}}}"#
            );
            let item = Item::parse(json.as_bytes()).unwrap();
            assert_eq!(item.slot, slot_value, "{slot}");
        }
    }
}
