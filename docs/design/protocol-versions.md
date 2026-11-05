# One build, two wire protocols: 26.2 clients beside a 26.3 server

This note designs the **server-role half of `crates/horizon-protocol/src/adapter/`**: a Minecraft 26.2
client joining a Horizon server that runs 26.3, with 26.2's wire format translated at `Connection`. It
completes what #6702 started (a 26.3 Horizon client joining 26.2 servers) and leaves the simulation
itself untouched.

It is deliberately not #9881/#9883: those run the game as the older release from per-version prepared
data. Here the world, tick and packets inside Horizon stay 26.3; only the bytes on a 26.2 connection
differ, so a 26.2 client gets a 26.3 world through a 26.2 view (below).

Horizon's internal representation is 26.3. A second Minecraft version is a wire dialect, not a second
game. Most of the adapter already exists and is the reference for the missing half: the 26.2 packet and
registry tables, the layout translators, and the per-connection `Session`. What is missing is server-role
translation and a server that accepts protocol 776; the PRs at the end fill exactly that in.

## What exists

The client-role rows are the reference implementation shipped by #6702; the last two are this note's
work.

| Layer | Where | 26.2 state |
| --- | --- | --- |
| Version set, ordering, protocol numbers | `adapter.rs:27-82` (`ServerVersion`) | 26.2 = 776, listed and ordered |
| Packet id tables, both directions | `adapter/v26_2.rs` | generated, complete |
| Registry id tables | `adapter/v26_2_ids.rs` | 11 registries differ; the rest `Same` |
| Layout translators | `adapter/translate.rs` | 26.2→26.3 done for a client |
| Per-connection state machine | `adapter/session.rs` | done, client role |
| Connection hook | `connection.rs:376-381` (`adapt`) | done, client role |
| Client selection by status ping | `net.rs:568-579` | behind `HORIZON_OLDER_SERVERS=1` |
| Server accepting a 26.2 client | `lan/conn.rs:487` | **missing: compares `!= PROTOCOL_VERSION`** |
| Reverse layout translation | — | **missing: serverbound up, clientbound down** |

## How the versions coexist

**One canonical form, one boundary.** Everything above `Connection::recv` / `Connection::send` sees 26.3
packets and 26.3 ids. `Connection` is the only place a version exists.

**Chosen once per connection.** `ServerVersion` is resolved before the handshake: the client reads the
server's status ping (`net.rs:570-578`), the server reads the handshake's VarInt
(`lan/conn.rs:470-502`). `Connection::adapt` stores `Option<Arc<Session>>` and never consults a version
again on the fast path. The enum is `Copy`, five variants, no allocation.

**Packet ids are static arrays, not a map.** For each (state, direction) an older version has a
`&'static [Option<u8>]` indexed by the id it sends; `None` means the other side has no such packet, so
it is dropped rather than guessed (`adapter.rs:172-186`). 26.3 gained three clientbound play packets
(`add_transient_block` 37, `post_effects` 83, `swing_animation` 123), `post_effects` in configuration
(10), and replaced serverbound `swing` with `punch` (46); 131 others renumbered. Nothing about the 26.3
tables changes to describe 26.2.

**Registry ids are runs, not tables of entries.** `Remap::Runs` holds `(from, to, length)` triples and
looks up by binary search (`adapter/ids.rs:20-43`). 26.2 differs in block states, items, entity types,
particle types, sound events, data component types, command argument types, custom stats, map
decorations, blocks and recipe serializers; the other registries map to themselves as a `Same` variant
with no data. Data component types are the sharp edge: 26.3 added `cooking_fuel`, `compostable` and the
`sign_text_*` family, and split `swing_animation` into `attack_animation` and `interact_animation`, which
is +13 −2. Components 26.2 has and 26.3 lacks are listed in `dropped_components`; their readers step over
the value and discard it (`adapter/ids.rs:184-193`).

**Registry ids are renumbered while decoding, not afterwards.** Some ids choose the layout of the bytes
that follow (item stacks, components, particles), so a post-pass copy is impossible. `ids::renumbering`
arms a thread-local `Cell<Option<(&Ids, Way)>>` for the duration of one decode
(`adapter/ids.rs:146-162`); the shared readers consult it through `item`, `component` and `particle`.
`Way::Up` is "the peer is older", `Way::Down` is "the peer is older and we are encoding for it". The
same mechanism serves both roles.

**Only packets whose layout changed are touched.** `translate.rs` names each one — chunk palettes and
light masks, `login`, `respawn`, `particles`, `move_entity`, `position_sync`, `animate`, advancements,
tags, and for 26.2 two quirks: a tag slot display is a bare tag id and pot decorations are item ids
(`adapter.rs:100-112`). Every other packet passes through byte for byte; the translator returns `None`
for the payload, not a copy.

**Registry sync is name-keyed, so it needs no id table.** `RegistryData` carries
`RegistryEntry { id: resource location, data: Option<Nbt> }` (`packets/configuration.rs`), built from
`RegistrySync::registry_packets` (`lan/data.rs:395`). A 26.2 peer is sent the entries it can name and the
26.3-only ones are omitted; the numeric ids in later packets are then translated by the run tables.

**Versions do not stack.** An older version is translated straight to the pinned numbering, one release
at a time (`adapter.rs:1-7`): 26.2→26.3 is one step. Nothing chains sessions.

