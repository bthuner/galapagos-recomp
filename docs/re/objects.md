# How the world thinks: objects, state machines and gates

> Source: `crates/formats/src/logic.rs` (parsers, a reference evaluator and
> tests) and the `logic` field of `WorldObject` in `crates/formats/src/worlds.rs`.
> Reverse engineered from `galapagos.exe` (Ghidra via REA, plus a raw
> `objdump` listing for the many functions that are only reachable through
> vtables) and checked against all 2,097 objects of the eight sections of
> the retail CD. No game data lives in this repository.

Confidence labels, as in `worlds.md`: **observed** = read in the code *and*
verified on the data; **inferred** = consistent with code and data, meaning
guessed; **unknown** = bytes we carry without understanding.

## The one-sentence version

Every object in a Galapagos world is a small **state machine**; once per
tick each one checks a few conditions ("was I clicked?", "is Mendel touching
me?", "has the bridge reached state 2?", "have 40 ticks passed?") and, when
a transition fires, it switches state and picks, by number, *which* texture
set, vertex morph, sound, motion path or class-specific action to run next.
There is no scripting language: the designers wired numbers together in a
Mac editor, and the engine is a ~100-line interpreter.

## 1. Where the logic lives in an object record — observed

`worlds.md` described the common record (`WorldObject_Read` @ `0x423b50`)
with its seven `block_end` offsets. All seven blocks are now decoded:

| Block | Starts at | Reader (exe) | Rust | Content |
|------:|-----------|--------------|------|---------|
| head  | record + 4 | `WorldObject_Read` | `WorldObject` | `block_end[7]`, parent, initial state, position |
| 1 | after head | `WorldObject_ReadMesh` @ `0x423150` | `Mesh` | polygons + vertices |
| 2 | `block_end[1]` | `StateMachine_Read` @ `0x42d580` | `StateMachine` | **the behaviour** |
| 3 | `block_end[2]` | vtable `0x460f60`+8 → `0x42aea0` | `ShapeAnim` | vertex-morph animation |
| 4 | `block_end[3]` | `PolyAppearance_Read` @ `0x42e180` | `AppearanceState` | textures per state |
| 5 | `block_end[4]` | `0x42b9e0` (class `SoundEmitters_Ctor` @ `0x42b7c0`) | `SoundEmitter` | positional sounds |
| 6 | `block_end[5]` | `MotionPaths_Read` @ `0x428cf0` | `MotionPath` | keyframed movement |
| 7 | `block_end[6]` | vtable `0x45f310`+8 → `0x40a0d0` | `ObjectParams` | arguments of the class trigger |

Every block starts with a `u16` "present" flag. Each parser ends **exactly**
on the next `block_end` for every object of the CD (the Rust test
`every_object_logic_parses_and_ends_on_its_block` checks it), and — the
surprise — **no class has a tail** after block 7: the per-class difference
is entirely in code (one overridden virtual, section 6), not in data.

Two head fields change meaning:

* `i16 folder` is the **parent object id**: a 1-based index into the
  section's object list (`ObjectRegistry_GetObject` @ `0x4282b0`). 2,028
  parents are `Fldr`, 35 `Objt`, 15 `MEnc`, 11 the camera slot. Observed.
* `u32 flags` is the **initial state** (low `u16`, high half always 0),
  queued by `StateMachine_SetInitialState` @ `0x42ce50`. In range for
  2,089/2,089 objects. Observed.

### Object ids

Each object registers itself in the world's object registry
(`ObjectRegistry_Register` @ `0x4284d0`, called from `StateMachine_Read`
and from the camera reader): ids are 1-based and follow the object list, the
camera included (every non-camera object has a state machine). The registry
(class vtable `0x460ce8`, at `Game + 0x17ee54`) holds three parallel arrays:
current state per id (`+0x0c`), object pointer (`+0x10`), subtree skip count
(`+0x14`), plus the *picked object* of this tick at `+0x18`.

## 2. The state machine block — observed

```text
u16 state_count
state_count x State {
  i16 value[6]; u8 flag[8]          entry Actions            (16 bytes)
  u32 state_flag                    0 or 1, see open questions
  i16 transition_count
  transition_count x Transition {
    i16 value[6]; u8 flag[8]        Actions
    i16 group_count
    group_count x {
      i16 condition_count
      condition_count x Condition { u16 opcode; u32 value }
                                    opcodes 0x16/0x17: u16 id, u16 state
    }
  }
}
```

### Actions: six numbered slots

The same 16-byte block is used for a state's *entry actions* and for a
transition's actions. `flag[2+i]` enables `value[i]`; `flag[1]` resets the
state timer; `flag[0]` is always 1 and never read.

| Slot | Meaning | Handler (WorldObject vtable `0x460ac8`) | Index base |
|-----:|---------|------------------------------------------|-----------|
| 0 | **class trigger** with params entry `n` | slot `0x18`, base `0x423400`, overridden per class | 1-based |
| 1 | shape (vertex morph) state `n` | slot `0x08` `0x423320` → `ShapeAnim_SelectState` @ `0x42adc0` | 1-based |
| 2 | appearance state `n` | slot `0x0c` `0x423350` → `0x42df50` | 1-based |
| 3 | play sound emitter `n` | slot `0x10` `0x4233a0` → emitter vtable +4 | 1-based |
| 4 | start motion path `n` | slot `0x14` `0x4233c0` → `MotionPaths_Select` @ `0x428c90` | 1-based |
| 5 | go to state `n` | slot `0x04` `CStateMachine_SetState` @ `0x42cfd0` | 0-based |

`SetState` also writes the new state into the registry (so others can test
it with opcode 0x16) and passes the state's `state_flag` to slot `0x20`
(stored at `+0x28`).

### Conditions: 23 opcodes

Inside a transition, conditions of a group are AND-ed, groups are OR-ed
(disjunctive normal form); a transition without groups never fires. Counts
are over the whole CD.

| Op | Test | Count |
|---:|------|------:|
| 0x01 | this object is the one clicked this tick (`registry+0x18 == this`) | 1340 |
| 0x02 | ... is not | 10 |
| 0x03 / 0x04 | ticks in state `>= n` / `<= n` | 934 / 627 |
| 0x05 / 0x06 | event bits `m` all posted this tick / not | 624 / 573 |
| 0x07 / 0x08 | rising / falling edge of `m` (this tick vs the previous) | 0 / 0 |
| 0x09 | `rand() % 100 <= n` | 166 |
| 0x0a..0x0f | position x, y, z `>=` / `<=` n (pairs) | 144..49 |
| 0x10, 0x11 | rotation[1] `& 0x3ff` `>=` / `<=` n | 0 / 0 |
| 0x12, 0x13 | rotation[0] `& 0x3ff` | 0 / 0 |
| 0x14, 0x15 | rotation[2] `& 0x3ff` | 10 / 27 |
| 0x16 | object `id` is in state `s` | 2195 |
| 0x17 | object `id` is not in state `s` | 368 |

All 2,563 `id`/`state` pairs of 0x16/0x17 point at an existing object and
an existing state. The only event mask used on the CD is **`1`**; the creature
code posts it (`0x4185e0`, `0x418956`: `object->vtable[0x24](&creature.events)`)
to the object it is in contact with, and `WorldObject_PostEvent` (slot `0x24`,
`0x4232f0`) ORs it into `+0x1c` **and forwards it to the parent**, so a
folder "feels" Mendel touching any of its children. Event 1 = "Mendel is
touching me" is inferred from the posting site (creature ground/contact
probe with masks `0x100` / `0x40000100`).

