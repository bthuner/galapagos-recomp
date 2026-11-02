# Saving the world: save pads, snapshots, `.gal` files and the time screen

> Source: `crates/engine/src/sim/save.rs` (snapshot, our save format, the
> clock), the `SavedCreature` block and `touches_save_pad` in
> `crates/engine/src/creature.rs`, the `Save` trigger and portal kinds 5/6
> in `crates/engine/src/sim.rs`, and the `game_*` / `time_screen_layout`
> exports at the end of `crates/engine/src/lib.rs`. Reverse engineered from
> `galapagos.exe` (Ghidra via REA plus a raw `objdump` listing). No game
> data lives in this repository.

Confidence labels as in the other specs: **observed** = read in the code
(and, where data is involved, checked on the CD); **inferred** = consistent
with code and data but not proven; **unknown** = carried without
understanding.

## The one-paragraph version

The original keeps exactly **one** spare copy of the world in memory, the
*snapshot*. It is taken when a level starts, after you save, and after you
open a game. When Mendel's body is destroyed, the snapshot is poured back
into the world and Mendel reappears where he stood when it was taken — but
his **brain is never part of the snapshot**, so he keeps everything he
learnt while dying. A `.gal` file is that same stream *plus* Mendel (brain
included) and the clocks; opening one restores everything. The famous
"save pads" are simply polygons whose frame flags carry bit 5 (`0x20`):
clicking one while Mendel stands on one opens the Save dialog.

Three surprises came out of the code:

1. The object class called **`Save` has nothing to do with save pads**. It
   is a persistent variable that lets a world remember its own state across
   section changes (§2).
2. **Level time excludes the time screen**: the game rolls the clock back
   after you dismiss it (§6).
3. **Portal kind 5 ends the game** and kind 6 is a "die and respawn" pad:
   they only differ by the reaction they hand Mendel (§7).

## 1. The save-pad flow — observed

### The click (`GameView_OnClick`, `0x41b980..0x41bb2c`)

```c
hit = World_Pick(ray, mask = 0xfffffffe);              // 0x4089e0
if (hit && (Object_FrameFlags(hit.obj, hit.poly) >> 5) & 1) {   // 0x409680 → 0x42e0f0
    mendel = Game_GetCreature();                        // 0x41c580: Game+0x17efb0
    if (mendel->vtable[0x20]())                         // 0x419930: return creature+0xaf0
    {
        App_Pause(0, 0);                                // 0x41ed20
        saved = App->vtable[0x10c]();                   // "Save Galapagos Game" dialog, 0x41f440
        App_Pause(1, 0);
        if (saved) Creature_InjectFlags(mendel);        // 0x419940: creature+0xae8 |= 0x21000
    }
}
Registry_SetPicked(hit.obj);                            // the click still reaches the object
```

`S` with the menu modifier (inferred `Ctrl+S`: DirectInput scancode `0x1f` in the app's key poll
`0x416e50`; 0x10/0x18/0x19/0x31 there are Q/O/P/N) runs the same three lines, also guarded by
`creature->vtable[0x20]()`. So: **no save outside a save pad.**