**For a server the roles mirror.** A Horizon server reading a 26.2 client needs serverbound packets
translated *up* and clientbound packets translated *down*. The registry renumbering already works both
ways; the missing pieces are the two layout entry points (`translate::serverbound` up,
`translate::clientbound` down) and a `Session` that knows it is the server. That is the bulk of the
remaining work.

**What a 26.2 client sees is 26.3 simulated through a 26.2 lens.** The server keeps one 26.3 world and
one 26.3 tick. Content that has no 26.2 id — the wool and concrete slabs and stairs 26.3 added — cannot
be represented; the down tables have no entry for it. Those block states fall back to one the client can
draw, registry entries are omitted, and items lose the components the client cannot parse. This is a
compatibility view, not a 26.2 game; players who want 26.2 mechanics need #9881's per-version data.

## What stays shared

The translation boundary is `Connection`, so every system above it is version-agnostic by construction:
the renderer (`gfx.rs`, wgpu pipelines, shaders, LOD), meshing and chunk storage, streaming, world
generation, physics and the tick scheduler, mobs and AI, audio, authentication, the save format and
datafixers, commands, the container menus, HUD and screens, the dedicated server's admin, CLI, control
API and anticheat. None of them link a version, branch on one, or read a version table.

A version differs only where the wire differs: packet ids, the layout of a named packet, registry ids,
and the handful of components whose encoding changed. Everything else is data the client already
receives in a shared form.

## What a 26.3-only player pays

Nothing, and the reason is structural rather than a promise.

- **No adapter is created.** `Connection::adapt(ServerVersion::Current)` stores `None` in both halves
  (`connection.rs:376-381`). There is no default `Session`, no lazy cell, no "current" table.
- **One null check per packet, no copy.** `recv` matches `&self.adapter`; `None` returns the packet it
  already has (`connection.rs:443-449`). `send` checks the same option before `send_contents`
  (`connection.rs:599-609`). A native 26.3 session never reaches `Session::clientbound` /
  `Session::serverbound`, so no translation buffer is allocated and no payload is copied.
- **Registry renumbering is disarmed.** `ACTIVE` is a thread-local `Cell<Option<(&Ids, Way)>>`
  const-initialised to `None` (`adapter/ids.rs:146-147`). `apply` returns its input on the `None` arm
  (`adapter/ids.rs:164-166`); no version is read, no table is indexed, nothing is allocated.
- **The 26.2 tables are static data, never initialised.** They are `&'static [Option<u8>]` arrays and a
  `static Ids` built by const evaluation: they live in read-only data, cost no heap, spawn no thread and
  are not read — so they are not even faulted into resident memory — on a 26.3-only path.
- **Nothing is downloaded or read.** 26.2's shape comes from the peer's own packets plus compiled
  tables. There is no per-version data directory, manifest or file read at startup. (A 26.2 *client*
  needs its own 26.2 assets to draw; that is its install, not Horizon's cost.)
- **The residual cost is unchanged from today.** One predictable branch per packet and one thread-local
  read per registry id already ship in 26.3 builds. The design adds no branch, allocation or lookup to
  that path, and PR 6 asserts it rather than assuming it.

## How 26.3 stays safe

26.3 remains the only canonical form. The 26.3 packet structs, `ids.rs`, tick logic and save format are
not modified to describe 26.2; new constants live in `adapter/v26_2.rs` and new layout code in
`translate.rs`. `PROTOCOL_VERSION` stays the default at every site that states a version.
`tools/gen_version_consts.py --check` pins the committed version facts to the jar, and regenerating
`ids.rs` from 26.3's report reproduces it byte for byte (`docs/version-ports.md`), so a change made to
please 26.2 shows up as a failed regeneration or a failed 26.3 harness. Server-side acceptance starts
behind a `server.properties` switch with an empty default, so a 26.3-only server behaves exactly as it
does now.

## Follow-up PRs

Each is under about 500 lines, compiles alone, and keeps every 26.3 harness green.

1. **`NetConfig` version policy** — `accept_older: Vec<ServerVersion>` on `NetConfig`
   (`lan/mod.rs`), a `server.properties` key parsed into it, empty by default. Status keeps advertising
   777. No behaviour change yet.
2. **Handshake recognition** — `lan/conn.rs` accepts a protocol in the configured set instead of
   `!= 777`, attaches nothing yet, and login passes through as today; state the accepted version on the
   connection so later PRs can read it. Tests cover 777 (unchanged), 776 accepted when configured, 776
   refused when not.
3. **Server-role session and the zero-cost hook** — `Session::for_server`, `Connection::adapt_server`,
   and serverbound-up / clientbound-down layout translation for the packets that already have a
   client-role translation. Tests: a 26.2 body round-trips, and `adapt(Current)` leaves both adapter
   slots `None`.
4. **Registry sync down** — filter `RegistryData` entries by name for a 26.2 peer, apply the `DOWN` runs
   to the numeric registries, and drop components the client cannot parse. Tests compare the emitted
   names and ids against the generated 26.2 tables.
5. **26.2 gameplay divergences** — `swing`/`punch`, `add_transient_block`, `post_effects`,
   `swing_animation` versus the split attack/interact animation, and the palette fallback for block
   states 26.2 lacks.
6. **Tests and the overhead proof** — a 26.2 bot against a real Horizon server in `lan_e2e`, registry
   mapping cases, and assertions that a 26.3 session constructs no `Session`, allocates no translation
   buffer and reads no 26.2 table.
7. **Turn it on** — default `accept_older` to 26.2 once 6 is green, with the release note saying a 26.2
   client sees a 26.3 world through a 26.2 view.
