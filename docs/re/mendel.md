# Mendel: body, senses and the NERM brain

> Source: `crates/formats/src/creature.rs` (parser + tests). Reverse engineered
> from `galapagos.exe` (Ghidra via REA, plus the raw `objdump` listing for the
> many methods Ghidra never turned into functions because they are only
> reachable through vtables) and checked against the retail CD. No game data
> lives in this repository.

Confidence labels: **observed** = read in the code *and* verified on the
data; **inferred** = consistent with the code, meaning partly guessed;
**unknown** = not understood yet.

## The short version

Mendel is far simpler, and far more elegant, than the box copy suggests.

* His **body** is a vertex-animated mesh: a shell plus the two legs of one
  side, each stored as **128 key frames of one walk cycle**. The other side
  is the same legs mirrored and played half a cycle later. No skeleton, no
  inverse kinematics.
* His **brain** is three tiny controllers. Each one is a **10-state Markov
  chain** whose transition probabilities depend on the sensor values through
  a lookup table (the "genome"). The output is "how often the chain sat in
  an odd state". There is no neural network.
* He **learns** with a **genetic algorithm**: each controller keeps 30
  candidate genomes, tries them one after another, scores each by a running
  *cost* (pain, getting close to things, standing still), and every 30
  trials replaces the worst 15 with mutated mixes of the best 15. "Entropic
  reduction" is marketing for "lower cost wins".
* His **senses** are two infrared "eyes" (three ray casts each), two touch
  points at the front corners, and your right click, which drives the same
  two touch inputs.
* A `.mnd` file is just the three controllers dumped to disk: 28,242 bytes.

```text
  IR rays / touch / right click            NERM x3                Actuators          Body
  ------------------------------    ---------------------    ----------------    --------------------
  input L (agent+4)  ---------+---> c0 posture  -> 0..256 -> act38 (13,256) --> stance 41..207, ray range/angle
  input R (agent+8)  ---------+---> c1 speed    -> 0..256 -> act3c (20,128) --> speed = 2*v-256 (back/fwd)
  input 2 (agent+0xc) = 0 ----+---> c2 turn     -> 0..256 -> act40 (20,128) --> turn  = |2*v-256|, side = v>=128
                                          ^
                cost (pain, proximity, idling) -> genetic algorithm every 65536 cost-ticks
```

## Where Mendel lives in the `worlds` archive

| Offset (EN CD) | Size | What | Reader |
|---|---|---|---|
| `0x3601c6` | 85,656 | body (shell + 2 leg stances, 128 frames each) | `CreatureBody_Read` @ `0x426a70` |
| `0x37505e` | 5 x 28,242 | five pre-trained brains | `Creature_ReadBrain_Mnd` @ `0x419bd0` |
| `0x3977f8` | | sound set, 11 sounds | `Creature_ReadSoundCues` @ `0x417ab0` |
| end of each section | 468 | camera motion profiles (not Mendel!) | `Camera_ReadMotionProfiles` @ `0x414060` |

Every block ends exactly where the next begins (asserted by the tests).

The creature object is `0xb08` bytes, built by `LoadCreature` @ `0x41cde0` →
`Creature_ctor` @ `0x417360` → `Creature_ReadMaster` @ `0x417570`. It
inherits from an **Agent** base (`Agent_ctor` @ `0x425a80`, vtable
`0x460bd0`) that owns the brain, and embeds a **mover** at `+0x50`
(physics/collision, vtable `0x45fbb0`) and the **body mesh** at `+0x13c`
(vtable `0x460ca0`).

## 1. The body — observed

### 1.1 Byte layout

`CreatureBody_Read` @ `0x426a70`:

| Field | Value | |
|---|---|---|
| `u32 body_polygons` | 34 | |
| `u32 leg_polygons` | 22 | |
| `u32 body_vertices` | 41 | |
| `u32 leg_vertices` | 32 | |
| `BodyPart shell` | | read with args (kind 0) |
| `BodyPart legs_a` | | (kind 1, sub 0) |
| `BodyPart legs_b` | | (kind 1, sub 1) |

The engine allocates `body + 2 x leg` polygons (78) and vertices (105):
the second copy of the legs is the mirrored side.

`BodyPart`, read by `CreatureBody_ReadPart` @ `0x426c50`:

```text
i32 offset[3]                                   (-2,34,219) (280,0,117) (263,112,119)
u32 polygon_count                               asserted == expected
polygon_count x { u32 n; u16 index[n] }         n = 3..7
polygon_count x f32[6]                          texture coordinates (below)
128 x { u32 vertex_count; vertex_count x i16[3] }   key frames, vertex_count asserted
polygon_count x { u16 texture; u16 color_ramp; i16 u; i16 v }
```

On load each frame vertex `(x,y,z)` is transformed (`CreatureBody_ReadPart`,
code at `0x426f1b`; re-checked in the listing, this is the exe's, not an
adaptation):

```text
x' = 140 - offset[2] - z
y' = -(offset[1] + y)
z' = -(offset[0] + x)
```

Indices are offset into the shared vertex buffer and, for the shell and
`legs_a`, **reversed** (`CreatureBody_RemapIndices` @ `0x427040`, flag 1);
`legs_b` keeps file order because it is drawn mirrored.

**Textures** (observed): `texture + 16` is a slot of the *current world
section*. Slots 16..19 of all eight sections hold the same four blue-violet
skins, each repainted in its section's palette. `color_ramp` (4 or 8) is a
section colour-ramp index (a material, §1.3). `u, v` (polygon
`+0x5c/+0x5e`) are a **texel offset**, the same field a world polygon's
appearance frame sets (`0x402420`, mesh vtable slot 6): the Direct3D
draw adds `(u & 0x1ff) / 128, (v & 0x1ff) / 128` to the texture
coordinates (`0x40e850`), the software one `u & 0x7f, v & 0x7f` texels
(`0x404100`). 61 of the 78 polygons carry one.

**Texture coordinates** (observed by rendering): unlike the world meshes,
Mendel has real UVs, in texels (128 = one texture). The six floats are
`(u,v)` at three reference vertices: engine-order vertices `n-1, n-2, n-3`,
with `n-2` always `(0,0)`. `CreatureBody_ConvertUVs` @ `0x4270e0` solves the
affine map on the polygon plane and re-expresses it at vertices `0,1,2`
(what the rasteriser wants). For a GPU: fit the affine map from the three
references and evaluate it at every vertex (`creature::polygon_uvs`).

### 1.2 The walk cycle — observed

`CreatureBody_SetPose` @ `0x426410(turn, side, stance, speed, frac)`:

```text
phase16 += ((speed * frac) >> 8) * gait          # gait = body+0x69c, 24 by default
f = table[phase16 >> 8]                          # table[i] = i/2  (CreatureBody_ctor @ 0x426230)
shell      = shell.frame[f]
sideA(ph)  = lerp(legs_b.frame[t(ph)], legs_a.frame[t(ph)], stance/256)
if side != 0: A = sideA(ph)
else:         A = lerp(sideA(ph), sideA(-ph), turn/256)       # this side steps backwards when turning
B  = mirror_z(side == 0 ? sideA(ph+128) : lerp(sideA(ph+128), sideA(-(ph+128)), turn/256))
phase16 += (speed - (speed*frac >> 8)) * gait    # the rest of the step
```

So turning is **tank style**: one side's legs play the cycle in reverse,
proportionally to the turn amount. A full stride takes
`65536 / (256 x 24)` = about 10.7 ticks at full speed.

