//! The Wardrobe's link to Horizon's cosmetics service ([docs/cosmetics.md]): the catalogue,
//! the account's own items, what each slot wears and whether others see it. The launcher's
//! Wardrobe page and the game's own wardrobe screen both speak this protocol.
//!
//! The service learns who the player is the way an online-mode server does: it hands out a
//! one-time server id, the caller tells Mojang's session server that the account joins it
//! (`join`), and the service asks Mojang. The Minecraft token goes to Mojang only; the service
//! gives back a session of its own, kept in memory here.
//!
//! None of this is drawn or translated here: a caller maps [`Error`] to its own words and keeps
//! [`Saved`] wherever its platform keeps player data.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where cosmetics are bought and the founder set is claimed: the website's shop.
pub const SHOP_URL: &str = "https://playhorizon.dev/shop";

/// Why a request to the service failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// No answer: offline, or the service is down.
    Offline,
    /// Mojang didn't confirm the account (the `join` failed or was refused).
    Account(String),
    /// The service's session ended: sign in to it again.
    SignedOut,
    /// An item the account doesn't own, or one for another slot.
    NotYours,
    /// Anything else the service answered.
    Service(u16),
    /// The answer wasn't the JSON this client knows.
    Malformed,
}

impl Error {
    /// Whether the change that failed should wait and be sent again: the service wasn't
    /// reached, or failed on its side.
    pub fn retry_later(&self) -> bool {
        matches!(self, Self::Offline | Self::Service(500..) | Self::Malformed)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimState {
    #[default]
    NotYet,
    Open,
    Closed,
    /// A state this client doesn't know, from a newer service: the answer still reads, and the
    /// rest of it (`open`, the dates) decides.
    #[serde(other)]
    Unknown,
}

/// The founder set: free to claim between `starts_at` and `ends_at` (Unix seconds, `[start,
/// end)`), retired for good after.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Claim {
    #[serde(default)]
    pub state: ClaimState,
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(default)]
    pub starts_at: Option<i64>,
    #[serde(default)]
    pub ends_at: Option<i64>,
    /// Whether this account claimed it (in `me` only).
    #[serde(default)]
    pub claimed: Option<bool>,
    /// It can be claimed now: the public window, or a preview for an account on the service's
    /// list.
    #[serde(default)]
    pub open: bool,
    /// Open only because the account is on the service's preview list.
    #[serde(default)]
    pub preview: bool,
    /// The service grants the set from the Wardrobe (`POST /v1/claim`): the shop's payment
    /// provider doesn't sell where the player is (in `me` only).
    #[serde(default)]
    pub one_click: bool,
}

impl Claim {
    /// Whether the Wardrobe offers "Claim for free": open for this account, not claimed yet, and
    /// the service says the checkout isn't for this player.
    pub fn here(&self) -> bool {
        self.open && self.one_click && self.claimed == Some(false)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Entry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub hash: String,
    /// `head`, `back`, `torso`, `shoulder`, `name_tag` (the game draws them) or `theme` (the
    /// launcher's).
    #[serde(default)]
    pub slot: String,
    /// The item's name per game language code (`en_us`, `fr_fr`...).
    #[serde(default)]
    pub names: BTreeMap<String, String>,
    /// A launcher theme's colours (`slot` is `theme`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
}

/// A launcher theme: the sky's six stops, top to bottom (`#rrggbb`), and the accent's hue.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Theme {
    pub sky: [String; 6],
    pub accent_hue: f32,
}

impl Theme {
    /// The sky as `0xrrggbb`, or None if a stop isn't a colour.
    pub fn sky(&self) -> Option<[u32; 6]> {
        let mut out = [0; 6];
        for (o, c) in out.iter_mut().zip(&self.sky) {
            let hex = c.strip_prefix('#').filter(|h| h.len() == 6)?;
            *o = u32::from_str_radix(hex, 16).ok()?;
        }
        Some(out)
    }
}

impl Entry {
    /// The name in `lang` (a game language code), else English, else the id.
    pub fn name(&self, lang: &str) -> &str {
        self.names
            .get(lang)
            .or_else(|| self.names.get("en_us"))
            .map_or(&self.id, String::as_str)
    }