`0x21000` is a *reaction*, not a death: effect kind `(f >> 12) & 15 = 1`,
variant `(f >> 16) & 7 = 2`, respawn reason `(f >> 19) & 7 = 0` (nothing
happens afterwards). Mendel shimmers to confirm the save (inferred from
the effect parameters; the effect itself isn't ported).

### "Mendel stands on a save pad" (`creature+0xaf0`)

`Creature_HandleSurfaceEvents` @ `0x419550` ends with (observed,
`0x419730`):

```c
creature.on_save_pad = (creature.surface_flags >> 5) & 1;   // +0xaf0 = (+0x138 >> 5) & 1
```

and `surface_flags` (`creature+0x138`) is the OR of the frame flags of the
polygons Mendel's ten hull segments hit in `CreatureMover_CollideAndTouch`
@ `0x418450` — a `World_Pick` with **mask `0x100`** (solid) at
`0x418595`, plus flags injected by portals and the save itself
(`creature+0xae8`, copied in at `0x4188b2`).

**Data check (observed, puzzling)**: every polygon frame on the CD whose
flags have bit 5 has flags **exactly `0x22`** (bit 1 = camera collision,
bit 5 = save pad) and **never `0x100`**: 95 frames over 48 objects
(`Objt`, `Tele`, `Strt`, `Rend`), most of them vertical panels. Read
literally, the hull probe can never report them, and `+0xaf0` could never
become 1. Either the solid floor and the shimmering panel are meant to be
hit together by some path we have not found, or the shipped data and the
code disagree. **Our port (inferred)**: `on_save_pad` is also true when a
hull segment or a vertical segment through his body crosses a `0x20`
polygon, from either side (`Creature::touches_save_pad`), which matches the
readme's "click a save pad while Mendel is walking on it".

### What 0x41ce90 really records

`0x41ce90` (called by the `Save` trigger, §2) forwards to the archive
object's slot `0x14` — it is **not** part of the save-pad flow.

## 2. The `Save` class: ten persistent variables — observed

`Save` trigger @ `0x41afe0` (vtable `0x45fe38`, slot `0x18`):

```c
WorldObject_Trigger(entry);                              // 0x423400 (base)
if (params.shorts[0])                                    // 0x40a080(0)
    Archive_SetVar(params.shorts[1], Registry_State(this.id)); // 0x41ce90 → 0x41d530
else
    StateMachine_QueueState(this, Archive_GetVar(params.shorts[1])); // 0x41ceb0 → 0x41d560, 0x42ce50
```

`0x41d530` / `0x41d560` store/load `archive+0x13c + 4*slot` for
`0 <= slot < 10`. The archive object (`Game+0x17ee70`, vtable `0x4601c0`)
lives as long as the game, so these ten `i32` **survive section changes**,
and they are written to both the snapshot and the `.gal` (§3).

On the CD only section 2, the hub, uses it: objects `#212`
(`params = [(0,1), (1,1)]`) and `#213` (`[(0,0), (1,0)]`) load slot 1 / 0
when the hub starts and store their state back when it changes (`#163`,
`#164`, `#192` have no params). This is how the hub knows, on your second
visit (after section 5), that its first loop is done and the second exit
may open (inferred from the gate table in `objects.md`; the test
`save_variables_reach_the_hub_when_data_present` checks the mechanism:
with slots 0/1 = 1 the two objects come back in state 1).

## 3. The snapshot — observed

`Game_TakeSnapshot` @ `0x41d170`:

```c
delete Game.snapshot;                                   // +0x17eff0
Game.snapshot = new MemoryStream();                     // 0x4258d0 (vtable 0x460bb0)
Camera_WriteState(Game.camera, snapshot);               // 0x4142a0
World_WriteState(Game, snapshot);                       // 0x422460, see below
Mover_GetSurface(&mendel.mover, &obj, &Game.respawn.obj, &Game.respawn.poly, ...); // 0x4124a0
Mover_GetLocal(&mendel.mover, &local, &Game.respawn.heading);                       // 0x4124f0
Game.respawn.local = local; Game.respawn.local.z = 0;   // +0x17efc4..+0x17efd4
snapshot->Seek(0);
```

Note what is **not** in it: the creature. `World_WriteState` @ `0x422460`:

1. archive slot 3 (`0x41d5c0`): `u16` current section (`+0x58`), then the
   ten `i32` variables (40 bytes);
2. for every object id `0..count`: object slot `0x4c` (`0x424010` for
   world objects, `0x409bc0` adds six 18/6-byte camera-ish records for the
   `Objt` subclass): the matrix (`+0x64`), dirty flag and position/rotation
   (`+0xf8`, `+0xe4`), the state machine (`0x42d960`: current state, timer,
   pending actions, the `state_flag`), the running shape morph, appearance,
   sound emitters, motion path and params blocks, the three "playing"
   flags (`+0x13c/+0x13e/+0x140`), `+0x14c`, `+0x5c`, and per polygon two
   words (`0x402400`, `0x402470`: the current frame and its timer);
3. the sound system (`0x42cae0`): the count then the state of each of the
   35 voices that is playing a looping sound.

**When** (callers of `0x41d170`): new game (`0x4170cd`, `0x417310`), level
start right after `LoadSection` in `Game_LevelEnd` (`0x41caf3`), after a
successful Save (`0x41bc07`, view slot `0x100`), after Open (`0x41bbd9`,
view slot `0xfc`). A level-start snapshot is taken *before the first tick*
(initial states still queued) and before Mendel exists; the respawn point
is then filled by the next `Game_SpawnCreature` (`0x41c590` writes
`+0x17efc4..+0x17efd4` in both of its branches).

**Restore** (`Game_OnCreatureRespawn` @ `0x41c700`, reason 3):

```c
snapshot->Seek(0);
Camera_ReadState(camera, snapshot);                     // 0x414130
World_ReadState(Game, snapshot);                        // 0x4223c0: may reload the section
snapshot->Seek(0);
mendel.mover->vtable[4](respawn.obj, respawn.poly, respawn.local, respawn.heading); // place him
Camera_Snap(camera, mendel);                            // 0x4129a0
```

`World_ReadState` reads the section index first and, if it differs from
the loaded one, calls `LoadSection` (slot `0x18`) before reading the
objects (`0x4223ed`).

**Our port** (`Sim::capture` / `Sim::restore`, `WorldState`): per object
the state, timer, events, pending actions, position, rotation, running
motion path (index + frame), appearance state, per-polygon frame, the
"animating" flag and the folder skip; plus the ten variables, the CRT
random seed and the pending click. Restoring rewrites every object's
transform and appearance frame into the render buffers and drops queued
portal requests. Taken in `world_build` (level start), `game_save` and
`game_load`; restored by `Creature::finish_vanishing` on reason 3 (before
placing Mendel), followed by `EV_CAMERA_SNAP`. The respawn point is
Mendel's world position at snapshot time — equivalent to the original's
surface-relative one, since the world is restored to that very instant.
Shape morphs and sound voices are not in our snapshot (not simulated).

## 4. The `.gal` file — observed layout

There is no header: the file is the raw bytes of a `MemoryStream` written
by `Game_WriteState` (game vtable slot `0x2c` @ `0x41d010`) and read by
slot `0x28` @ `0x41cf40`. Save (`0x41f2a0`) writes the stream through a
file object (`0x42f9a0`, slot `0x74`); Open (`0x41efc0`) reads the whole
file into a `GlobalAlloc` block, wraps it in a `MemoryStream`
(`0x4259a0`) and hands it to the view (slot `0xfc`, which reads and then
takes a snapshot). Filter `"Galapagos saved games (*.gal)|*.gal|"`,
default name `"Untitled Game"`. All values little-endian, the
`MemoryStream` writes fields in their in-memory size:

| Part | Writer | Content |
|---|---|---|
| world | `0x422460` | as in §3: `u16` section, `i32[10]` variables, every object's state, sound voices |
| creature | slot `0x14` @ `0x419a90` | body (`0x426000`), mover (`0x412620`: surface object id, polygon, heading, local position, `+0x52`, `+0x24` up vector, `+0xe4`), the **three NERM controllers and three actuators** (`0x425f50`-style: 8 `i32` + 2 `i16` agent fields, `0x428070` x3, `0x42ab90` x3), then `u16` `+0x8f4 +0x8f6 +0x8f8 +0x8fa +0x8fc +0xa2c +0xa30 +0xa32 +0xaf0 +0xae2 +0xaee`, `u32 +0xae8`, `u16 +0xb04 +0xb06`, afflictions (`0x4067a0`), 20 bytes at `+0x16c`, 20 bytes at `+0xa34` |
| camera | `0x4142a0` | camera state (not decoded) |
| game | `0x41d010` | `u16` Mendel exists (`+0x17eff8`); respawn point: `u16` object, `u16` polygon, `i32[3]` local position, `u16` heading; pending teleport: `u16 u16 i32[3] u16`, `u16` pending flag (`+0x17efea`); `u32` total ticks (`+0x17ee84`); `u32` level start ticks (`+0x17ee88`) |

Saving also refreshes the respawn point to Mendel's current spot (the
`0x4124a0` call inside `0x41d010`). The `.mnd` export (`"Mendel NERMs
(*.mnd)"`) is the brain alone, see `mendel.md` §4.5.

