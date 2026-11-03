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
| 1 | shape (vertex morph) state `n` | slot `0x08` `0x423320` → `ShapeAnim_SelectState` @ `0x42adc0`, then slot `0x70` `0x423530` (§4) | 1-based |
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
| 0x10, 0x11 | key `rx` (`+0xf2`, getter vtable `+0x38` = `0x423510`) `& 0x3ff` `>=` / `<=` n | 0 / 0 |
| 0x12, 0x13 | key `ry` (`+0xf0`, `+0x3c` = `0x423500`) `& 0x3ff` | 0 / 0 |
| 0x14, 0x15 | key `rz` (`+0xf4`, `+0x40` = `0x423520`) `& 0x3ff` | 10 / 27 |
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
  // pass 2: apply what fired; next(id) reads skip[id] AFTER applying id,
  // so a folder switching off here hides its subtree from this very pass
  // (their queued actions wait until it wakes up)
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
  if (goto)      SetState(goto);                      // 0x42cfd0; order of the calls:
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
switch in the same tick, never half-way.

`SetState` (`CStateMachine_SetState` @ `0x42cfd0`, slot `0x04`, shared by
every class: `Objt`, `Fldr` and the portals all point there) stores the
state number, its record, its `state_flag` (slot `0x20`, `+0x28`) and the
registry entry (`0x4283b0`). It does **not** reset the tick counter
`+0x2c`; only the actions' reset flag does. Observed. (The port used to
reset it on every state change.)

### Level load: every initial state is entered before the first tick — observed

The object reader `World_Read` @ `0x4222d0` (called by `0x408d90` from the
section loader `0x41cc20`):

```c
ObjectRegistry_Init(&R, count);        // 0x428430: state[], object[], skip[] zeroed
for (k = 0; k < count; k++)             // read every record through the
  Register(CreateByFourCC(...)->Read()) //   factory; StateMachine_Read
                                        //   queues the initial state (0x42ce50)
for (id = 1; id <= count; id++)         // 0x422372..0x422396
  if (R.object[id]) StateMachine_ApplyPending(R.object[id]);
level_ticks = 0;                        // +0x17ee80
```

So the **initial state is entered at load**, in list order and *without*
folder skipping: `SetState` writes it into the registry and the entry
actions (appearance, sound, motion, class trigger such as a folder
switching itself off, timer reset) all run before `Game_Tick` evaluates
anything. The first tick then evaluates every enabled object in its real
initial state with a tick counter of 0. Only after the loader returns does
the section change code take the level-start snapshot (`0x41caf3` →
`Game_TakeSnapshot` @ `0x41d170`). The `Save` variables live in the archive
object (`Game+0x17efb0`), created before the read, so a `Save` read-back in
an initial state already sees them.

Why it matters — section 2: folder #193 (holding `Strt` #194, Mendel's
start pad) starts in state 0 (entry: switch on) with one transition "#178
in state 0 → state 1" (entry: switch off its 9 descendants). #178 is a
folder that sits in state 1 and drops to 0 while Mendel touches its
subtree (events 1); its initial state is 1. Applied at load (exe), #193 stays on and #194 spawns
Mendel after 100 ticks; with the initial states merely queued until the
first tick's apply pass (the port before this fix), #193 read #178 in state
0 on tick 0 and switched off for good, so Mendel only appeared with the
debug "wake every machine". The same mistake made many objects evaluate
state 0's transitions once on tick 0 (e.g. section 2's #24..#45 jumped from
initial state 3 to 1), and let children of folders that disable themselves
at load run one tick.

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
| `0x00000102`, `0x00000002` | camera (`0x413d43`, `0x411ee4`) | camera wall probes, modes 7–9 (`0x413cb0`; observed, §6) |

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
3. if a shape morph is playing (`+0x13c`): step it (slot `0x74` = `0x4235d0`,
   below): next frame, `w = 0x100` so no blend;
4. if an appearance animation is playing (`+0x13e`): slot `0x7c`;
5. if a motion path is playing (`+0x140`): slot `0x84` = `0x423a30`: one
   frame per tick;
6. `StateMachine_Evaluate`.

### Motion paths (platforms, bridges, fans, doors) — observed