Proof: `walk_sheet.png` (four phases, two views) and `obj/mendel.obj` +
`mendel.mtl` + `mendel_tex16..19.png` in the scratch folder (not
committed: they are game data). Rest pose = phase 0, stance 128 (the
engine's load-time call is `SetPose(128, 1, 128, 0, 0)`). Mendel is a flat,
crab-like shell on four long two-segment legs, 105 vertices / 78 polygons.

### 1.3 His colours: materials, light, offsets, culling — observed

Mendel is lit exactly like the world (worlds.md §5.3); nothing about him
is special-cased in the renderer. What makes him look the way the 1997
footage shows is in the data:

* **Materials.** `CreatureBody_ReadPart` stores `color_ramp` at polygon
  `+0x60` (`0x426c50`). The first time a section is read
  (`0x41cc20` → `0x4177d0`, only while `+0xaf4/+0xaf8` are still null)
  each of his 78 polygons gets its own copy of that section material,
  appended to the renderer's table (vtable `+8` = `0x405620`), and
  `+0x60` is pointed at the copy; `+0xaf8` keeps the original index,
  `+0xaf4` the copy. The affliction manager's tick (`0x405fc0`) copies
  the *current* section's material over every copy before the effects
  run, so an effect's brightness (`0x406110`) lasts one tick. The look
  object at `+0x8f0` (`0x4105d0`) keeps a second polygon array with a
  second set of copies, refilled by `0x4109a0`, `0x410ba0`, `0x410c20`
  through the "set material n" `0x4056d0` (inferred: what camouflage
  draws with). On the CD only materials 4 (`m0` = 0.63)
  and 8 (`m0` = 0.96) are used, with `m1 = 0`, `m2 = m3 = m5 = 1`, the
  same in all eight sections: a face turned to the light reaches row 15
  (the plain texture), one turned away `trunc(m0/2 · 31)` = 9 or 14, and
  he never takes a specular highlight. Material 8 is the four front
  faces of the head (shell polygons 2..=5).
* **Normals.** One per polygon (flat), from the posed vertices
  (`0x4019a0`, recomputed when the mesh changes, `+0x2c`) and turned by
  his matrix (`0x401640`); the light is the section's, every frame.
  There are no vertex normals and no Gouraud shading.
* **Skins.** Textures 16..19 of the section (the engine adds 16 on
  load, `0x426c50`), paletted like any section texture. Skin 18, the
  head's, has a bright cyan-white stripe; the **texel offsets** put it
  in view on the head's front and underside (head polygon 0 is offset
  by (134, −2974), i.e. (6, 98) texels).
* **Culling.** His body is a mesh with the backface flag on (`0x402150`),
  so his back faces are dropped like the world's (worlds.md §5.3).

The port had the materials and the light right but ignored the offsets
(`unk0/unk1`): the head showed the violet part of skin 18 and he looked
darker and plainer than on the footage. With the offsets the glowing
stripe appears where the footage has it (3:00, 7:50).

## 2. Locomotion — observed

### 2.1 From brain to motion

`Creature_ApplyMotorOutputs` @ `0x4189c0` reads the three actuators:

```text
turn_out  = act40.value; turn  = |2*turn_out - 256|; side = turn_out >= 128
speed     = 2*act3c.value - 256                      # signed: walks backwards too
stance    = act38.value
gait      = 24, or 53 under affliction 3, or 40 under afflictions 7/8/9
CreatureBody_ComputeMotion(turn, side, stance, speed)     @ 0x426350
CreatureBody_SetPose(turn, side, stance_interp, speed, frac)
if grounded: vertical_velocity = 2 * (height - previous_height)
```

`CreatureBody_ComputeMotion` @ `0x426350` produces the per-tick deltas
(24-bit fractional accumulators carry the remainders):

```text
forward  += (256 - turn) * speed * gait * 420  >> 24     # creature+0x7d0, world units
turn_d   += turn * speed * gait * 245 >> 24              # creature+0x7d2, 1024 units per circle
turn_d    = side ? turn_d : -turn_d
height    = 41 + stance * 166 / 256                      # creature+0x7d4, body height above ground
```

At full speed straight ahead: about 39 units/tick; spinning: up to about
23/1024 of a turn (8 degrees) per tick.

### 2.2 The polygon walker — observed

Port: `crates/engine/src/walker.rs` (no stand-ins left; the few inferred
details are listed at the end). The mover lives at `creature+0x50`; its
base class (vtable `0x45f530`, `Mover_Init` @ `0x411b60`) is specialised
by the creature (vtable `0x45fbb0`):

| Slot | Address | What |
|---:|---|---|
| 1 | `0x418310` → `0x411bf0` | `Mover_Place(obj, poly, u, v, h, heading)` |
| 3 | `0x417f40` → `0x411e10` | the tick: slots 4, 8, 9, 5, then "let go" |
| 4 | `0x418060` | `CreatureMover_Integrate` |
| 5 | `0x411ec0` | `Mover_FindSurface` (attach `0x411f55`, let go `0x41213e`) |
| 6 | `0x412180` | surface → world: position and the three render angles |
| 7 | `0x418230` | surface probe `k` (segment from `pos + up/4` to `pos + frame·(px, py, −32 − stance)`) |
| 8 | `0x4183d0` | gather the `'Coll'` objects whose box meets the hull's (`0x4084e0`) |
| 9 | `0x418450` | `CreatureMover_CollideAndTouch` |

**State** (offsets in the mover): `+0x8` object and `+0x30` polygon he
lives on, `+0x52` grounded, `+0x44/+0x48` `(u, v)` in that polygon's
plane, `+0x4c` `h` = his origin's height along its normal, `+0x50`
heading (1024ths, in the plane), `+0x24` his up = the polygon's normal
in world space (× 0x400), `+0xa8` world position, `+0x94` flight velocity
(1/256 units), `+0xa0` spin, `+0xa4` vertical speed of `h`, `+0xe4` the
polygon's flags. A polygon's frame is the record's `+0x28` matrix
(`Math_FrameFromNormal` @ `0x4312b0`, polygon → object) with its origin
on the polygon's **first vertex**; world = object position + object
rotation · (v0 + frame · (u, v, h)) (`0x412180`).

**One tick** (`Mover_Step` @ `0x411e10`):