### The interpreter, precisely

```c
// Game_Tick @ 0x41bc50 calls ObjectRegistry_TickAll @ 0x4284f0(dt)
void tick_all(registry R) {
  // pass 1: update + evaluate, skipping disabled folders' subtrees
  for (id = next(0); id <= R.count; id = next(id))   // next(i) = i + R.skip[i] + 1
    R.object[id]->Update(dt);                         // slot 0x28, see 4
  R.picked = NULL;                                    // the click lasts one tick
  // pass 2: apply what fired
  for (id = next(0); id <= R.count; id = next(id))
    StateMachine_ApplyPending(R.object[id]);          // 0x42ce70
}

void StateMachine_Evaluate(obj o) {                   // 0x42d110, end of Update
  for (t in o.state.transitions)                      // ALL of them, in order
    if (any(g in t.groups : all(c in g : eval(c))))
      o.pending.merge(t.actions);                     // later transitions win per slot
  o.ticks += 1;                                       // u16 at +0x2c
  o.prev_events = o.events; o.events = 0;
}

void StateMachine_ApplyPending(obj o) {               // 0x42ce70
  if (!o.pending.dirty) return;
  if (o.pending.goto) {                               // entry actions of the target
    S = o.states[o.pending.goto];                     // overwrite enabled slots;
    o.pending.merge(S.entry);                         // a target with its own goto
  }                                                   // chains exactly one level
  if (goto)      SetState(goto);                      // order of the calls:
  if (shape)     SelectShape(shape - 1);              //   state, shape, appearance,
  if (appear)    SelectAppearance(appear - 1);        //   sound, motion, trigger,
  if (sound)     PlayEmitter(sound - 1);              //   timer reset
  if (motion)    StartMotion(motion - 1);
  if (trigger)   ClassTrigger(trigger - 1);
  if (reset)     o.ticks = 0;
  o.pending.clear();
}
```