    /// Whether the id and hash are plain parts of a file name, by the game's rule
    /// (`horizon_cosmetics::fetch`): the cache's paths are made of them.
    pub fn plain(&self) -> bool {
        [&self.id, &self.hash].into_iter().all(|t| {
            !t.is_empty()
                && t.len() <= 64
                && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Catalogue {
    #[serde(default)]
    pub items: Vec<Entry>,
    #[serde(default)]
    pub claim: Claim,
}

/// The signed-in account's items: what it owns, what each slot wears, whether others see it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Me {
    #[serde(default)]
    pub uuid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub owned: Vec<String>,
    #[serde(default)]
    pub equipped: BTreeMap<String, String>,
    /// "Show my cosmetics": off, other players see this account without them.
    #[serde(default = "shown")]
    pub visible: bool,
    #[serde(default)]
    pub claim: Claim,
    /// What the account claimed or bought through Paddle, newest first (the purchase history
    /// Mojang's guidelines ask for).
    #[serde(default)]
    pub purchases: Vec<Purchase>,
}

/// One order, as the cosmetics service keeps it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Purchase {
    /// Paddle's transaction id (`txn_…`): the order number on Paddle's receipt.
    #[serde(default)]
    pub transaction_id: String,
    /// An item id, or `founder_set`.
    #[serde(default)]
    pub item: String,
    /// Paid, tax included, in the lowest unit of `currency`.
    #[serde(default)]
    pub amount: i64,
    #[serde(default)]
    pub currency: String,
    /// `paid`, `partly_refunded`, `refunded` or `charged_back`.
    #[serde(default)]
    pub status: String,
    /// Unix seconds.
    #[serde(default)]
    pub at: i64,
}

impl Purchase {
    /// The amount as people write it: `2.99 USD`; `None` for nothing paid.
    pub fn price(&self) -> Option<String> {
        (self.amount > 0).then(|| {
            let (whole, cents) = (self.amount / 100, self.amount % 100);
            format!("{whole}.{cents:02} {}", self.currency)
        })
    }
}

fn shown() -> bool {
    true
}

/// A change to the outfit, sent at once or, offline, kept until the service answers.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// Wear `item` in `slot`, or nothing there.
    Equip { slot: String, item: Option<String> },
    /// "Show my cosmetics".
    Visible(bool),
}

impl Me {
    /// What the outfit looks like with `change` made, before the service confirms it.
    pub fn apply(&mut self, change: &Change) {
        match change {
            Change::Equip {
                slot,
                item: Some(item),
            } => {
                self.equipped.insert(slot.clone(), item.clone());
            }
            Change::Equip { slot, item: None } => {
                self.equipped.remove(slot);
            }
            Change::Visible(on) => self.visible = *on,
        }
    }
}

/// What a Wardrobe keeps on disk per account: the last catalogue and answer, and the changes
/// still to send. No session or token is ever kept.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Saved {
    pub catalogue: Option<Catalogue>,
    pub me: Option<Me>,
    pub pending: Vec<Change>,
}

impl Saved {
    /// `<dir>/<uuid>.json`.
    pub fn path(dir: &Path, uuid: &str) -> PathBuf {
        dir.join(format!("{uuid}.json"))
    }

    pub fn load(dir: &Path, uuid: &str) -> Self {
        std::fs::read(Self::path(dir, uuid))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path, uuid: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = Self::path(dir, uuid);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(self).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(tmp, path)
    }

    /// Make `change` here: the outfit shows it at once and it waits to be sent. A newer change of
    /// the same slot (or of visibility) replaces an older one still waiting.
    pub fn change(&mut self, change: Change) {
        if let Some(me) = &mut self.me {
            me.apply(&change);
        }
        let same = |c: &Change| match (c, &change) {
            (Change::Equip { slot: a, .. }, Change::Equip { slot: b, .. }) => a == b,
            (Change::Visible(_), Change::Visible(_)) => true,
            _ => false,
        };
        self.pending.retain(|c| !same(c));
        self.pending.push(change);
    }
}