1. *Integrate* (`0x418060`). Grounded: `u += cos[h]·fwd >> 14`,
   `v += sin[h]·fwd >> 14` with the heading *before* this tick's turn,
   then `heading += turn`; `hvel += g`, `h += hvel`, `h` clamped to the
   stance height (`+0x784`); the **conveyor** (`0x41dc30`, below); the
   world position from `(u, v, h)` (slot 6). Nothing checks the
   polygon's edges here: `(u, v)` may leave it. Airborne: `vel += up ·
   g` (`up × 0x700 >> 10`), `pos += vel >> 8`, `heading += spin`.
2. *Collide* (`0x418450`), see below.
3. *Find the surface* (`0x411ec0`). Up to five probes (`k = 0..4`, in-
   plane offsets `(0,0) (7,7) (7,−7) (−7,−7) (−7,7)`, `+0x6c..+0x90`):
   each a segment along **his own down** from 256 above his origin to
   32 below his feet, cast with mask 2 (floors) through `World_Cast` @
   `0x4089e0` (nearest of up to ten hits). A hit counts only if
   `0x408c50` accepts it: the probe's end lies **behind** the polygon's
   plane, at most 0x400 deep (a 0x400-long segment along the normal from
   the end must cross the plane). That is the whole one-sidedness:
   `World_Cast` itself (`0x407740`) accepts crossings both ways.
   * same object and polygon, grounded: stay; refresh `up` (`0x409880`).
   * another polygon (or landing): **attach** (`0x411f55`): `+0x52 = 1`,
     `up` = the new normal; if the two polygons' normal angles
     (`+0x4c/+0x4e`) differ or either object is turned, the heading is
     carried across the **hinge**: axis = `old up × new up`
     (`0x431ef0`; parallel normals: `(0, −n.z, n.y)` or `(0, −n.z, n.x)`),
     expressed in the old polygon's frame (through the old object's
     stored rotation `+0x10`) and in the new one's; `heading +=
     atan(new) − atan(old)` (`0x430c10`). Then `(u, v)` = the probe's hit
     point in the new frame (`0x431ad0` with `+0x3a`), `h` = the stance
     height (slot 7's return value), `hvel = 0`, and slot 6.
   * nothing: **let go** (`+0x52 = 0`). `Mover_Step` then turns the last
     step into the flight velocity (`(pos − prev) × 256`) and the last
     yaw change into the spin (`0x411e5c`).
   Then `+0xe4` = the polygon's flags, ORed into the surface flags unless
   a wall brought lethal or affliction bits (`& 0x1e00f000`, `0x41215d`).

**So:** there is no adjacency search and no edge test. He crosses from
polygon to polygon because the probe under him hits the next one
(across objects too: the probe sees every gathered object); at an edge
with nothing under him he simply **falls**, with his walking momentum
(the brain's ledge ray, §3, is what keeps him away from edges). Steps
and slopes are whatever the probe reaches: up to 256 above his origin,
down to 32 below his feet. **Any floor-flagged polygon qualifies,
whatever way it faces**, and his up becomes its normal: section 1's
teleport pads face +y and he stands on them sideways; section 2 starts
him on a polygon facing −z, upside down; the section-0 capsule carries
him through its rolls glued to its floor. **Gravity is his own**: in
flight he keeps the last floor's normal (nothing but `Mover_Init` resets
`+0x24` to `(0, 0, 0x400)`, at `0x411bdd`) and falls along −up.

**The hull** (`0x418450`): the ten points (`creature+0xa60`, set in
`Creature_ReadMaster` @ `0x417570`, listed below) turned by the heading in the polygon's plane (`(x c − y s, x s +
y c, z)`), through the polygon frame and object rotation; segment `k`
runs from the current, progressively pushed origin `P` (from `P + up/4`
for the feet, `k = 9`) to `P + offset`, cast with mask `0x100` (walls,
any orientation). For a hit accepted by `0x408c50`: the wall's object
feels him (vtable `+0x24`, event 1), `P += penetration / 2`;
grounded, only the in-plane part of the push moves `(u, v)` (`h` stays)
and the world position is recomputed; airborne, `pos = P` and the
velocity, in the wall's frame, gets its into-the-wall component
replaced by the wall's **bounce** speed from `flags & 0xe00`: `0, 0x1000,
0x6400, 0xfa00, 0x15e00, 0x1c200` (1/256 units: 0 = stop, up to 450
units/tick). Every hit, accepted or not, adds its flags to the surface
flags (an affliction-1 polygon *replaces* them, `0x418848`) and hull
points 0 and 2 set the touch inputs. After the loop the game's injected
flags (`creature+0xae8`) replace the surface flags, the click hull is
read, and **the polygon's object he lives on feels him every tick**
(`0x41894a`), in flight too.

```text
hull (forward, left, up): (400,450,225) (-450,-450,225) (400,-450,225) (-450,450,225)
                          (±450,0,0) (0,±450,0) (0,0,450) (0,0,-450)