Because evaluation and application are two passes, every object sees the
world as it was at the start of the tick: a lever and the bridge it controls
switch in the same tick, never half-way. The initial state is queued at load
time, so each object's first entry actions run on the very first tick.

`StateMachine::evaluate` / `resolve` and `Condition::eval` in `logic.rs`
implement exactly this against a `LogicView` trait.

### A real example — section 1, gate object #119 (`Tele`)

```text
state 0  entry: shape 1, appearance 1, sound 2         (closed, humming)
         -> state 1   when object 120 is in state 1    (the gate's frame opened)
state 1  entry: sound 1                                (open)
         -> state 0   when object 120 is in state 0
         -> state 2   when event 1                     (Mendel steps in)
state 2  entry: trigger 1                              (params[0]: change section -> 2)
```

## 3. Picking: how a click becomes condition 0x01 — observed

`0x41b980..0x41bb2c` (mouse handler of the game view): the cursor position
is turned into a ray of length 25,000 from the camera (`0x41bf10`), and
`World_Pick` (`0x4089e0` → `0x408730`) walks every polygon of every object,
keeping polygons whose **current appearance frame flags** intersect a mask,
and returns the nearest hit (object, polygon index, hit point; up to 10
candidates). Masks seen:

| Mask | Caller | Meaning (from the data histogram) |
|------|--------|-----------------------------------|
| `0xfffffffe` | mouse click | everything except bit 0 |
| `0x00000001` | mouse click with a configured key held | bit 0 is set on **`MEnc`** polygons only |
| `0x00000100` | creature (`0x41859a`) | bit 8: ground/contact (8,679 frames) |
| `0x40000100` | creature (`0x419122`) | + bit 30: set only on `PBal`/`PFrk` polygons (hazards) |
| `0x00000102`, `0x00000002` | camera (`0x413d43`, `0x411ee4`) | camera collision (inferred) |