/// A session with the service: an opaque token, good for the time it said.
pub struct Session {
    token: String,
    until: Instant,
    pub uuid: String,
    pub name: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Session({}, <hidden>)", self.name)
    }
}

impl Session {
    /// A made-up session for a caller's preview: it carries no credential, so the service
    /// refuses anything sent with it.
    pub fn preview(uuid: &str, name: &str) -> Self {
        Self {
            token: String::new(),
            until: Instant::now() + Duration::from_secs(3600),
            uuid: uuid.into(),
            name: name.into(),
        }
    }

    pub fn live(&self) -> bool {
        Instant::now() < self.until
    }

    /// Whether this is the account `uuid`'s session and it has not run out.
    pub fn for_account(&self, uuid: &str) -> bool {
        self.uuid == uuid && self.live()
    }
}

/// A request to the service: `method` on `path`, with the service's session if any, answering
/// the HTTP status and the JSON body. `Http` in the launcher and the game, a fake in tests.
pub trait Call: Send + Sync {
    fn call(
        &self,
        method: &str,
        path: &str,
        session: Option<&str>,
        body: Option<&Value>,
    ) -> Result<(u16, Value), String>;
}

impl Call for crate::Http {
    fn call(
        &self,
        method: &str,
        path: &str,
        session: Option<&str>,
        body: Option<&Value>,
    ) -> Result<(u16, Value), String> {
        let url = format!("{}{path}", self.base());
        let mut request = self
            .agent()
            .request(method, &url)
            .set("Accept", "application/json");
        if let Some(session) = session {
            request = request.set("Authorization", &format!("Bearer {session}"));
        }
        let answer = match body {
            Some(body) => {
                let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
                request
                    .set("Content-Type", "application/json")
                    .send_bytes(&bytes)
            }
            None => request.call(),
        };
        let answer = match answer {
            Ok(answer) => answer,
            Err(ureq::Error::Status(status, answer)) => {
                let mut bytes = Vec::new();
                let _ = std::io::Read::read_to_end(
                    &mut std::io::Read::take(answer.into_reader(), 1 << 20),
                    &mut bytes,
                );
                let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                return Ok((status, value));
            }
            Err(ureq::Error::Transport(_)) => return Err("offline".into()),
        };
        let bytes = crate::Http::read(Ok(answer))?;
        let value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        Ok((200, value))
    }
}

pub struct Service {
    call: Box<dyn Call>,
}

impl Service {
    pub fn new(call: Box<dyn Call>) -> Self {
        Self { call }
    }

    /// The service the game asks too (`HORIZON_COSMETICS_API` overrides it).
    pub fn live() -> Self {
        let api = std::env::var("HORIZON_COSMETICS_API").unwrap_or_else(|_| crate::API.to_owned());
        let agent = format!("Horizon/{}", env!("CARGO_PKG_VERSION"));
        Self::new(Box::new(crate::Http::new(&api, &agent)))
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        session: Option<&Session>,
        body: Option<Value>,
    ) -> Result<Value, Error> {
        if let Some(session) = session
            && !session.live()
        {
            return Err(Error::SignedOut);
        }
        let token = session.map(|s| s.token.as_str());
        match self.call.call(method, path, token, body.as_ref()) {
            Ok((status, value)) if (200..300).contains(&status) => Ok(value),
            Ok((401, _)) => Err(Error::SignedOut),
            Ok((403, value)) if value["error"] == "not_owned" => Err(Error::NotYours),
            Ok((400, value)) if value["error"] == "wrong_slot" => Err(Error::NotYours),
            Ok((404, value)) if value["error"] == "item" => Err(Error::NotYours),
            Ok((status, _)) => Err(Error::Service(status)),
            Err(_) => Err(Error::Offline),
        }
    }

    /// Where the founder set's claim stands for everyone (`GET /v1/shop`, no session).
    pub fn shop(&self) -> Result<Claim, Error> {
        parse(self.call("GET", "/v1/shop", None, None)?)
    }