```

**Platforms**: grounded he is `(u, v, h)` on a polygon of an object, so a
moving, turning or rolling object carries him (slot 6 re-derives his
world position from its current transform every tick). The capsule
ride needs nothing else.

**Conveyors** (`0x41dc30`) — observed: a polygon whose flags have bits
2..3 (`0x4`, `0x8`, `0xc`) has a 16-byte scroll record
(`GameObject_BuildTextureScroll` @ `0x41d9e0`, at `+0x1d0` via the index
array `+0x1cc`): current offset `(u, v)`, previous offset, and its texel
basis. The basis (`0x41d6d0`) is the Jacobian of the polygon frame's
`(x, y)` with respect to the texture coordinates `(s, t)` of vertices 0,
1, 2 (`(0,127) (0,0) (127,0)`, set at `0x42320d`), each term × `-256.0`
(`0x46028c`; `0x460288` is the `1.0` of the reciprocal determinant) and
truncated to a short: `+8 = −256·∂x/∂s`, `+0xc = −256·∂x/∂t`,
`+0xa = −256·∂y/∂s`, `+0xe = −256·∂y/∂t`. The world direction does not
depend on which frame is used: it is `−∂world/∂(s,t) · Δ`.

The record is zeroed at build, so a polygon whose appearance was never
applied does not move him. Each applied frame goes through slot 0x14
(`0x41daf0`: **a new texture number resets the record**, prev = cur = the
frame's offset) then slot 0x18 (`0x41dbc0`: prev = cur, cur = the frame's
`(u, v)` read back through `0x423100`). Flip-books step once per tick, so
the step is the difference of consecutive frames; a stopped flip-book keeps
its last step. Grounded on the polygon, every tick (`0x418060`, after the
walk and the stance bob):

```text
du = fold((cur.u & 0x7f) − (prev.u & 0x7f))      fold: into −64..64
dv = fold((cur.v & 0x7f) − (prev.v & 0x7f))
shift = ((flags >> 2) & 3) − 1                  0x4: full, 0x8: half, 0xc: quarter
u += (m[+8]·du + m[+0xc]·dv >> 8) >> shift
v += (m[+0xa]·du + m[+0xe]·dv >> 8) >> shift
```

The sign makes him **ride the visible flow** of the texture. The usual
belt slides 8 texels a frame over polygons whose texel is 2220/127 =
17.48 units: **139 units a tick**, about 4.6× his walking speed (30).
Speeds on the CD (`0x6` polygons, max over a flip-book): section 1 has 6
to 140, section 2 mostly 117–146 with a few long skewed polygons far
faster (#84: 3,519; #174: 979; #152: 466), section 4 up to 193, section 5
up to 1,060, section 7 up to 151; all straight from the formula.

**Section 2's belt #197** (polygon 2, the underside of a 10360 × 2220 box
at z = 34780, facing −z, flags `0x6`; its only appearance has 16 looping
frames `v = 0, −8, … −120`, applied by its initial state 2) flows **across
its width** (t runs along `v1→v0`, world −y): basis `−4474` on `v`,
`(−4474 · −8) >> 8 = +139` per tick. Mendel starts hanging under #194's
polygon 6 at the same height, walks onto the belt by himself after about
two seconds and is carried off its y = −37000 edge in ≤ 16 ticks, then
falls "up" (along his down, +z) to his death. The port reproduces the
exe's arithmetic exactly (test `section2_belt_carries_him_at_the_exes_speed`),
so this is the original's behaviour, not a scale error; nothing in the
belt's state machine stops it (states 0/1 only follow event 1).

**Gravity** `g` per tick: `-7` normally, `+7` under affliction 12, `-2`
under 10, `0` under 11 (anti/low/zero gravity, inferred names). In
flight the constants are `0x700`/`0x200` on a 1/256 velocity: the same
7 units/tick² (`0x4180d9`, `0x41814e`).

**Placement** (`Mover_Place` @ `0x411bf0`): object id, polygon, `(u, v,
h)`, heading; `+0x52 = 0`, `up` = the polygon's normal, velocity 0, the
world position from the frame, then slots 8 and 5 at once: within the
probe's reach (`h ≤ stance + 32`) he stands on the polygon (or on what
the probe meets first), otherwise he falls along its −normal. Spawns
(`Game_SpawnCreature` @ `0x41c590`), teleports (`Game_RequestTeleport` @
`0x41c690`, executed by `Game_OnCreatureRespawn` @ `0x41c700` reasons
1/2) and respawns (reason 3) all go through it with six stored values:
the respawn point `Game+0x17efc4..+0x17efd4` and the pending teleport
`+0x17efd6..+0x17efe8` are both `(u16 obj, u16 poly, i32 u, v, h, u16
heading)`.

**Modes 0 and 1** (dying, rematerialising): `Creature_Update` @
`0x417da0` still runs the mover at the end (`0x417eb8`): he keeps riding,
settles on his new floor, or falls.

**Polygon flags** for every cast (`0x409680` → `0x42e0f0`): the
appearance's per-polygon flag array `+0x14`, which applying a frame
(`0x42df50` state change, `0x42e050` flip-book step) overwrites **only
when the frame's `enabled` byte is positive**. Disabled frames (all of
them flagged 0 on the CD) leave the polygon what it was: pad #83's
`[e1 0x2, e0 0x0, e0 0x0]` stays a floor.

**Object rotations** (needed to know which way a moving polygon faces):
a motion key's `rx` goes to the object's `+0xf2`, `ry` to `+0xf0`, `rz`
to `+0xf4` (the pose setters `0x422af0/0x422cb0/0x422e80`), and the
matrix is `Math_RotationMatrix` @ `0x430ce0(+0xf2, +0xf0, +0xf4)`
applied as `v · M` (`Object_UpdateMatrices` @ `0x4096a0`, `0x409880`):
`rx` is a yaw about z, `ry` a pitch about x, `rz` a roll about y. The
capsule ends its ride at `[512, 0, 0]`, a half *yaw*: upright over the
pad, where its floor goes away and Mendel drops onto #83. (Our first
port read `rx` as a pitch; with polygon gravity he then fell into the
sky.) The motion path's mode (`path+2`): 0 adds the frame to the pose
every tick (0x422e80; section 1's pads bounce ±100 that way), 1 = rest
position + frame (0x422cb0), 2 = the frame as is (0x422af0).

Inferred in the port: floating point instead of the exe's integer
frames; flip-book scroll deltas taken between consecutive frames of the
list (the exe keeps the last two applied offsets, one frame behind);
when a polygon's state has no enabled frame after a reload, it keeps
the flags it had; the click hull `MEnc` is posed from his frame by
`sim::angles_of` rather than the mover's angle formulas (slot 6).

### 2.3 Death, shatter, regeneration

`Creature_HandleSurfaceEvents` @ `0x419550` reads the surface flags:

| Bits | Effect |
|---|---|
| `0xf000` | lethal/teleporting surface: start the transition effect (`+0x8ec`, kind `(f>>12)&15`, variant `(f>>16)&7`), mode → 0 (dying) |
| `0x1000000` | only together with `0xf000`: **pain**, add `500 << 16` to all three brain costs when the effect fires |
| `(f>>19)&7` | what happens after the effect (`+0xaee`, the respawn reason below) |
| `0x800000` | delayed: the effect fires only after 80 ticks of contact (`+0x8f8`, reset to 80 whenever he stands on a harmless floor) |
| `(f>>25)&15` | start affliction `n` (`Afflictions_Apply` @ `0x405e20`) |

`Creature_Update` @ `0x417da0` runs the dying effect to the end, then calls
`Game_OnCreatureRespawn` @ `0x41c700(reason)`: 1/2 = teleport to the
pending destination, **3 = death: restore the world snapshot taken at the
last save pad and put Mendel back on it** (the brain is *not* restored: he
keeps what he learned), 4/5 = level transitions. Then mode 1 (rematerialise)
→ 2 (alive). `Game_TeleportCreature` @ `0x41c590` stores the respawn point.

### 2.3.1 The transition effect (`+0x8ec`) — observed

Port: `crates/engine/src/transition.rs` (the tables there are verbatim).

**Timeline** (`Creature_Update` @ `0x417da0`, asm, the decompiler lost
it): mode 2 runs `0x419550`, which on a lethal touch calls
`0x424870(kind, variant, creature+0x7d4)` (`+0xb0` = stance height − 10,
the effect's "floor" in model space, y down) and `0x4249b0` (start), and
sets mode 0. Every later tick, mode 0 calls the effect's update (vtable
slot 1, `0x424960`); the tick it returns 0 it calls `0x41c700(reason)`,
ends (reason 3) or ticks the affliction manager, starts the same effect
**backwards** (`0x4248c0`) and sets mode 1 — and mode 1's first update
runs that same tick (`0x417e8e`); when it returns 0, mode 2. Modes 0/1
never pose him (the effect works on a frozen copy); the mover still runs.

**Who starts which effect** (CD scan: `transition_probe`):

| Source | Kind, variant |
|---|---|
| lethal floors (`0xf000`, `(f>>16)&7`) | 1 v0 (s0), 1 v1 (s5), 2 v0 (most common), 2 v4, 2 v6, 3 v0 (s4), 3 v1 (the delayed agitating floors), 4 v0, 5 v0 (s4), 7 v0 (s1) |
| falling too long (`0x181000`) | 1 v0 |
| portal reactions (`0x41b140`) | kind 1 teleport `0xf1000` → 1 v7; kind 2 `0x161000` → 1 v6; gate (3) `0x251000` → 1 v5, reason 4; kind 5 `0x2d1000` → 1 v5, reason 5; kind 6 `0x1b2000` → 2 v3, reason 3; kind 7 `0x151000` → 1 v5 |
| spawn (`0x41c590` args 7/8 = `Strt` `ints[5..6]`, via `0x4198f0`: start + backwards, mode 1) | 3 v3 (s0 #57, the birth: plays as 3 v0x43), 1 v6 (s0 #83); every other `Strt`: kind 0 = no effect, mode stays 2 (the creature's initial `+0x8fa`) |
| predator bites (predators.md) | `PBal` 2 v3, `PFrk` 2 v0; section 7's `Pred` walls 2 v4, 4 v0 |

Kind 6 is never used.

**Manager** (`0x424520`, 0xb4 bytes): `+4` owner, `+8/+0xa` pre/post
delay, `+0xc/+0xe` kind/variant, `+0x10/+0x12` queued follow-up,
`+0x14` current sub-effect, `+0x18..+0x30` the seven sub-effects (kinds
1, 2 at `+0x1c`, 3, 4, 5, 6, 7), `+0x38` the pending sound cue (a copy of
the template `+0x74`: sound, bank `Game+0x17ee7a` = the world bank,
pitch 1.0, distances 3000/30000, volume 256), `+0xb0` floor. `0x4248c0`
(backwards): variant + 0x40; kind 3 → v0x40 unless v0x43; 4 → 2 v0x41;
5 → 2 v0x44; 6 → v0x40; 7 → 2 v0x40.

**Sub-effect base** (`0x425680`, vtable `0x460ba0`): start `0x4256f0(n)`
copies the owner's polygons and vertices into a private mesh (`0x4105d0`:
one vertex per polygon corner, one material per polygon, reset from the
section material `0x4056d0`) and swaps it into the object (`0x402280..`);
update `0x425800` = `pre` idle ticks, `n` steps (the pending cue plays
before the first, `0x4255a0`), `post` idle ticks, then `0x425790` gives
the object its own mesh back and the update returns 0. Per-polygon
material writes: `0x4111b0(b)` (m0/m1, the affliction encoding),
`0x411290` (m2), `0x411300` (m3); `0x411340` shows/hides (`+0x64`, which
the draw `0x402490` tests); object `+0x2a` = back-face culling.

| Kind | Start (args) | Step | What it does |
|---:|---|---|---|
| 1 fade | `0x40fa50(rate, n, flags)`, duration `N·rate + n − 1` | `0x40fd40` / `0x40fe50` (flag 2, backwards) / `0x40ff60` (flag 0x100) | every `rate` ticks the next polygon (order: flag 1 random, 0x10/0x20/0x40 sorted by its highest x/y/z, 0x80 descending) starts at brightness `0x7fff<<1` (whiter than normal) and loses `0x7fff/n` a tick; below 5000 it is hidden. Backwards: hidden, then shown and brightened from black. Flag 8: dithered (random 0 or 0xffff against the value). Flag 4: back faces culled. **Bug**: the sorted lists store `i + 1` (`0x40f780`), so polygon 0 is never picked and the last pick writes past the array |
| 2 burst | `0x411640(dur, a, b, spin_v, spin_a, flags, floor)` | `0x411810` | every polygon moves out from its centre `c` (mean of its corners + `rand&0xff − 0x7f` per axis; flag 8: `(0, ȳ − floor, 0)`) to `c·k/256`, `k = A + (r·(A − 256) >> 5)`, `r = rand & 0xff` per polygon, `A` from 256 to `a` (8.8); flag 4: tumbling by `rnd(0..511)·(B − 256) >> 8`; flag 0x20: vertical spread / 8; flag 0x10: all white (m0..m3 = 0xffff) on the first step (backwards: the last); flag 2: random flicker; flag 1: backwards (from `a` to 256). Spins the owner's heading (`0x410f70` writes `*(creature+0xa0)` = the mover's `+0x50`): speed and acceleration ×256; backwards it starts `n·v + n²·a/2` ahead and comes back to n·a/512 off. Culling off |
| 3 squash | `0x409cb0(dur, flags, floor)` | `0x409da0` → `0x409e90(s, axes, about_floor)` | flag 2: y scaled from 256 to 0 about the floor; 0/4/8: flat at once; 1 (backwards): `s = 256·(1 − cos 6v · e^−v)`, `v` 0 → 5, a springy rise; 0x10: all axes 0 → 256 about the origin plus random flicker |
| 4 jolt | `0x4113b0(dur, mask, 0)` | `0x4113e0` | every corner `+ (rand & mask) − mask/2` per axis each tick; brightness alternates `rand + 0x7fff` / `rand` (m0..m3); the last two ticks 0x6000, mask 0x1f. v0..v2 queue a burst (2 v4 / 2 v3 / 2 v0) |
| 5 melt | `0x416850(45, floor)` (base runs 47) | `0x416980` | brightness `+60000/45` a tick up to 0xea60 (white glow) for 45 ticks; then sound 0x24 and every polygon tumbles about its centre (`rand&0xf − 8` per axis × t, frozen at its first floor contact) and drops (speed +1 a tick); below the floor its speed halves and turns up; the base is held at 2 ticks until every speed is in −1..1 |
| 6 stretch | `0x42dc00(dur, flags, floor)` | `0x42dca0` → `0x42dd80(s)` | y by `s/256` about the floor, x and z by `√(256/s)`; flag 1 a damped `1 + sin(9x)/(2x²)` wobble; flag 2 hides everything (v1). Not on the CD |
| 7 shatter | `0x429bc0(g, dA, dB, flags, clink delay, floor)`, base 4 | `0x429d10` | polygons fly from `(0, ȳ − 4·floor, 0)` outward (`A += dA`), tumbling (`B += dB`), falling (`vy += g`); a corner below the floor lands the polygon (state 1 → frozen), is clamped to it, and plays sound 0x22 at pitch 0xb000 ± 0.25 once the clink delay ran out; held alive until all landed. Post 30 |

**Variant tables** (`0x424ab0`, `0x424e20` + jump table `0x4250f8`,
`0x425140`, `0x425280`, `0x425370`, `0x425480`) — sound (1-based world
bank) / flash (`0x438830(id, 200)`):

* kind 1: v0 (0.3, 8, 0xd) 1/3; v1 (0.0001, 8, 5) 2/–; v2 (0.14, 4, 0x190)
  3/–; v5 (0.7, 12, 0xa4) sound 6 at once (pitch 0xd000, 10000/20000) /
  4 (hold 40, then **white**, which stays); v6 (0.3, 10, 0x24) 7/–; v7 (0.1,
  6, 0x94) 8/–; 0x40 (0.3, 8, 0xf) 1/3; 0x41 (0.0001, 8, 7) 2/–; 0x42
  (0.1, 3, 0x192); 0x45 (0.7, 20, 0xa6) **pre 50**, sound 6 (0xb000,
  volume 190) / 5 (white → normal); 0x46 (0.3, 10, 0x26) 7; 0x47 (0.1, 6,
  0x96) 9.
* kind 2 (dur, a, b, spin v, spin a, flags) sound/flash: v0 (100, 2000,
  2000, 0, 0, 0x16) 10/6; v1 (60, 700, 1200, –, –, 0x14) 11/7; v2 (100,
  1400, 800, 0x34) 12/8; v3 (60, 2000, 500, 0x32) 13/9; v4 (80, 1800, 1,
  0x32) 14/10; v5 (40, 300, 500, 6.0, 0, 0x14) 15/11; v6 (80, 1500, 800,
  0x1e) 16/12; backwards all sound 18, no flash: 0x40 (60, 600, 500, 0,
  0.5, 0x19), 0x41 (60, 300, 500, 0x0d), 0x42 (60, 500, 800, 10.0, 0,
  0x31), 0x43 (60, 500, 400, 0.4, 0, 0x37), 0x44 (60, 500, 800, 0, 0.5,
  0x37), 0x45 (60, 300, 600, 0, 0.5, 0x15), 0x46 (60, 400, 600, 0x1d).
  Every kind 2 cue has pitch variation 0x4000.
* kind 3 (dur, flags): v0 (2, 0) 0x13/0xd post 50; v1 (60, 2) 0x14 post 40;
  v2 (40, 4) 0x15/0xe; v3 (1, 8); 0x40 (70, 1) 0x17; 0x43 (60, 0x10).
* kind 4 (dur, mask): v0 (30, 0x3f) 0x18/0xf → 2 v4; v1 (62, 0x1f)
  0x19/0x10 → 2 v3; v2 (21, 0x7f) 0x1a/0x11 → 2 v0; v3 (38, 0x3f) 0x18/0xf
  post 60.
* kind 5 v0: sound 0x23 (then 0x24 when it collapses).
* kind 6: v0 (65, 0) 0x1c → 6 v1; v1 (0, 2) 0x1e/0x13 post 60; 0x40/0x41
  (100, 1) 0x1d/0x12.
* kind 7 (g, dA, dB, flags, delay) post 30: v0 (6, 8, 5, 0, 30) 0x1f/0x14;
  v1 (30, 16, 5, 0, 12) 0x20/0x15; v2 (8, 2, 5, 4, 6) 0x21/0x16; v3 (1, 3,
  1, 4, 6) 0x1f/0x16.

The flash scripts 0x00..0x16 are in `flash.rs` (`Palette_InitScripts` @
`0x438290`).

**Durations that matter** (ticks of 1/24 s): lethal 2 v0 = 100 + 60
back; 3 v1 (agitating floors) = 100 + 70; the gate's 1 v5 ≈ `0.7·N + 11`
(N = his ~80 visible faces: 65 ticks), the screen white from tick ~50.

**Footage** (QLiZsijShbY, section 0): the gate at the end (t ≈ 593.5 s)
bleaches the screen to white over ~0.5 s (flash 4's 10 frames), holds it
~0.7 s, then the time screen fades in (flash 0) — the port's 51 → 65
ticks match. The birth (t ≈ 15.2 s): a small, flat-shaded violet Mendel
grows in the beam (3 v0x43: scale 0 → 1 with random flicker). No death
by a lethal floor is in the video.

inferred (port): the per-polygon rotation of `0x410f90` (matrix
`0x430430`, angles in 1024ths) uses the object rotation formula; kind
1's sort keys are taken at the start (the exe: when the creature is
built); `m2`/`m3` writes are not modelled (only the m0/m1 brightness); a
saved game keeps no running effect (it ends at the load); when the host
loads a section after a gate the effect's reverse is moot.

Two more rules at the top of `0x419550` (observed):

* under affliction 1, a surface with bit 22 (`0x400000`) loses its lethal
  bits, and one with bit 29 (`0x20000000`) loses its affliction bits;
* **falling too long kills**: airborne, `+0x8f6` is set to 65 ticks (130
  if `+0x134 & 0x10`: the flags of the polygon he lives on, the mover's
  `+0xe4`, written by `0x412157` every tick), `+0x8f4` counts; past the
  limit the surface becomes `0x181000` (lethal kind 1, then death).
  Touching a `0x10` surface resets the count. On landing, more than 5
  airborne ticks is event 4 (the landing cue).

Flags handed over by the game (`creature+0xae8`, `0x4198b0`, e.g. a
predator's bite through `MEnc`, see predators.md) join the surface flags
at the next touch.

### 2.4 Afflictions — observed (effects, durations), inferred (names)

`creature+0x8e8` is an *active effect manager* (`ActiveEffects_ctor` @
`0x405820`, 0xd8 bytes, created by `Creature_ReadWorldSection`); the
current affliction is the short at `creature+0x8fc` (set by `0x4198d0`,
read by `0x4198e0`). Port: `crates/engine/src/affliction.rs`.

**Trigger**: only surfaces. `Creature_HandleSurfaceEvents` calls
`Afflictions_Apply` @ `0x405e20((f >> 25) & 15, 0)` whenever the surface
flags have bits in `0x1e000000` — every tick he touches such a polygon.
Apply ends a *different* running affliction (restores his polygons,
`0x405f10`), stores the number, and calls `Afflictions_Select` @
`0x406350`, which starts that affliction's effect object (vtable slot 1)
with its duration and plays its start sound and palette flash.

**Lifetime**: each effect (base `0x406840`; tick `0x4068f0`) runs a
lead-in delay, `duration` ticks of its visual (slot 4), a lead-out delay,
then ends (slot 2). Both delays are 0 for all twelve, so the affliction
lasts exactly `duration` ticks and is gone on the next. The manager's
tick (`0x405fc0`) runs in `Creature_Update` after the cost and before the
surface events, and first restores every polygon's colour ramp. Effects
1, 3, 4, 5 re-arm their duration on every Apply (standing on the polygon
keeps them going); 2, 6, 10..12 (`0x4241b0`) and 7..9 (`0x424390`) only
start when idle. Death (respawn reason 3, `0x417e68`) and loading a saved
game (`Creature_Read` → `0x405f10` at `0x419967`) end it.

| # | ticks | effect class | what changes (where it is read) | start sound / flash |
|---|---|---|---|---|
| 1 | 1440 | `0x42b150` | **shield**: lethal surfaces with bit 22 and affliction surfaces with bit 29 are ignored (0x41955b); his ledge ray sees polygons with bit 22 (`flags |= 0xc0`, 0x419290); skin: last texture slot, `+0x5c += 8`, brightness from four triangle-wave oscillators plus noise (`0x42b200`) | 37 / `0x17` (blue flash) |
| 2 | 1440 | `0x42db50` | actuator steps (4, 7) (`0x418970`); no visual | — |
| 3 | 1440 | `0x42cb90` | **speed**: gait 53 (`0x4189c0`), actuator steps (1, 6); visual `0x42cc00` restores ramps every 20..23 ticks (no visible change) | 39 / `0x18` (strobe) |
| 4 | 4320 | `0x4072b0` | **camouflage**: every polygon takes the texture and ramp of the floor polygon he stands on (`0x407350` via the mover's `0x4124a0`); `PBal` stops chasing (`0x415237`) | 40 / `0x19` |
| 5 | 240 | `0x414e10` | **blind**: both eyes 0 (`0x418fe8`); his four first polygons by mean x (the front, inferred) go dark (ramp 6000), fading back over the last 24 ticks (`0x414f80`) | 41 at half pitch / `0x1a` (white-out) |
| 6 | 720 | `0x424160` | **confused**: each eye holds 0 or 256, re-drawn when `seed & 0xf0000 == 0` on the brain's LCG (1/16 → 0, else 1/16 → 256; `0x418ff9`) | 42 / `0x1b` (negative screen 30 s) |
| 7, 8, 9 | 96 | `0x424340`, `0x407420`, `0x406ce0` | **agitated**: gait 40, actuator steps (4, 7), event 1 (the agitated cue, sounds 1/2) every tick; while ≥ 49 ticks are left, every `rand & 31` ticks a palette flicker (7: `0x1d..0x1f` warm, 8: `0x21..0x23` cold, 9: `0x25..0x27` green, priority 0) and a crackle (world sounds 43..45 / 46..48, pitch variation ±0.5 / ±0.25, random volume) | — |
| 10, 11, 12 | 100 | `0x42f280`, `0x42f2d0`, `0x406c90` | gravity −2, 0, +7 instead of −7 (`0x418060`) | 53 / 54 / 55, `0x28..0x2a` |

Sounds are 1-based world-bank sounds (manager cue template `+0x9c`:
bank `Game+0x17ee7a`, pitch 1.0, distances 2000/20000, volume 256). The
banks of all eight worlds share those slots; the sounds of 4, 6, 10, 11,
12 are **empty slots** on the CD (test `affliction_sounds_exist_in_every_world`).
The flashes are palette scripts (`Palette_InitScripts` @ `0x438290`,
played by `0x438830(id, priority)`, stepped once a tick by `0x437690`):
see `crates/engine/src/flash.rs`.

**On the CD** (`hazard_probe`): only 1 (section 1 #103, `0x02000100`),
3 (section 1 #166, section 5 #176, `0x06000100`) and 7/8/9. Every 7/8/9
polygon is also a *delayed* lethal floor (`0x2ed93002`: affliction 7,
lethal kind 3, after 80 ticks, bit 29): standing there he is agitated,
then dies. 2, 4, 5, 6, 10..12 are unused.

**Brain**: afflictions never touch the cost directly; they change what
the brain senses (5, 6, 1), how fast its muscles move (2, 3, 7..9) and so
what the cost measures. Pain stays the lethal-surface flag `0x1000000`.

inferred (port): the start sound and flash are played when the affliction
starts, not on every Apply (the exe replays them each tick of contact);
a ramp brightness above 0x7fff is drawn as a brighter light.

## 3. Senses — observed

`Creature_SenseInfrared` @ `0x418b00`, every tick after the mover:

* Two eyes, **left** (input `creature+4`) and **right** (`creature+8`).
  An eye only fires if its input is still 0 (i.e. no touch this tick) —
  touch wins over sight.
* Each eye casts **three parallel rays**, direction heading ± `a` in his
  polygon's plane (through its frame and the object's rotation), `a =
  100 - posture*50/256` (in 1/1024 turns: about 35 to 18 degrees),
  length `R = 1500 + posture*500/256`: the posture output also controls
  how far and how wide he looks. The origins stand on his **feet**
  (`pos − up · stance_height`): `+up/4` (256, high), `+up/64` (16,
  middle) and `−up/32` (32 *below* the floor's surface: the ledge ray).
* High and middle rays: `Creature_CastIrRay` @ `0x4190f0`, the **nearest**
  polygon of mask `0x40000100`; it counts if its flags have `0xc0`
  (IR-visible) or `0x40000000`, otherwise it hides what is behind.
* The low ray: `Creature_CastIrLedgeRay` @ `0x4191f0` takes all its hits
  (`World_CastAll` @ `0x408b20`, nearest first, ten at most), so it meets
  the sides of the slab he walks on from inside. IR class `0x40` (floor
  sides, walls `0x340`): an edge; if the next hit is less than 251
  further it was a seam between slabs (keep going), otherwise the edge
  is a drop and is seen; a last lone edge is seen too. Class `0x80` or a
  hazard: seen at once. Class `0xc0` or 0: the ray stops unseen. Under
  the shield (affliction 1) bit-22 polygons count as `0xc0` (`0x419290`).
* Value of a hit: `clamp(256 - (dist - 656) * 256 / (R - 450), 0, 256)`:
  256 = touching, 0 = nothing in range. The eye reports the ray with the
  smallest distance (`0x419070`; 10,000 = nothing).
* Falling (`+0x8f4 != 0`, limit not 130): both eyes read 256 (`0x41906a`).
* Side effects for sounds: ray hit flags `0x40000000` → event 2,
  `(flags & 0x380000) == 0x180000` → event 8; `|L - R| > 230` → event 16.

**Right click** (observed chain, inferred names): the section's `MEnc`
object (one per section, stored by the object factory at `0x41c0a7` into
`game+0x17efbc`) is a click hull glued to Mendel each tick
(`Creature_AttachTouchHull` @ `0x417d60`). `MEnc_PopTouchSide` @
`0x4261a0` maps the clicked polygon (0..7) to 1..4; sides 1/3 hold
`input L = 256`, sides 2/4 hold `input R = 256`, for **4 ticks**. So a
right click is felt exactly like bumping into something on that side.

The third brain input (`agent+0xc`) is never written (always 0): it acts
as a per-state bias.

## 4. The NERM brain — observed

### 4.1 Data

Three controllers (`Agent+0x2c/0x30/0x34`, class `Nerm`, 0x24e0 bytes,
`Nerm_ctor(3 inputs, 200 samples)` @ `0x427aa0`), wired by
`Nerm_ConnectInput` @ `0x427da0`: input slot 0 = right eye (`agent+8`),
slot 1 = left eye (`agent+4`), slot 2 = `agent+0xc` (0).

Serialised by `Nerm_Read` @ `0x428070` / `Nerm_Write` @ `0x428110`
(9,414 bytes; see the table on `creature::Nerm`): `state`, the transition
table (`state -> {stay, down, up}` clamped at 0 and 9), output levels
(even states 0, odd states 256), a probability cache, the genome
`u8[30][10][3][9]`, knots `0,32..256`, `exp_table[i] = floor(2^(i/24))`,
30 fitness values, the current individual, `input_count = 3`,
`samples = 200`.

### 4.2 One decision (every tick)

```text
Nerm_Step (0x427dc0) = Nerm_RunMarkovChain(Nerm_ComputeTransitionProbs(inputs))