The hit object is stored as the registry's picked object
(`ObjectRegistry_SetPicked` @ `0x406b50`) and the hit record is copied into
the object at `+0x1d4`. If the hit polygon has frame flag bit 5 (`0x20`),
the handler first talks to the creature (`0x41c580`, creature vtable `+0x20`)
and may call `0x419940` — a "go there"/"use" request (inferred; bit 5 is on
57 `Objt`, 32 `Tele`, 4 `Strt` frames). So **`PolyFrame.flags` are the
collision/picking classes of each polygon**, and they can change with the
appearance state: a door that opens can stop being solid. Bits 2..3 select
texture scrolling (see `worlds.md`).

## 4. Per-tick object update and motion — observed

`WorldObject_Update` @ `0x423420` (slot `0x28`), in this order:

1. if the transform is dirty (`+0xf8`): rebuild the matrix from position
   `+0xe4..+0xec` and rotation `+0xf0..+0xf4` (`0x402370`);
2. if vertices are dirty (`+0xfa`): slot `0x70`;
3. if a shape morph is playing (`+0x13c`): step it (slot `0x74` = `0x4235d0`):
   blend two frames per vertex, `v = a + ((b - a) * w >> 8)`, `w` in 1/256;
4. if an appearance animation is playing (`+0x13e`): slot `0x7c`;
5. if a motion path is playing (`+0x140`): slot `0x84` = `0x423a30`: one
   frame per tick;
6. `StateMachine_Evaluate`.

### Motion paths (platforms, bridges, fans, doors) — observed

```text
u16 path_count
path_count x {
  u16 looping           1 = wrap to frame 0, 0 = stop at the end    (observed in 0x428c20)
  u16 relative          1 = offsets from the rest pose, 0 = absolute (inferred, 0x423a30)
  u32 frame_count       = sum(steps + 1) of all keys but the last, + 1  (1,516/1,516)
  u16 key_count
  key_count x { i32 x, y, z; i32 rx, ry, rz; i32 steps }        28 bytes
}
```

The loader expands keys into one pose per tick with
`MotionKey_Lerp` @ `0x428a90`: `p = a + (b - a) * i / (steps + 1)`, integer
division (C truncation), for position and rotation alike. There is no
easing: speed is chosen by the number of in-between steps. A path with more
than two keys and `relative != 0` is post-processed with two 16.16 constants
(`0xeb85` ≈ 0.92, `0xf0a3` ≈ 0.94, `0x401140`/`0x401300`) — unknown,
possibly a rotation of the keys into the object's frame. Rotations are in
1024ths of a turn (conditions mask them with `0x3ff`); values such as 2048
mean "two full turns", which is how fans spin. Example, section 0 #6:
keys `x = 0 → -3000 → 3000 → 0` with 30/60/30 steps: a platform that sways
3,000 units each side over 124 ticks, looping.

### Vertex morphs (`ShapeAnim`) — observed layout, inferred meaning

```text
u16 state_count
state_count x { u16 rate; u16 frame_count; u16 key_count;
  key_count x { Frame; u16 m; m x Frame } }       (frame_count = total frames)
Frame = u32 nv; nv x i16[3]; u32 np; np x f32[6]
```

`nv` and `np` always equal the mesh's vertex and polygon counts (2,521
frames). The six floats per polygon are copied into the in-memory polygon
at `+0x44` (`0x4236be`) — most likely the texture-mapping gradients, since
there are no UVs on disk (inferred). `rate` (1..45) is unknown, probably a
playback speed.

### Sound emitters — observed layout, inferred meaning

```text
u16 count; count x {                                         34 bytes
  i16 sound[3]     indices into the section sound set, -1 = none
  u32 pitch        16.16, 65536 = 1.0
  u32 variation    16.16 fraction (0, 0.02, 0.05, 0.2)
  i32 near, far    attenuation distances (5000/20000 ... 300000 = global)
  i16 volume       256 = full
  i16 looping      0/1
  u32 reserved[2]  always 0
}
```

## 5. Class triggers and params — observed