    pub fn catalogue(&self) -> Result<Catalogue, Error> {
        parse(self.call("GET", "/v1/catalogue", None, None)?)
    }

    /// Sign `name` in: `join` tells Mojang the account joins the server id the service chose.
    pub fn sign_in(
        &self,
        name: &str,
        join: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<Session, Error> {
        #[derive(Deserialize)]
        struct Start {
            server_id: String,
        }
        #[derive(Deserialize)]
        struct Finish {
            session: String,
            uuid: String,
            name: String,
            expires_in: u64,
        }
        let start: Start = parse(self.call("POST", "/v1/auth/start", None, Some(json!({})))?)?;
        join(&start.server_id).map_err(Error::Account)?;
        let body = json!({ "server_id": start.server_id, "name": name });
        let finish: Finish = match self.call("POST", "/v1/auth/finish", None, Some(body)) {
            Err(Error::SignedOut) => {
                return Err(Error::Account(
                    "Mojang could not confirm the account".into(),
                ));
            }
            other => parse(other?)?,
        };
        Ok(Session {
            token: finish.session,
            // A minute early, so a request never races the end.
            until: Instant::now() + Duration::from_secs(finish.expires_in.saturating_sub(60)),
            uuid: finish.uuid,
            name: finish.name,
        })
    }

    pub fn me(&self, session: &Session) -> Result<Me, Error> {
        parse(self.call("GET", "/v1/me", Some(session), None)?)
    }

    /// Claim the founder set here (only where `Claim::here`): the account's answer after it. One
    /// the service already recorded is no failure.
    pub fn claim(&self, session: &Session) -> Result<Me, Error> {
        match self.call("POST", "/v1/claim", Some(session), Some(json!({}))) {
            Err(Error::Service(409)) => self.me(session),
            other => parse(other?),
        }
    }

    /// Wear `item` in `slot`, or nothing there (`None`).
    pub fn equip(&self, session: &Session, slot: &str, item: Option<&str>) -> Result<Me, Error> {
        let body = json!({ "slot": slot, "item": item });
        parse(self.call("PUT", "/v1/equipped", Some(session), Some(body))?)
    }

    /// "Show my cosmetics": off, other players see this account without them.
    pub fn set_visible(&self, session: &Session, visible: bool) -> Result<Me, Error> {
        let body = json!({ "visible": visible });
        parse(self.call("PUT", "/v1/visible", Some(session), Some(body))?)
    }

    /// Send the changes still waiting, oldest first. Each one the service takes, or refuses for
    /// good (an item no longer owned), leaves `saved.pending`; the first that should be tried
    /// again stops the rest and is the error. `saved.me` is the service's last answer, with what
    /// still waits applied on top.
    pub fn sync(&self, session: &Session, saved: &mut Saved) -> Result<(), Error> {
        while let Some(change) = saved.pending.first().cloned() {
            let sent = match &change {
                Change::Equip { slot, item } => self.equip(session, slot, item.as_deref()),
                Change::Visible(on) => self.set_visible(session, *on),
            };
            match sent {
                Ok(me) => saved.me = Some(me),
                Err(e) if e.retry_later() || e == Error::SignedOut => return Err(e),
                Err(_) => {}
            }
            saved.pending.remove(0);
        }
        Ok(())
    }

    /// Send the changes waiting and read the account's own answer with `session`, which is kept
    /// while it is live: the account is proved with Mojang once per service session, not once
    /// per refresh. The service's 401 (its session ended) signs in once more and retries; any
    /// other failure stands.
    pub fn sync_with_session(
        &self,
        name: &str,
        uuid: &str,
        session: &mut Option<std::sync::Arc<Session>>,
        saved: &mut Saved,
        join: impl Fn(&str) -> Result<(), String>,
    ) -> Result<(), Error> {
        let mut renewed = false;
        loop {
            let current = match session.clone().filter(|s| s.for_account(uuid)) {
                Some(current) => current,
                None => {
                    let signed = std::sync::Arc::new(self.sign_in(name, &join)?);
                    *session = Some(signed.clone());
                    signed
                }
            };
            if let Err(e) = self.sync(&current, saved) {
                if e == Error::SignedOut && !renewed {
                    *session = None;
                    renewed = true;
                    continue;
                }
                return Err(e);
            }
            match self.me(&current) {
                Ok(mut me) => {
                    for change in &saved.pending {
                        me.apply(change);
                    }
                    saved.me = Some(me);
                    return Ok(());
                }
                Err(Error::SignedOut) if !renewed => {
                    *session = None;
                    renewed = true;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// An item's file, from `cache` (`<id>-<hash>.json`, kept by hash: it never changes for one)
    /// or the service, then kept there. An entry whose id or hash isn't `Entry::plain` is refused
    /// before the disk or the service is touched.
    pub fn item_cached(&self, entry: &Entry, cache: &Path) -> Result<crate::Item, Error> {
        if !entry.plain() {
            return Err(Error::Malformed);
        }
        let path = cache.join(format!("{}-{}.json", entry.id, entry.hash));
        if let Some(item) = std::fs::read(&path)
            .ok()
            .and_then(|b| crate::Item::parse(&b).ok())
        {
            return Ok(item);
        }
        let url = format!("/v1/items/{}?hash={}", entry.id, entry.hash);
        let value = self.call("GET", &url, None, None)?;
        let bytes = serde_json::to_vec(&value).map_err(|_| Error::Malformed)?;
        let item = crate::Item::parse(&bytes).map_err(|_| Error::Malformed)?;
        if std::fs::create_dir_all(cache).is_ok() {
            let _ = std::fs::write(&path, &bytes);
        }
        Ok(item)
    }
}

fn parse<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, Error> {
    serde_json::from_value(v).map_err(|_| Error::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Fake {
        log: Arc<Mutex<Vec<String>>>,
        /// `(status, body)` per path, taken in order; the last one repeats.
        answers: Arc<Mutex<Vec<(String, u16, Value)>>>,
        joined: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        fn answer(&self, method: &str, path: &str, status: u16, body: Value) {
            self.answers
                .lock()
                .unwrap()
                .push((format!("{method} {path}"), status, body));
        }
    }

    impl Call for Fake {
        fn call(
            &self,
            method: &str,
            path: &str,
            session: Option<&str>,
            body: Option<&Value>,
        ) -> Result<(u16, Value), String> {
            self.log.lock().unwrap().push(format!(
                "{method} {path} auth={} body={}",
                session.unwrap_or(""),
                body.cloned().unwrap_or(Value::Null)
            ));
            let key = format!("{method} {path}");
            let mut answers = self.answers.lock().unwrap();
            let at = answers
                .iter()
                .position(|(p, ..)| *p == key)
                .ok_or_else(|| "no answer".to_string())?;
            let (_, status, value) = answers[at].clone();
            if answers[at + 1..].iter().any(|(p, ..)| *p == key) {
                answers.remove(at);
            }
            Ok((status, value))
        }
    }

    fn me(equipped: Value) -> Value {
        json!({
            "uuid": "a", "name": "Alex", "owned": ["hat"], "equipped": equipped,
            "visible": true,
            "claim": {"state": "closed", "items": [], "claimed": true},
        })
    }

    #[test]
    fn a_newer_change_of_a_slot_replaces_the_one_waiting() {
        let mut saved = Saved {
            me: Some(Me::default()),
            ..Saved::default()
        };
        saved.change(Change::Equip {
            slot: "head".into(),
            item: Some("hat".into()),
        });
        saved.change(Change::Visible(false));
        saved.change(Change::Equip {
            slot: "head".into(),
            item: Some("cap".into()),
        });
        // The newer hat wins, and only one change of the slot waits.
        assert_eq!(
            saved.pending,
            [
                Change::Visible(false),
                Change::Equip {
                    slot: "head".into(),
                    item: Some("cap".into())
                }
            ]
        );
        assert_eq!(
            saved.me.unwrap().equipped.get("head").map(String::as_str),
            Some("cap")
        );
    }

    #[test]
    fn a_change_the_service_refuses_is_dropped_and_an_outage_stops_the_rest() {
        let fake = Fake::default();
        fake.answer("PUT", "/v1/equipped", 403, json!({"error": "not_owned"}));
        fake.answer("PUT", "/v1/visible", 503, json!({"error": "down"}));
        let service = Service::new(Box::new(fake));
        let session = Session::preview("a", "Alex");
        let mut saved = Saved {
            me: Some(Me::default()),
            pending: vec![
                Change::Equip {
                    slot: "head".into(),
                    item: Some("gone".into()),
                },
                Change::Visible(false),
            ],
            ..Saved::default()
        };
        assert_eq!(service.sync(&session, &mut saved), Err(Error::Service(503)));
        // Refused for good: dropped. Not reached: still waiting.
        assert_eq!(saved.pending, [Change::Visible(false)]);
    }

    #[test]
    fn signing_in_through_mojang_signs_in_as_the_account_that_joined() {
        let fake = Fake::default();
        fake.answer("POST", "/v1/auth/start", 200, json!({"server_id": "id-1"}));
        fake.answer(
            "POST",
            "/v1/auth/finish",
            200,
            json!({"session": "svc", "uuid": "a", "name": "Alex", "expires_in": 600}),
        );
        fake.answer("GET", "/v1/me", 200, me(json!({"head": "hat"})));
        let service = Service::new(Box::new(fake.clone()));
        let mut session = None;
        let mut saved = Saved::default();
        let done = service.sync_with_session("Alex", "a", &mut session, &mut saved, |id| {
            fake.joined.lock().unwrap().push(id.to_owned());
            Ok(())
        });
        assert_eq!(done, Ok(()));
        assert_eq!(*fake.joined.lock().unwrap(), ["id-1"]);
        assert_eq!(
            saved.me.as_ref().and_then(|m| m.equipped.get("head")),
            Some(&"hat".to_owned())
        );
        let log = fake.log.lock().unwrap();
        assert!(log.iter().any(|l| l == "GET /v1/me auth=svc body=null"));
    }

    #[test]
    fn the_outfit_survives_a_round_trip_through_disk() {
        let saved = Saved {
            catalogue: Some(Catalogue {
                items: Vec::new(),
                claim: Claim::default(),
            }),
            me: Some(Me {
                uuid: "a".into(),
                name: "Alex".into(),
                owned: vec!["hat".into()],
                equipped: [("head".to_owned(), "hat".to_owned())]
                    .into_iter()
                    .collect(),
                visible: false,
                ..Me::default()
            }),
            pending: vec![Change::Visible(true)],
        };
        let dir = std::env::temp_dir().join(format!(
            "horizon-wardrobe-saved-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        saved.save(&dir, "a").unwrap();
        assert_eq!(Saved::load(&dir, "a"), saved);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_item_file_is_read_once_and_kept_on_disk() {
        const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let item = json!({
            "format": 1, "id": "hat", "slot": "head", "bone": "head",
            "texture": {"png": PNG}, "parts": []
        });
        let fake = Fake::default();
        fake.answer("GET", "/v1/items/hat?hash=h", 200, item);
        let service = Service::new(Box::new(fake.clone()));
        let entry = Entry {
            id: "hat".into(),
            hash: "h".into(),
            slot: "head".into(),
            ..Entry::default()
        };
        let dir = std::env::temp_dir().join(format!(
            "horizon-wardrobe-items-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = service.item_cached(&entry, &dir).unwrap();
        assert_eq!(first.id, "hat");
        let asked = fake.log.lock().unwrap().len();
        // The file is on disk: the next read asks the service nothing.
        let second = service.item_cached(&entry, &dir).unwrap();
        assert_eq!(second.id, "hat");
        assert_eq!(fake.log.lock().unwrap().len(), asked);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_founder_set_is_offered_here_only_when_the_service_says_so() {
        let mut claim = Claim {
            open: true,
            one_click: true,
            claimed: Some(false),
            ..Claim::default()
        };
        assert!(claim.here());
        claim.claimed = Some(true);
        assert!(!claim.here());
        claim.claimed = Some(false);
        claim.one_click = false;
        assert!(!claim.here());
    }
}