```text
u16 path_count
path_count x {
  u16 looping           1 = wrap to frame 0, 0 = stop at the end    (observed in 0x428c20)
  u16 mode              0 = add to the pose, 1 = rest + frame, 2 = absolute (observed, 0x423a30)
  u32 frame_count       = sum(steps + 1) of all keys but the last, + 1  (1,516/1,516)
  u16 key_count
  key_count x { i32 x, y, z; i32 rx, ry, rz; i32 steps }        28 bytes
}
```

The loader expands keys into one pose per tick with
`MotionKey_Lerp` @ `0x428a90`: `p = a + (b - a) * i / (steps + 1)`, integer
division (C truncation), for position and rotation alike. There is no
easing between two keys: speed is chosen by the number of in-between steps.

**Paths of more than two keys are smoothed** (observed, `0x429010..0x4291af`;
ported in `MotionPath::frames`). If the path's mode (`path+2`) is not 0, the
loader runs every expanded frame up to the **second-to-last key** through a
per-axis smoother — positions with factor `0xeb85` (≈ 0.92), rotations with
`0xf0a3` (≈ 0.94), both reset first (`0x401140` → `0x406aa0`, reset
`0x406af0`, step `0x401300` → `0x406b90`). The smoother is a critically
damped alpha-beta tracker in 16.16: `g = 1 − a²` (`0x10000 − (a·a >> 16)`),
`h = (1 − a)²`; the first input passes through, then each step predicts
`p = x + (v >> 16)`, `e = in − p`, `x = p + (g·e >> 16)`, `v += h·e`. Then
the last stretch is re-lerped from the smoothed second-to-last key towards
the exact last key, for `i = 1 .. steps − 1` (`jl`: the frame just before
the last key keeps its first lerp). So corners are rounded off, the path
still ends exactly on its last key, and only that last corner keeps its
kink. 108 of the CD's 1,516 paths qualify, the intro's capsule among them
(its largest change of speed from one tick to the next drops from 536 to
81 units). The first port skipped this step ("unknown"), and platforms
visibly lurched at every key. Rotations are in
1024ths of a turn (conditions mask them with `0x3ff`); values such as 2048
mean "two full turns", which is how fans spin.

`WorldObject_StepMotion` @ `0x423a30` hands each frame to a pose setter
chosen by the path's mode (observed in the listing): **0** → slot `0x68`
= `0x422e80` *adds* the frame to the current pose every tick (a nudge:
section 1's pads bounce ±100 along their normal with one-key paths
`[0, ±100, 0]`); **1** → slot `0x64` = `0x422cb0`: rest position
(`+0xd0`) plus the frame's position, the frame's rotation as is; **2** →
slot `0x60` = `0x422af0`: the frame as is. All three store the key's
`rx` in `+0xf2`, `ry` in `+0xf0` and `rz` in `+0xf4`, and the object
matrix is `Math_RotationMatrix(+0xf2, +0xf0, +0xf4)` @ `0x430ce0`
(`0x4096a0`), applied as `v · M`: `rx` turns about z (yaw), `ry` about x,
`rz` about y (`sim::transform`). Example, section 0 #6:
keys `x = 0 → -3000 → 3000 → 0` with 30/60/30 steps: a platform that sways
3,000 units each side over 124 ticks, looping.

### Drawing between ticks — not in the original

The 1997 game drew once per tick (`Game_Tick` @ `0x41bc50` passes a full
tick, `0x100`, to the updates; the motion step takes no fraction). A browser
draws at 60 Hz or more, so a 24 Hz pose holds for two or three frames and
then jumps. The port blends the object matrices (`Sim::transforms_at`),
Mendel's mesh (`Creature::vertices_at`) and the camera with the same
`alpha` (the fraction of the next tick already elapsed): a platform glides
and Mendel stays glued to it. Angles blend the short way round; inferred:
a move of more than 8192 units or a quarter turn in one tick (a path
restarting elsewhere, a parked predator), a teleport of Mendel (> 4096), a
change in his mesh size, or a snapshot coming back is a cut, drawn as is.
Shape morphs are not blended.

### Vertex morphs (`ShapeAnim`) — observed, ported in `sim.rs`

```text
u16 state_count
state_count x { u16 polygon_count; u16 frame_count; u16 key_count;
  key_count x { Frame; u16 m; m x Frame } }       (frame_count = total frames)
Frame = u32 nv; nv x i16[3]; u32 np; np x f32[6]
```