ComputeTransitionProbs:                         # 0x427c50
  for s in 0..10:
    for j in 0..3:                              # stay, down, up
      x = input[(3*s + j) % 3]                  # = input j
      b = x*8 >> 8                              # knot segment
      g = genome[individual][s][j]
      level = x == knot[b] ? g[b] : g[b] + (x-knot[b])*(g[b+1]-g[b])/(knot[b+1]-knot[b])
      w[j] = exp_table[level]
    p[s][j] = w[j] * 32767 / sum(w)

RunMarkovChain:                                 # 0x427de0
  acc = 0
  repeat 200 times:
    r = rand15()
    walk the cumulative p[state][0..2]; first one > r (or the last) gives next state
    acc += output[state]
  return acc / 200                              # 0..256
```

`rand15()` is the engine-wide LCG at `0x468640`:
`seed = seed*1103515245 + 12345; return (seed >> 16) & 0x7fff`.

Verified on the CD: brain 0's cached probabilities are reproduced exactly
by this formula with inputs `(32, 0, 0)` for all three controllers
(`brains_match_retail_cd` test), which also confirms that the third input
is 0. (Only 4 of the 15 caches match a simple search; the others were
probably written mid-evolution: open question.)

### 4.3 From controller to muscle

`Agent_Think` @ `0x425dc0` (called by `Creature_Think` @ `0x418970`):

```text
o0, o1, o2 = Nerm_Step(c0), Nerm_Step(c1), Nerm_Step(c2)
act38.target = o0;  act3c.target = o1;  act40.target = o2
repeat n38 times: Actuator_Step(act38, 0, 256)
repeat n3c times: Actuator_Step(act3c, 0, 256); Actuator_Step(act40, 0, |2*act3c - 256|)
# (n38, n3c) = (1,2) normally, (1,6) affliction 3, (4,7) afflictions 2/7/8/9
```

So turning only happens while walking. An **actuator** (`Actuator_ctor`
@ `0x42aa00(rate, shift=3, start)`: act38 = (13, 256), others = (20, 128))
is a slew-limited servo in 16.16 fixed point (`Actuator_Step` @ `0x42aa50`).
The constructor stores `rate << 16` (checked in the listing), so the speed
limit is `rate` whole units per tick: 13 for posture, 20 for speed/turn.

```text
step(bias, dt):                                   # dt = 256 is one tick
  vmax = (rate << 16)*dt >> 8;  vel = (vel << 8) / last_dt * dt >> 8;  last_dt = dt
  prev = value
  if target > pos:  acc = vel < vmax  ? (vmax - (vel < 1 ? 4*vel : vel)) >> shift : 0
  if target < pos:  acc = vel > -vmax ? (vel < 0 ? -(vmax + vel) : -4*vel - vmax) >> shift : 0
  if target == pos: acc = 0
  vel = clamp(vel + ((acc - bias)*dt >> 8), -vmax, vmax)
  pos += vel; clamp pos to [0, 256<<16] (velocity zeroed at the walls)
  value = pos >> 16