Block 7 is a list of parameter entries, 124 bytes each:
`u16 10; i32 ints[10]; u16 10; i16 shorts[10]; u16 10; i32 ints2[10]; u16 8; i16 shorts2[8]`
(the lengths are asserted by the reader). Action slot 0 calls the class's
trigger (vtable slot `0x18`) with entry `n-1`; the trigger interprets it.
The factory (`Game_CreateObjectByFourCC` @ `0x41bf90`) maps FourCCs to
classes:

| FourCC | Ctor | Vtable | Trigger (slot 0x18) | Behaviour |
|--------|------|--------|---------------------|-----------|
| `Objt` | `0x41d5f0` | `0x4601f8` | `0x423400` (select only) | scenery; behaviour is entirely in its state machine |
| `Fldr` | `0x4285a0` | `0x460d10` | `0x4286f0` | `shorts[0]` 1 = enable, 0 = disable subtree; `shorts[1]` = descendant count → registry skip (410/410 equal the real subtree size). Slots 1..4 are no-ops (`0x413e60`). |
| `Tele`, `Strt` | **same class** `0x41b0a0` | `0x45fee8` | `0x41b180` | portal, see below |
| `STel` | `0x41aeb0` (subclass of the portal) | `0x45fd88` | portal | Update `0x41af10`: while **Space** (DirectInput scancode `0x39`) is held, the object counts as clicked: a "skip" key (section 0 only) |
| `Save` | `0x41af80` | `0x45fe38` | `0x41afe0` | if `shorts[0]`: `0x41ce90(state, shorts[1])`: save-pad bookkeeping (inferred) |
| `Voic` | `0x41b510` | `0x45ff98` | `0x41b570` | `shorts[0] = n`: play Smacker file `data/<n>` (`sprintf("%d")`, `Music_PlaySmacker` @ `0x439d90` → `SmackOpen`); 0 = stop. Values 1..10 match files `1`..`10`: music/voice tracks. Observed. |
| `Rend` | `0x419ee0` | `0x45fcd0` | `0x419f40` | 9-way switch on `shorts[0]` (0..8): renderer effects (unknown) |
| `Over` | `0x419e30` | `0x45fc20` | `0x419e90` | `0x419c50(shorts[0])`: overlay (unknown) |
| `MEnc` | `0x4260d0` | `0x460c10` | base | at (0,0,0) in every section; Update `0x426150`: when picked, makes Mendel react (`0x41cb80`); its polygons carry pick bit 0 (inferred: Mendel's own click target) |
| `Pred` | `0x42a1a0` | `0x460ea8` | `0x42a2d0` | predator: `shorts[0]` 1/2 = start an attack/behaviour (ignored while one runs), `shorts[1]` active, `shorts[2..3]` animation ids played through the creature animation system (`0x424870`) |
| `PBal`, `PFrk`, `PMan` | `0x415020`, `0x415480`, `0x415930` | `0x45f6c0`.. | `0x42a2d0` (shared with `Pred`) | animated hazards; their polygons carry bit 30, which the creature probes with mask `0x40000100` (inferred: contact hurts) |
| `PPol` | `0x4292b0` | `0x460de8` | `0x429430` | uses `ints` (e.g. `1, 22500, 520, 1000, 0, 30, 0, -1000`): unknown |
| `Cmra` | `0x412700` | `0x45f608` | — | camera, section 8 |

### Portals (`Tele` / `Strt` / `STel`) — observed

`shorts[0]` selects the kind; `ints` carry the arguments:

| Kind | Effect | Args |
|-----:|--------|------|
| 1 | teleport Mendel next to this object (`Game_RequestTeleport` @ `0x41c690`, executed by `Game_ExecuteTeleport` @ `0x41c700`) | `ints[0]` mode, `ints[1..3]` offset, `ints[4]` heading (1024ths) |
| 2 | same + camera snap (`Camera_Snap` @ `0x4129a0`) | same |
| 3 | **dimensional gate** (`Game_RequestSectionChange` @ `0x41c6e0`) | `ints[0]` destination section, `ints[1]` level number |
| 4 | **spawn Mendel** (`Game_SpawnCreature` @ `0x41c590`) — every `Strt` | mode, offset, heading, 3 extra ints |
| 5, 6 | Mendel reaction only | — |
| 7 | teleport + reaction | as 1 |

Kinds 1, 2, 3, 5, 6, 7 also call `Game_CreatureReact` @ `0x41cb80` with a
per-kind constant (`Portal_InitReactions` @ `0x41b140`: `0xf1000`,
`0x161000`, `0x251000`, `0x2d1000`, `0x1b2000`, `0x151000`), handed to the
creature (`0x4198b0`; see the creature spec).

### The gate table: why 8 sections and 5 worlds

The level-end routine (`0x41c8c0`) shows the "time screen" picture of level
`ints[1]` (`0x41a2d0(level + 1)`), prints the level and total times (tick
counters `/ 24`), waits for a key or click, then loads section `ints[0]`
through the game's virtual `LoadSection` (slot `0x18`). Every kind-3 portal
on the CD:

| From section | Object | → Section | Level (time screen) |
|-------------:|-------:|----------:|--------------------:|
| 0 | #160 | 1 | 0 |
| 1 | #119 | 2 | 1 |
| 2 | #186 | 3 | 2 |
| 2 | #204 | 6 | 6 |
| 3 | #26  | 4 | 3 |
| 4 | #119 | 5 | 4 |
| 5 | #39  | 2 | 5 |
| 6 | #9   | 7 | 7 |

(Object numbers are 1-based ids.) So the flow is
**0 → 1 → 2 (hub) → 3 → 4 → 5 → back to 2 → 6 → 7**. Section 2 is a hub:
it is the only section with save pads (5 `Save` objects), has two exits,
and is re-entered after section 5 (inferred: its second exit opens once the
first loop is done). Section 0 holds the `STel` "press Space" skip and
probably is the introduction; section 7 holds the four predators. Which
sections the readme calls the "5 worlds" is not settled; the five creature
overlays (`worlds.md`) are applied for world `i + 1`, which fits sections
1, 3, 4, 5, 6/7 being themed worlds around the hub (inferred).

Local teleporters (kind 1) come in rows: section 1 has 16 `Tele` pads in
two rows of 8, 3,700 units apart, each placing Mendel at a fixed offset
facing 256 or 768 — a corridor of stepping-stone transporters; section 5
has 7 in a chain.

## 6. Camera — observed layout, inferred behaviour

`Cmra` holds one path per object (`n` = total object count, verified) and
**one key per polygon of that object** (689 non-empty paths, each with
exactly `polygon_count` keys). It is not a spline: it is a lookup table.
Each tick `Camera_Update` @ `0x412d50` asks the creature which polygon it
stands on (`0x4124a0` → object id, polygon index); if that changed and the
polygon's key has `a != 0`, the key becomes the camera target
(`Camera_ApplyPolygonKey` @ `0x412f60`), and the camera chases it every
tick (`0x407030`, `0x4070b0`) with the smoothing of the key's *style*.

```text
CameraKey (20 bytes)
u8  a          1 = this polygon defines a camera
u8  b          style index 0..32
i32 x, y, z    camera position (absolute, or offset; depends on style.mode)
u16 c[3]       orientation, 1024ths of a turn (inferred)
```

The 468-byte "section trailer" is the style table, identical in all eight
sections (`Camera_ReadStyles` @ `0x414060`):
`u32 (ignored); u16 33; 33 x { u32 damping[2] (16.16: 0.6..0.95); i16 a, b (15/300, 20/300, 30/700, 7/150, 30/500); i16 mode }`.
Modes (from `0x412f60`): 0 and 7 fixed world point; 1, 8 and 4 relative to
the object Mendel stands on (`0x4230d0`); 2, 3, 9, 5, 6 relative to Mendel
(`0x4183a0`); 10, 11 x/y from the object, z from Mendel. The styles come in
triples (modes 0/1/2 with the same damping), so a designer picked "fixed,
object-relative or Mendel-relative" and a smoothness. The "sweeping,
cinematic" feel of the footage is this: per-floor-tile camera targets with
exponential smoothing. Polygons with `a = 0` keep the previous camera, and
the default key is used at load time.

## 7. The game loop — partly observed

`Game_Tick` @ `0x41bc50`:

1. `ObjectRegistry_TickAll` (update + evaluate all objects, then apply);
2. unless paused (`+0x17effa`) and once Mendel exists (`+0x17eff8`):
   creature update (creature vtable `+0x1c`);
3. camera update (`Camera_Update`);
4. `0x42c2f0` (sound update, inferred);
5. `++level_ticks; ++total_ticks` (`+0x17ee80`, `+0x17ee84`).

Rendering happens outside the tick. The level-end screen divides the tick
counters by 24 to print times (`mul 0xaaaaaaab; shr 4`), so the game is
designed around **24 ticks per second** (inferred; the idle handler
`App_IdleTick` @ `0x4218d0` calls the tick when `GetTickCount()` passes
`last + period`, but the period's setter was not found).

**Level start**: load the section (objects, initial states queued) → first
tick runs every object's entry actions → the `Strt` object's state machine
fires its trigger (after e.g. 100 ticks or on an event), which spawns
Mendel at `Strt.position + offset` facing `heading` (`Game_SpawnCreature`
sets `+0x17eff8` and, depending on its arguments, snaps the camera; the
exact argument mapping of `0x41c590` is not pinned down). Teleport and
spawn requests are queued and executed by `Game_ExecuteTeleport` when the
creature code asks.

## 8. What a reimplementation needs

* Load: everything in `Section` / `WorldObject.logic` (done). Ids = list
  index + 1; registry arrays of `count + 1`.
* Tick at 24 Hz: the two-pass interpreter above (`StateMachine::evaluate`,
  `resolve`), folder skipping, motion paths (`MotionPath::frames`),
  appearance state switches (already parsed), vertex morphs (blend 1/256).
* Picking: ray vs polygons with the current frame's flags (mask
  `!1`), nearest hit → `clicked` for exactly one tick.
* Events: set bit 1 on the polygon owner Mendel touches, OR it into every
  ancestor.
* Portals/gates/spawn, `Voic` music (Smacker files already decoded),
  the per-polygon camera with style smoothing.
* Still missing: creature movement/collision (another spec), the exact
  camera chase formula (`0x407030`, `0x4070b0`, `0x406f80`), `Rend`, `Over`,
  `PPol` and predator AI, the save-pad dialog wiring.

## Open questions

* `State.state_flag` (`u32`, 0/1, stored at `+0x28` via slot `0x20`): set
  on "waiting for Mendel" states of gates and starts; maybe "interactive".
* Sound emitter fields beyond the indices and volume; `ShapeState.rate`;
  the six per-polygon floats.
* The relative-path post-processing constants (0.92 / 0.94).
* Frame flag bits 6, 7, 9, 10..29 (bits 6, 8, 9 co-occur on almost every
  solid surface).
* Camera style `a, b` fields and the exact smoothing.
* Event bits other than 1 (none used on the CD).
* `Rend` (9 effects), `Over`, `PPol`, `MEnc` details; how predators hurt
  Mendel.
* The tick period actually programmed into `App_IdleTick`.

REA note: the function names in this document were applied with
`set_addresses_names` in the shared session; they are ephemeral and must be
re-applied after reopening the binary. Many handlers (`0x42cfd0`,
`0x423320`, `0x41b180` ...) are vtable-only and absent from Ghidra's
function list; read them in the objdump listing.