`nv` and `np` always equal the mesh's vertex and polygon counts, and the
`u16` the reader keeps at `+0xe` (once read as a "rate") is the polygon
count too, on every block of the CD. **Every object with a mesh has one**
(1,679 of 1,679), and it is not decoration: it is where the world's
texture coordinates live.

* **Select** (shape action, slot `0x08` `0x423320`): `ShapeAnim_SelectState`
  `0x42adc0` stores the state (`+0xc`) and frame 0 (`+0x10`) and returns
  "more than one frame", kept at `WorldObject +0x13c` (playing); then
  slot `0x70` `0x423530` puts the current frame on the mesh: its vertices
  become the mesh's (`0x402280`), and its six floats per polygon are
  copied to the polygon record's `+0x44..+0x5b`: the texels `(u, v)` of
  vertices 0, 1, 2, replacing the initialiser's (0,127) (0,0) (127,0)
  (`0x42320d`).
* **Play** (update step 3, slot `0x74` `0x4235d0(w)`): slot 1 of the
  helper (`0x42ae50`) advances the frame and **wraps it to 0** (a playing
  state loops until another shape action); with `w != 0x100` the frame
  would be blended with the mesh's vertices (`a + (b − a)·w >> 8`, into
  `+0x154`, `+0xfa` dirty), but `Game_Tick` always passes `0x100`, so
  the frame is used as is; then vertices and texels as above.
* **Read by**: the draw (Direct3D re-derives every vertex's `(u, v)` from
  the three texels when the mesh changes, docs/re/worlds.md), the casts
  (`0x423130` returns the current frame's vertices to the mover,
  `0x411c3d`, `0x4125ba`, and the world cast, `0x408822`), and the
  scrolling-floor basis (`0x41d9e0` calls slot `0x70` right before
  `0x41d6d0` reads `+0x44`, at build: state 0, frame 0).
* **Saved** (`0x42b0f0` / `0x42b120`: `+0xc`, `+0x10`); the object's
  load (`0x423f29`) calls slot `0x70` after reading it.

On the CD **every object's initial state selects a shape** but one
(section 0's #113, until it enters its state 0: 1,678 of 1,679;
`examples/shape_probe.rs`), and **no** frame carries the initialiser's
texels, so the level-load pass (§2) re-maps the whole world's textures
before the first frame is drawn. 28 objects have real morphs (states of
more than one frame), e.g.
section 0's flame curtains #70 around the start disc: state 0 shape 1
(flat, ±93) and appearance 3 (hidden); when #57 enters state 2 (Mendel is
born) it waits 60 ticks, starts shape 2 (32 frames, rising) with
appearance 1 (red, texture 37), and at 90 ticks shape 3 holds it at full
height (−93..2960) until the capsule #3 enters its state 2, when shapes 4
and 6 lower it and appearance 3 hides it again.