We are **not** binary compatible (the in-memory object layouts would have
to be mirrored byte for byte). Our own format, `GLPS` version 1, carries
the same facts:

```text
u8[4] "GLPS"   u32 version = 1
u32 section    u32 total_ticks   u32 level_start_ticks   u64 sim_tick
WorldState                          the live world
u8 has_snapshot, WorldState         the respawn snapshot
u8 has_creature, u32 brain_len, u8[brain_len] brain (the .mnd bytes),
                 SavedCreature (414 bytes)

WorldState = u32 rng; u16 picked; u16 next_pick (0 = none);
             i32 vars[10]; u32 object_count; object_count x ObjState
ObjState   = u16 state; u16 ticks; u32 events; u32 prev_events;
             u8 pending_mask; i16 value per set bit 1..6;
             i32 pos[3]; i32 rot[3];
             u8 has_motion; u16 path; u32 frame;
             u32 appearance; u8 animating; u32 skip;
             u32 n; u16 poly_frame[n]
pending_mask: bit 0 present, 1 trigger, 2 shape, 3 appearance, 4 sound,
             5 motion, 6 goto, 7 reset_timer
SavedCreature: mode (u8 kind, u16 ticks, u8 teleport?, f32[3], i32),
             pos/fwd/up f32[9], forward/turn/height/hvel/stance_height i32[5],
             air f32[3], u32 air_ticks, ground contact (u8, u32 obj, u32 poly,
             f32[9]), u32 surface, u16 delay, i32 left, i32 right,
             touch (u8 u8 u8), i32 outputs[3], actuators i32[3][7],
             i64 cost[3], i64 count[3], i64 fwd_acc, i64 turn_acc, u16 phase,
             i32 stance, i32 turn, u8 side,
             3 x (u8, f32[3], i32) spawn / respawn / safe points,
             stats (u32, f32, u32 x4), u32 brain rng, u16 cue_quiet[5]
```