```

### 4.4 Learning

`Creature_AccumulateCost` @ `0x419470`, every tick (`L` = left eye,
`R` = right eye, `turn` = `2*o2 - 256`, `speed` = `2*act3c - 256`):

```text
w = (L+R)*416/512 + 96                         # proximity weight
count0 += w; count2 += w; count1 += 256
cost1  += (L+R)/16 - speed + 256               # speed controller: go fast, stay clear
d = L > R ? |turn + L|*512/(L+256)
  : L < R ? |turn - R|*512/(R+256)
  :         ||turn| - R|*512/256
cost2  += w*d/512;  cost0 = cost2              # posture and turning share one cost
pain (lethal surface with flag 0x1000000): cost0,1,2 += 500 << 16
Agent_LearnIfDue (0x425e80): for each k with count_k > 65536:
    Nerm_ReportFitnessAndEvolve(c_k, cost_k * 65536 / count_k); cost_k = count_k = 0
```

The turn cost says "turn away from the side that sees more" (a turn of
`-L` when the left is brighter costs nothing).

`Nerm_ReportFitnessAndEvolve` @ `0x427ea0`:

```text
fitness[individual] = cost; individual += 1
if individual == 30:
    rank = argsort(fitness)                    # selection sort, lowest first
    for child in rank[15..30]:                 # the worst half
        for every gene g (270 per individual):
            parent = rank[rand15() % 15]
            genome[child][g] = clamp(genome[parent][g] + rand15() % 21 - 10, 0, 255)
    individual = 0