The port had none of this until `fix/pose-red-panels`: the curtains
stayed flat (the low red wedges on the pad's sides) and every polygon of
the world wore one stretched texture copy.

### Sound emitters — observed layout, inferred meaning

```text
u16 count; count x {                                         34 bytes
  i16 sound[3]     1-based indices into the section sound set, -1 = none
                   (observed on data: 1-based leaves 1 of 1,467 references on
                   an empty slot, 0-based leaves 107); one is picked at random
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
| `Save` | `0x41af80` | `0x45fe38` | `0x41afe0` | **not the save pads**: a persistent variable. `shorts[0] = 1`: store this object's state in slot `shorts[1]` of the archive object's `i32[10]` (`0x41ce90` → `0x41d530`); `0`: read the slot back as the next state (`0x41ceb0` → `0x41d560`, `0x42ce50`). Observed; see `saves.md` |
| `Voic` | `0x41b510` | `0x45ff98` | `0x41b570` | `shorts[0] = n`: play Smacker file `data/<n>` (`sprintf("%d")`, `Music_PlaySmacker` @ `0x439d90` → `SmackOpen`); 0 = stop. Values 1..10 match files `1`..`10`: music/voice tracks. Observed. |
| `Rend` | `0x419ee0` | `0x45fcd0` | `0x419f40` | 9-way switch on `shorts[0]` (0..8): renderer effects (unknown) |
| `Over` | `0x419e30` | `0x45fc20` | `0x419e90` | a flat picture over the 3-D view (observed, see **The title overlay** below): `shorts[0]` picture (0 = the GALAPAGOS strip, -1 = take it down), `shorts[1]` centre, `shorts[2..3]` x, y, `shorts[6]` white flash. One on the CD: section 0's #59, world 0's title |
| `MEnc` | `0x4260d0` | `0x460c10` | base | at (0,0,0) in every section; Update `0x426150`: when picked, makes Mendel react (`0x41cb80`); its polygons carry pick bit 0 (inferred: Mendel's own click target) |
| `Pred` | `0x42a1a0` | `0x460ea8` | `0x42a2d0` | predator (observed, see **predators.md**): `shorts[0]` 1 = vanish (then parked at 100000), 2 = appear at home then active; ignored while one runs; `shorts[1]` play the transition effect, `shorts[2..3]` its kind and variant (the same effect class as Mendel's dematerialisation, `0x424520`/`0x424870`; ported in transition.rs, see predators.md) |
| `PBal`, `PFrk`, `PMan` | `0x415020`, `0x415480`, `0x415930` | `0x45f6c0`, `0x45f7b0`, `0x45f8a0` | `0x42a2d0` (shared with `Pred`) | hunters: once active, a trapezoidal mover lunges past Mendel; six probes (mask 0x103) touch his click hull `MEnc`, which hands him the predator's flags (`0x405b2100` / `0x40582100`: lethal kind 2, death; `PMan`'s `0x40000100` is harmless: a hazard he sees). Bit 30 on their polygons makes his eyes see them. No `PMan` on the CD |
| `PPol` | `0x4292b0` | `0x460de8` | `0x429430` | the launcher: `ints` = mode, radius, angle, bob amplitude/phase, wobble amplitude/phase, lift; circles its home towards Mendel's bearing; `PBal` appears where it is (`Game+0x17efc0`) |
| `Cmra` | `0x412700` | `0x45f608` | — | camera, section 8 |

### The title overlay (`Over`) — observed

The huge chrome "GALAPAGOS" that appears over world 0 is neither in the
intro movie nor a 3-D object: it is section 0's object #59, class `Over`
(the only one on the CD), with two params entries and three states:

```text
state 0  (initial)  entry: reset timer          -> state 1 after 40 ticks
state 1             entry: trigger 1, sound 1   -> state 2 after 150 ticks
state 2             entry: trigger 2, sound 1
param 1  shorts [0, 1, 0, 0, 508, 37, 1, 0, 0, 0]   show picture 0, centred, flash
param 2  shorts [-1, 0, ...]                         take it down
```

The trigger `0x419e90` selects the entry and calls `0x419c50(shorts[0])`:

* `-1`: `0x436230(1)` releases the overlay surface (`0x48b65c`) and zeroes
  `0x46972c`, which keeps the blitter from drawing any overlay for the next
  10 frames.
* otherwise: if `shorts[6] != 0`, `Palette_Play(0, 250)` (the white flash,
  flash.rs script 0, 10 frames from white back to normal);
  `TimeScreen_Select` `0x41a2d0(shorts[0])` picks the picture by the view
  size (`0` gives the title strips: 504x30 / 256x15 / 128x8, pictures 0 /
  10 / 20 in 8-bit, 30 / 33 / 36 in 16-bit); `0x435e70` creates one
  off-screen DirectDraw surface of that size, `0x41aa10` fills it and sets
  its source colour key to **the top-left pixel**; position: x = `shorts[2]`,
  y = `shorts[3]`, centred if `shorts[1]` (`shorts[4..5]`, 508 and 37, are
  read and unused).
* `0x436270`, called from the frame presenters (`0x40f72e`, `0x42efd9`,
  `0x42f21e`), `BltFast`s that surface onto the back buffer with
  `SRCCOLORKEY` every frame, adding `(screen − picture) / 2` (screen size
  `0x46971c`/`0x469720`, 640x480) when centred.

So: 40 ticks (1.67 s) after world 0 is loaded the title appears, centred,
with a white flash; 150 ticks (6.25 s) later it disappears, no fade. The
1997 footage agrees: up at 1.68 s, down at 7.97 s, the strip 504/640 of the
screen's width, vertically centred, and *not* whitened by the flash (it is
blitted after the palette remap). Only world 0 has an `Over` object: the
other levels show no title card. The overlay belongs to the renderer, not
to the world, so it is not saved.

Port: `overlay.rs` (the trigger and the strip sizes), `Sim::overlay`,
`sim_overlay(w, h)`, and `web/src/title.js` (a 2-D canvas over the WebGL
one, colour-keyed, sized in original pixels × the canvas's scale).
`examples/overlay_probe.rs` lists every `Over` and `Rend` object.

### Portals (`Tele` / `Strt` / `STel`) — observed

`shorts[0]` selects the kind; `ints` carry the arguments:

| Kind | Effect | Args |
|-----:|--------|------|
| 1 | teleport Mendel next to this object (`Game_RequestTeleport` @ `0x41c690`, executed by `Game_ExecuteTeleport` @ `0x41c700`) | `ints[0]` the polygon of this object that becomes his floor contact (§6), `ints[1..3]` offset, `ints[4]` heading (1024ths) |
| 2 | same + camera snap (`Camera_Init` @ `0x4129a0`, §6) | same |
| 3 | **dimensional gate** (`Game_RequestSectionChange` @ `0x41c6e0`) | `ints[0]` destination section, `ints[1]` level number |
| 4 | **spawn Mendel** (`Game_SpawnCreature` @ `0x41c590`) — every `Strt` | contact polygon, offset, heading, 3 extra ints |
| 5, 6 | Mendel reaction only; the reaction ends in respawn reason 5 (kind 5: **game completed**) or 3 (kind 6: respawn from the snapshot), see `saves.md` | — |
| 7 | teleport + reaction | as 1 |

Kinds 1, 2, 3, 5, 6, 7 also call `Game_CreatureReact` @ `0x41cb80` with a
per-kind constant (`Portal_InitReactions` @ `0x41b140`: `0xf1000`,
`0x161000`, `0x251000`, `0x2d1000`, `0x1b2000`, `0x151000`), handed to the
creature (`0x4198b0`; see the creature spec). Read as surface flags they
pick Mendel's transition effect and what follows it (mendel.md §2.3.1):
kind 1 → effect 1 v7, reason 1; kind 2 → 1 v6, reason 2; kind 3 → 1 v5
(the white-out), reason 4 (the gate: the time screen comes only after
the effect, the port defers `EV_SECTION`); kind 5 → 1 v5, reason 5;
kind 6 → 2 v3, reason 3; kind 7 → 1 v5, reason 2. Kind 4 (spawn) passes
`ints[5..6]` as the materialise effect (`0x41c590` → `0x4198f0`; kind 0 =
none: s0 #57 uses 3 v3, s0 #83 1 v6, every other `Strt` none).

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

## 6. Camera — observed, ported in `crates/engine/src/camera.rs`

There are no camera paths. The designers gave **every polygon of every
object a camera key** (`Cmra` record: one path per object, one key per
polygon, 689 non-empty paths). When Mendel's floor contact moves to a
polygon whose key is set, that key becomes the camera's destination. The
camera *always looks at Mendel*; a key decides where the eye goes, how it
gets there, and the roll.

```text
CameraKey (20 bytes in the file, 24 in memory)       Camera_ReadPaths @ 0x413e80
u8  a          1 = this polygon has a camera            observed
u8  b          style index (read as signed char)        observed
i32 x, y, z    eye position, absolute or relative       observed
i16 yaw, pitch, roll   1024ths of a turn                observed: stored at
                       +0x12, +0x10, +0x14              (the reader's order)

CameraStyle (14 bytes, 33 of them, identical in all sections)  @ 0x414060
u32 d_look     16.16 tracker constant for the look-at point     observed
u32 d_eye      16.16 tracker constant for the eye               observed
i16 accel      eye mover acceleration, units/tick²              observed
i16 max        eye mover top speed, units/tick                  observed
i16 mode       0..11, see below                                 observed
```

Checks against the data: section 0's default key, read as (yaw, pitch,
roll), aims at the `Strt` pad within 1° (test
`camera_follows_mendel_in_section0`). Two keys (sections 0 and 5) have
`b = 0xff`, i.e. style −1, which reads before the table in the original:
we ignore them.

### The pipeline (classes `0x427780` → `0x414a40` → `0x406e70` → camera `0x412700`)

`Camera_Update` @ `0x412d50`, called by `Game_Tick` after the creature:

1. Read Mendel's contact (`0x4124a0`: object, polygon, polygon record).
   No contact at all: do nothing. If (object, polygon) differs from the
   current key's and that polygon's key has `a = 1`:
   `Camera_ApplyPolygonKey` @ `0x412f60`, then `0x407030` (retarget).
   A polygon without a key keeps the current one, **and the current
   key's object and surface stay the reference**.
2. **Mover** (`0x4278c0`): the eye point travels in a straight line to
   the key with a trapezoid speed profile: `speed += accel` up to `max`
   until the remaining distance is below the braking distance
   (`Σ 0, accel, 2·accel … ≤ max`, or half the trip if shorter), then
   `speed -= accel`; `pos += delta · speed / total`. Distances use
   `0x430fa0`: `max + 3/8·(the other two) − max/16`.
3. **Reference** (vtable `+0x14` @ `0x4136d0`): the mover's point is made
   absolute according to the style's mode:

   | mode | eye = | notes |
   |---:|---|---|
   | 0 | key | world point |
   | 1 | key + object position | object = the key's object (`0x4230d0`) |
   | 2 | key + Mendel | Mendel = creature `+0xa34` (`0x4183a0`) |
   | 3 | as 2 | holds still while Mendel is off the ground (`0x412400`) |
   | 4 | object + R·key | R = the object's rotation (`0x409880`) |
   | 5 | Mendel + R·key | (`0x409940`) |
   | 6 | as 5 | held off the ground |
   | 7, 8, 9 | as 0, 1, 2 | after wall probes (`0x413cb0`, below) |
   | 10 | x, y: object + R·key; z: Mendel + (R·key).z | |
   | 11 | as 10 | held off the ground |

   On a key change (`0x412f60`) the camera's current absolute eye is
   re-expressed in the new key's frame (minus the reference, rotated by
   R⁻¹ for 4/5/6/10/11), so the move starts from where the camera is.
4. **Trackers** (`0x406a50`, one per axis, eye and look-at): a
   critically damped alpha-beta filter in 16.16,
   `α = 1 − d²`, `β = (1 − d)²`; `pred = out + vel>>16`,
   `err = x − pred`, `out = pred + α·err>>16`, `vel += β·err`. The eye
   uses `d_eye`, the look-at point (Mendel) `d_look`. On a key change the
   trackers keep their state and only change `d`; if `d_eye` changes the
   mover first restarts from the tracker's output (`0x414c10`). Mover
   acceleration and top speed are only loaded at `Camera_Init`.
5. **Orientation** (`Camera_Orient` @ `0x4134e0`): yaw and pitch from
   eye → look-at (`0x430c50`); the roll travels from the current roll to
   the key's along the mover's progress (`0x4070b0`), and is expressed in
   the frame of the key's polygon (`0x4312b0`: the rotation taking +z to
   the polygon normal). `Camera_KeyRoll` @ `0x4132d0` measures a key's
   roll in that frame. Net effect: the key's up direction relative to its
   surface is kept whichever way the camera looks. Polygons whose normal
   is exactly +z (`nz = 0x400`) skip the frame and use the roll as is.
6. The renderer gets eye + (pitch, yaw, roll) (`View_SetCamera` @
   `0x403870`) and builds its matrix with `0x4302e0`: rows = right,
   forward (camera y = depth), up (camera z = screen up; projection
   `0x401640`: `sx = cx + x·f/y`, `sy = cy − z·f/y`).

The fixed-point library: sine/cosine tables × 16384 and an arctangent
table, built at start-up by `0x4308c0` (round half up);
`Math_RotationMatrix` `0x430ce0` and its transpose `0x430e40` (4.12
`short[9]`; checked against the assembly: Ghidra drops a sign in
element 1); `0x431b50` A·B, `0x431ad0` v·M.

**Wall probes** (`0x413cb0`, modes 7–9 only, mask `0x102`): six probes of
±700 along each axis from last tick's eye; a polygon crossing one pushes
the eye (and the trackers' state) out along its normal so the probe tip
sits 8 units in front of it.

**Field of view** (`View_Init` @ `0x4033bb`): 110°, used as a focal
length of `width / tan 55°` with square pixels (`0x403580`). inferred:
"width" is the full viewport width, which gives ≈ 56° vertically on 4:3.

**Interpolation**: the tick takes a sub-tick fraction (`(d·p >> 8) − d +
cur`), but `Game_Tick` passes a full tick (`0x100`, `0x422230`); the
browser interpolates between the last two ticks instead (not in the
original).

### When the camera cuts — observed

`Camera_Init` @ `0x4129a0` jumps straight to a key, with fresh trackers:
at **level load** (`0x41ccd2`, no contact yet → the section's **default
key**, used with its full orientation and a look-at point 5000 units
ahead), at **spawn** (`Game_SpawnCreature` @ `0x41c590`), at **respawn**
(`0x41c700` reason 3) and after **kind-2 teleports** (reason 2). Portals
hand Mendel his floor contact as (portal object, `ints[0]`): the field
called `mode` in the portal table is that polygon (on the CD it always
names a polygon of the portal, nearly always one with a key). A contact
without a key leaves the camera where it is.

So in section 0: the default key frames the capsule and tube from above
(the footage's diagonal opening shot), the spawn cuts to the `Strt` pad's
key 0, the Space skip cuts to arrival pad `#83` polygon 9 (a fixed point
above the area), and from there the camera stays put, turning to follow
Mendel, until he steps on another keyed polygon; the capsule's keys
(polygons 25, 27, 29: modes 1, 0, 4) make the camera ride along with it.

### Adapted in the port

* The key follows Mendel's contact polygon as the polygon walker keeps
  it (`Creature::surface`, the mover's `+0x8/+0x30`): set by every
  placement, changed on landing, kept in flight, as in the original.
* `+0x21e` (a creature contact flag that also unlocks modes 3/6/11) is
  not ported; "off the ground" = the walker is not grounded.
* Object rotations come from our object transform (sim.rs, the exe's
  `0x430ce0` convention), not from the object's second angle set via
  `0x4097a0`.

### Which way is up: +z (settled)

Everything the camera code assumes says **+z is up**: the "upright"
surface test is `normal.z == 0x400`; the creature's contact record
resets its surface normal to (0, 0, 0x400) (`0x411b60`); every portal
pad (`Strt`, `Tele`) is an octagon in the xy plane facing +z; and most
designer keys have their up vector near +z (e.g. section 3: 159 of 209).
The creature code agrees (mendel.md §2.2): the hull's last point is the
feet at `(0, 0, −450)`, airborne gravity is `up · −0x700`, the floor
finder casts for polygons with flag bit 1 (`0x2`), and the 1997 footage
shows Mendel standing on the +z faces of the pads. Our first creature
port assumed Y-down (from the first render, see worlds.md) and walked on
the pads' sides. +z is only the *default*, though: Mendel's own up is the
normal of the floor polygon he stands on (mendel.md §2.2), and some
levels stand him sideways (section 1's +y pads) or upside down
(section 2's start).

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

**Level start**: load the section (objects read, then every initial state
entered with its entry actions, §2 "Level load") → snapshot → ticks → the `Strt` object's state machine
fires its trigger (after e.g. 100 ticks or on an event), which spawns
Mendel at `Strt.position + offset` facing `heading` (`Game_SpawnCreature`
sets `+0x17eff8`, sets his floor contact to (the portal, `ints[0]`) and runs `Camera_Init`, §6; the
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
  the per-polygon camera (ported: camera.rs, §6).
* Predators (`Pred`, `PBal`, `PFrk`, `PPol`; predator.rs, predators.md).
* Transition effects: transition.rs (mendel.md §2.3.1).
* `Over`: world 0's title (overlay.rs, title.js).
* Still missing: `Rend`, the save-pad
  dialog wiring.

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
* `Rend` (9 effects), `MEnc` details (how predators hurt Mendel
  is in predators.md).
* The tick period actually programmed into `App_IdleTick`.

REA note: the function names in this document were applied with
`set_addresses_names` in the shared session; they are ephemeral and must be
re-applied after reopening the binary. Many handlers (`0x42cfd0`,
`0x423320`, `0x41b180` ...) are vtable-only and absent from Ghidra's
function list; read them in the objdump listing.