`game_load` parses everything (brain and creature included) before
touching the running game, and refuses files of another object count.
Round trip: loading then saving reproduces the bytes, and both games then
run on identically (test `saved_game_round_trip_when_data_present`).

## 5. Respawn reasons (`Game_OnCreatureRespawn` @ 0x41c700) — observed

| Reason | Effect |
|---:|---|
| 0 | nothing (the save confirmation `0x21000`) |
| 1 | execute the pending teleport (`+0x17efd6..`) |
| 2 | same, then snap the camera |
| 3 | **restore the snapshot**, put Mendel on the respawn point, snap the camera |
| 4 | `Game_LevelEnd(0)`: time screen, then the next section |
| 5 | `Game_LevelEnd(1)`: "GALAPAGOS COMPLETED" |

Reasons come from `(flags >> 19) & 7` of a lethal surface (all the CD's
lethal polygons say 3) or of a portal reaction (`Portal_InitReactions` @
`0x41b140`): kind 1 `0xf1000` → 1, kind 2 `0x161000` → 2, kind 3
`0x251000` → 4, kind 5 `0x2d1000` → **5**, kind 6 `0x1b2000` → **3**,
kind 7 `0x151000` → 2. Kind 5 pads: section 3 `#53`, section 7 `#111`
(next to a kind-7 teleporter); kind 6: section 7 `#22`. Our port emits
`EV_GAME_COMPLETE` for kind 5 and a respawn request for kind 6 directly
from the portal (the original waits for Mendel's dissolve effect).

## 6. The level clock — observed

`Game_Tick` @ `0x41bc50` ends with `++Game+0x17ee80; ++Game+0x17ee84`.
`+0x17ee80` is reset at every section load (`0x4221ce`, `0x4223a3`);
`+0x17ee84` is the **total** game clock, `+0x17ee88` the total at the
start of the current level. New game zeroes both (`0x4172fc`,
`0x417305`: setters `0x4221e0`, `0x4221f0`).

`Game_LevelEnd` @ `0x41c8c0`:

```c
if (completed) Game.level = 8;                         // +0x17efee
screen = new TimeScreen(Game);                         // 0x41a010, 0x94 bytes
if (TimeScreen_Select(screen, Game.level + 1, &pic)) { // 0x41a2d0
    TimeScreen_Load(screen, &pic);                      // 0x41aa10: picture → surface
    TimeScreen_DrawLevel(screen, total/24 - start/24);  // 0x41a090
    TimeScreen_DrawTotal(screen, total/24);             // 0x41a170
    start = total;                                      // +0x17ee88
    Present(&pic);                                      // 0x436170
}
Fade(0, 250);                                           // 0x438830
mendel->Remove(); Game.mendel_exists = 0;
if (!completed) {
    Game.paused = 1;
    do { Idle(); } while (!(key Esc || key Space || mouse button));   // 0x438b20(1), (0x39), 0x438cf0/0x438d10 (mouse, inferred)
    Game.paused = 0;
    total = start;                 // the time spent looking at the screen does not count
    LoadSection(Game.next_section);                     // slot 0x18 (+0x17efec)
    Game_TakeSnapshot();                                // 0x41d170
}
```

The seconds are computed per counter (`floor(total/24) - floor(start/24)`,
unsigned, `mul 0xaaaaaaab; shr 4`), not on the difference. Our
`game_times()` returns both ticks and these exact seconds; the level clock
restarts in `world_build`, so the host reads it when `EV_SECTION` arrives.
**Dismissal**: Escape, Space or a mouse click. **Completed**: the
picture is shown with the times, Mendel is removed, and nothing else
happens: no wait, no next section (the game ends there, inferred: what
the window shows afterwards depends on the renderer, not ported).

## 7. The time screen — observed

`TimeScreen_Select` @ `0x41a2d0(n = level + 1, out)` picks one of the 39
pictures of the archive's global block (`Archive::picture(i)`) by the
**view size** (the game window's client area, `0x42eb60`) and the display
depth (`DAT_0048b67c`: 0 = 8-bit palette, else 16-bit):

| View | 8-bit picture | 16-bit picture | Picture size | Glyph table |
|---|---|---|---|---|
| ≥ 560×400 (16-bit: ≥ 640×400) | `n` (1..9) | 31, or 32 if `n == 9` | 560×421 | large |
| ≥ 288×200 (16-bit: ≥ 320×200) | `n + 10` | 34 / 35 | 288×211 | medium |
| ≥ 144×100 (16-bit: ≥ 160×100) | `n + 20` | 37 / 38 | 144×108 | small |

(`n = 0` would select the title strips 0/10/20 or 30/33/36: 504×30,
256×15 or 248×15, 128×8, "GALAPAGOS"; not used by the level end.) So the
8-bit set has one picture per level (p01..p08, p09 = "GALAPAGOS
COMPLETED") and the 16-bit set only "normal" and "completed". Our
`time_screen_layout` uses the 8-bit rule.

Each picture ends with a **glyph strip** `0123456789:` in its bottom rows
(the visible picture is `height - glyph_height` rows: 397, 199, 101).
Glyph tables (x in the strip, width; height):

| Size | digits `0..9` | `:` | height |
|---|---|---|---|
| large | x = 35·d, w 35 | x 350, w 18 | 24 |
| medium | x = 1 + 18·d, w 18 | x 181, w 9 | 12 |
| small | x = 9·d, w 9 | x 91, w 5 | 7 |

Text: `sprintf("%.2ld:%.2ld:%.2ld", (s/3600) % 100, (s/60) % 60, s % 60)`
(`0x41a090`), so `hh:mm:ss`. Drawn **into the picture's own pixels**
(`0x41aca0` → `0x41ad30`), copying glyph pixels except those equal to
the picture's first (top-left) pixel, the transparent colour:

| Size | level time | total time | y (top) |
|---|---|---|---|
| large | starts at x = 6 | **ends** at x = 558 | 358 |
| medium | starts at x = 6 | ends at x = 282 | 180 |
| small | starts at x = 1 | ends at x = 144 | 92 |

(The total is right-aligned: `0x41aca0` with `align = 1` first subtracts
the width of every glyph.) That is just under the LEVEL / TOTAL labels.

## 8. The web ABI (lib.rs, "Saves, snapshots and the time screen")

| Export | Meaning |
|---|---|
| `EV_SAVE_PAD = 6` (event, `a` = object id) | from `sim_click` when a `0x20` polygon is clicked while Mendel is on a pad: open the Save dialog |
| `EV_GAME_COMPLETE = 7` (`a` = 8, `b` = 9) | portal kind 5: show picture 9 with the times |
| `game_save() -> u32` / `game_save_ptr()` | take a snapshot (new respawn point) and serialise `GLPS` v1 |
| `game_load(ptr, len) -> i32` | adopt an `alloc`ed buffer, rebuild its section, restore; 0 ok, -1 not a save, -2 bad section, -3 does not fit |
| `game_section() -> i32`, `world_vertex_count() -> u32` | re-read the buffers after a load |
| `game_times() -> *const u32` | level ticks, total ticks, level s, total s |
| `game_on_save_pad() -> u32` | Ctrl+S guard |
| `game_reset_progress()` | new game: zero the variables and clocks before `world_build(0)` |
| `time_screen_layout(level, w, h) -> *const i32` | 30 i32: picture, size, glyph strip, positions, 11 (x, w) glyphs |

## Open questions

* Why no save-pad polygon on the CD is solid (`0x22` only), when the exe
  learns about pads only through a `0x100` probe.
* The camera state layout (`0x4142a0` / `0x414130`).
* The exact on-screen placement of the time picture (`0x436170`) and what
  stays on screen after "GALAPAGOS COMPLETED".
* The meaning of the per-object `+0x14c` and `+0x5c` words in the stream.