```

That is the whole of "NERM": a (15+15) evolutionary strategy with uniform
crossover over Markov-chain controllers. **Neurosis** has no variable of its
own: repeated pain inflates the cost of whatever genome was active, so the
surviving genomes are the ones that keep Mendel still or turning away
("cagey"). A safe environment lets the population drift back.

### 4.5 Initial brain, age, `.mnd`

* New game: the Preferences "age" slider (0..255) becomes a level at
  `0x4171ad`: 0 → 1, 1..84 → 2, 85..169 → 3, 170..254 → 4, 255 → 5.
  `Game_LoadCreatureBrainForSection` @ `0x41ced0(level)`: level ≤ 1 →
  `Agent_ResetBrain` @ `0x425d80` (`Nerm_Init` @ `0x427ac0`: genome from
  the LCG, knots, exp table); else the brain is loaded from
  `creature_overlays[level - 1]`. Overlay 0 is never used by this path.
* `.mnd` import/export (`UI_ImportMendelNerm` @ `0x41f5e0`,
  `UI_ExportMendelNerm` @ `0x41f880` → `Game_ImportNerm` @ `0x41d2a0` /
  `Game_ExportNerm` @ `0x41d2c0`) read/write exactly three `Nerm` records:
  a `.mnd` file is a 28,242-byte overlay. Import: observed; export uses the
  mirror writer with the same field list (sizes inferred).
* Save games (`Creature_Read` @ `0x419950`) additionally store the agent's
  inputs, costs and counters (`Agent_Read` @ `0x425f50`), the actuators,
  the mover, the pose and the creature's modes.

## 5. The 468-byte trailer — observed layout, inferred meaning

It is **camera** data, read by `Camera_ReadMotionProfiles` @ `0x414060`;
`Creature_ReadWorldSection` @ `0x4177d0` reads nothing from the section (on
the first section only it registers Mendel's per-polygon material copies,
§1.3; then it seeks to the global creature sound set).

```text
u32 unknown            1520 in every section
u16 n                  33
n x { i32 a; i32 b; i16 c; i16 d; i16 mode }      14 bytes
```

Identical in all 8 sections. `a`, `b` are the look-at and eye tracker
constants (16.16, 0.6..0.95), `c`, `d` the eye mover's acceleration and
top speed (7..30, 150..700 units/tick), `mode` 0..11 the key's reference
frame. A camera key's second byte picks the entry: see objects.md §6.

## 6. Sounds — observed

`Creature_ReadSoundCues` @ `0x417ab0` builds five cues (count + sound ids
into the 11-sound creature set, pitch 1.0, distances 15000/500/16000,
volume 256, attached to Mendel's position). `Creature_HandleSurfaceEvents`
plays them:

| Cue | Sounds | Trigger | Then silent for |
|---|---|---|---|
| `+0x978` | 5 | landing after more than 5 airborne ticks (event 4) | 5 ticks (own timer `+0xa32`) |
| `+0x900` | 1, 2 | agitated (event 1: afflictions 7/8/9, every tick) | 5..36 ticks |
| `+0x93c` | 3, 4 | eye ray hit a `0x40000000` polygon (event 2) | 20..51 ticks |
| `+0x9b4` | 6, 7 | eye ray hit a polygon with `(flags & 0x380000) == 0x180000` (event 8) | 20..51 ticks |
| `+0x9f0` | 8, 9, 10, 11 | `|L - R| > 230` (event 16) | 20..51 ticks |

The last four share one silence timer (`+0xa30`) and one plays per tick,
in the table's order; the extra `0..31` ticks come from the brain's LCG
(`0x468640`), so playing a cue advances his random sequence. A random
member of the cue is chosen by the sound system (CRT `rand`, `0x42b5f0`).

## 7. The tick

`Creature_Update` @ `0x417da0(frac)` is the creature's per-tick entry
(vtable slot 7). While alive (mode 2):

```text
mover.step()                     CreatureMover_Step @ 0x417f40 (+Integrate, +CollideAndTouch)
Creature_SenseInfrared()
Creature_Think()                 -> Agent_Think
Creature_ApplyMotorOutputs()     -> ComputeMotion + SetPose
Creature_AccumulateCost()        -> Agent_LearnIfDue
attach MEnc hull to the new position
afflictions.tick()
Creature_HandleSurfaceEvents()   pain, death, sounds
```

Dying (mode 0) and rematerialising (mode 1), it runs the effect and the
respawn logic, then `mover.step()` all the same (`0x417eb8`).

`frac` (stored at `creature+0xae0`) interpolates rendering between two
simulation states (`CreatureMover_Step` blends previous/current transforms
with it). **The simulation rate itself was not found**: the only fixed
delay seen is a 17 ms busy-wait in a palette fade. Open question.

## 8. What the runtime port verified (crates/engine/src/creature.rs, walker.rs)

Running Mendel in the live simulation confirmed or corrected:

* **Up is his floor's normal.** The world's +z is only the default
  (`Mover_Init`); the first port assumed Y-down (he walked on the sides
  of pads), the second a world-up ray-cast controller with a 60° slope
  limit, "no new floor unless it faces up", an edge rule and a fall-back
  to world up in flight. All of that is gone: the polygon walker (§2.2)
  stands him on whatever floor the probe under him finds.
* **Model origin**: the posed feet end exactly at `y = 41 + stance*166/256`
  (41 / 124 / 221 measured at stances 0 / 128 / 255): the origin floats
  `h` above the floor. Model axes after the load transform are forward
  `+x`, down `+y` (towards the feet), left `+z` (hull point 0, left `=
  +450`, drives the *left* input); the model's `−y` maps to his up.
* **Floors are flag bit 1** (`0x2`), walls bit 8 (`0x100`); save pads
  (`0x22`) are floors too, so standing on one puts `0x20` in his surface
  flags (no extra test needed). Disabled flip-book frames do not change a
  polygon's flags (§2.2), which is why pad #83 stays a floor.
* **Portal offsets are polygon-local**: a portal's `(u, v, h)` is measured
  from the first vertex of its polygon `ints[0]`, along that polygon's
  frame, `h` along its normal (Strt #57: `(383, 922) + (-387, -922)`;
  pad #83: exact). It is handed to `Mover_Place` as is.
* **Tilted pads**: section 1's 20 teleporters onto pads facing +y (#49..
  #64, #66, #73, #117, #163) land him on the pad's polygon, his up = +y,
  at his stance height, and he walks on (test
  `teleports_onto_tilted_pads_in_section1`). Section 2's `Strt` #194
  stands him upside down (up −z) on its polygon 6, which the probe meets
  74 units before the portal's polygon 1 (test
  `section2_starts_on_its_downward_start_polygon`).
* **One-sided polygons** (observed): the mover's hit test `0x408c50`
  accepts a polygon only from the side `(v1-v0) x (v2-v0)` points to.
* **The intro is a ride**: Mendel is born standing on the `Strt` disc #57,
  the floor of the birth tube #54. The disc hands the floor bit down its
  stack of four polygons, then to the docked capsule #3 (floors 25, 27,
  29 in turn); its 1,992-frame path rolls him through the sky glued to
  its floor (sideways, at times), arrives upright (`[512, 0, 0]` is a half
  yaw, §2.2) and its last state removes the floor: he drops ~1,000 onto
  the arrival pad #83, after ~133 s.
* **Touch wakes folders**: dropped on platform #77 (folder #75), his event
  1 switches the sleeping upper-level folder #145 back on within 20 ticks.
  Verified (test `mendel_wakes_the_world_in_section0`).
* **Pads repeat**: portal #83 in state 3 requests a teleport *every tick*
  until it feels Mendel; the port ignores requests whose destination is
  within 1,500 units (inferred: in the exe a request only becomes a
  teleport through a lethal-surface reaction with reason 1/2).
* **Every section spawns him unaided** (no "wake every machine"): the
  level load enters all initial states before the first tick
  (`World_Read` @ `0x4222d0`, objects.md §2), so section 2's folder #193
  sees #178 in state 1 and keeps `Strt` #194 awake; he appears on its
  polygon 6 at tick 100 (tests `every_start_spawns_mendel_without_waking_all`,
  `section2_spawns_mendel_on_194`). Spawn ticks: sections 0..7 = 333, 12,
  100, 60, 60, 20, 50, 1067.
* **Conveyors push hard, faithfully**: section 2's belt #197 sweeps him
  off its side at 139 units/tick within 16 ticks of stepping on it, and he
  falls to his death. Checked against the exe (§2.2 Conveyors): basis,
  shift, folding and the one-frame-per-tick step all match.
* **Disabled frames are drawn**: a frame's `enabled` byte only gates the
  flag copy (docs/re/worlds.md); the port had hidden disabled frames, so
  flip-book polygons (pads, belts) vanished on all but their first frame.
  Pick rays now read the flag array too, as `0x408730` → `0x409680` do.

## Open questions

* Game tick rate (and who calls `Creature_Update` with which `frac`).
* Polygon attributes `unk0/unk1` (`+0x5c/+0x5e`); exact trig-table scale
  (the tables at `0x472858`/`0x473058` are built at run time).
* Why only 4 of 15 cached probability tables match a 2-input search
  (stale caches written mid-evolution, or non-zero third input at
  authoring time?).
* Camera profile fields `a, b, c, d`; the `u32 1520`.
* What `+0x5c += 8` does to a polygon (affliction 1). (The transition
  effect is §2.3.1; open there: the exact matrix of `0x430430`, and the
  meaning of the cue's `+0x22` that kind 7 sets to `50 − rand & 31`.)
* Scroll record on the first tick of a new state: the exe's "previous"
  offset is the old state's frame (unless the texture changed); the port
  takes the new list's last frame (looping) to avoid saving the record.
  (The earlier note that `0x41dbc0` lags a frame was wrong: it reads the
  frame record after `0x42e050` advanced it.)
* The `+0x11c..+0x134` filter of the additive pose setter `0x422e80`
  (key components equal to stored values are zeroed): who sets it.
* `World_Cast`'s camera and predator users: do they reject back faces?
  (The port's camera and predator probes keep front faces only.)
* Overlay 0 is unreachable from the age slider: used elsewhere?
