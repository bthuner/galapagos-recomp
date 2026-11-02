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
polygon_count x { u16 texture; u16 color_ramp; u16 unk0; u16 unk1 }
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
section colour-ramp index that `Creature_ReadWorldSection` @ `0x4177d0`
re-registers with the renderer each time a section loads. `unk0/unk1`
(polygon `+0x5c/+0x5e`) are unknown.

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

## 2. Locomotion — observed (constants), inferred (some geometry)

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

### 2.2 Mover, gravity, collisions

`CreatureMover_Integrate` @ `0x418060` (mover vtable slot 4):

* grounded (`creature+0x52 != 0`): the position is kept in the
  coordinates of the polygon he stands on (`+0x44`, `+0x48` in its plane,
  `+0x4c` = height along its normal). `u += cos[heading] * forward >> 14`,
  `v += sin[heading] * forward >> 14` (tables `0x472858` = cos and
  `0x473058` = sin, both × 16384, filled by `Math_InitTables` @
  `0x4308c0` with `FSIN`/`FCOS`), `heading += turn_d` (10-bit angle),
  the height's velocity `+= g`, height clamped to the stance height;
  then the world's move-with-collision (`0x41dc30`). On a floor facing
  +z the polygon frame (`Math_FrameFromNormal` @ `0x4312b0`) is the
  world's, so in world terms the heading direction is `(cos h, sin h, 0)`.
* airborne: ballistic, `vel += up * g >> 10` (`up` = `+0x24..+0x2c`,
  his up vector), `pos += vel >> 8`, spin. Leaving a surface resets the
  contact normal to `(0, 0, 0x400)` (`0x411bdd`): **the world is Z-up**,
  gravity pulls along −z.
* gravity `g` per tick: `-7` normally, `+7` under affliction 12, `-2`
  under 10, `0` under 11 (inferred: anti/low/zero gravity zones).
  Airborne the constants are `0x700`/`0x200` added as `up * g >> 10` to a
  velocity kept in 1/256 units: the same **7 units/tick²** in both modes
  (verified in the listing at `0x4180d9` and `0x41814e`).

`CreatureMover_CollideAndTouch` @ `0x418450` (slot 9), every tick:

1. clears both sensor inputs (`creature+4`, `+8`) to 0;
2. tests 10 hull points (`creature+0xa60`, plain `x,y,z` triples set in
   `Creature_ReadMaster` @ `0x417570`, in his frame: forward, left, up):
   `(400,450,225)`, `(-450,-450,225)`, `(400,-450,225)`,
   `(-450,450,225)`, `(+-450,0,0)`, `(0,+-450,0)`, `(0,0,+-450)` (the
   last one = feet, extended by velocity/4), rotated by the heading,
   each as a segment cast from the body centre, against polygons with
   flag `0x100` (walls);
3. on a hit: grounded → slide; airborne → land, and the polygon's
   `flags & 0xe00` gives the impact tolerance (`0, 0x1000, 0x6400, 0xfa00,
   0x15e00, 0x1c200` — soft to hard surfaces, inferred);
4. **touch**: hull point 0 hit → `input L = 256`; point 2 hit →
   `input R = 256`;
5. the touched polygon's flags become the creature's "surface flags"
   (`creature+0x138`), plus any flags injected by the game
   (`creature+0xae8`, e.g. `0x21000` by `0x419940`).

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

**Afflictions** (inferred meaning, observed effect): 12 polygon-triggered
states held in `creature+0x8fc` by an effect manager that also alters his
look (`Afflictions_Select` @ `0x406350`). Seen in Mendel's code: 5 = blind
(both IR inputs 0), 6 = random sensor noise, 3 = fast gait, 7/8/9 = gait
40 + agitated sounds + faster actuators, 10/11/12 = gravity changes, 1 = IR
also sees polygons flagged `0x400000`.

## 3. Senses — observed (logic), inferred (exact ray geometry)

`Creature_SenseInfrared` @ `0x418b00`, every tick after the mover:

* Two eyes, **left** (input `creature+4`) and **right** (`creature+8`).
  An eye only fires if its input is still 0 (i.e. no touch this tick) —
  touch wins over sight.
* Each eye casts **three rays** in the same direction, from three origins
  stacked along the body's up axis (high, middle, low). Direction: heading
  ± `a`, with `a = 100 - posture*50/256` (in 1/1024 turns: about 35 to 18
  degrees); length `R = 1500 + posture*500/256`. The posture output thus
  also controls how far and how wide he looks.
* High and middle rays: `Creature_CastIrRay` @ `0x4190f0`, first hit with
  mask `0x40000100` on a polygon whose flags have `0xc0` (IR-visible) or
  `0x40000000`. The low ray: `Creature_CastIrLedgeRay` @ `0x4191f0` walks
  *all* hits and reports a drop (two hits more than 250 apart): it sees
  ledges.
* Value of a hit: `clamp(256 - (dist - 656) * 256 / (R - 450), 0, 256)`:
  256 = touching, 0 = nothing in range. The eye reports the nearest of
  its three rays.
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
`Creature_ReadWorldSection` @ `0x4177d0` reads nothing from the section (it
re-registers Mendel's colour ramps and seeks to the global creature sound
set).

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
| `+0x978` | 5 | landing after more than 5 airborne ticks (event 4) | 5 ticks (own timer) |
| `+0x900` | 1, 2 | agitated (event 1: afflictions 7/8/9) | 5..36 ticks |
| `+0x93c` | 3, 4 | eye ray hit a `0x40000000` polygon (event 2) | 20..51 ticks |
| `+0x9b4` | 6, 7 | eye ray hit a `0x180000`-class polygon (event 8) | 20..51 ticks |
| `+0x9f0` | 8, 9, 10, 11 | strong left/right asymmetry (event 16) | 20..51 ticks |

A random member of the cue is chosen by the sound system.

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

`frac` (stored at `creature+0xae0`) interpolates rendering between two
simulation states (`CreatureMover_Step` blends previous/current transforms
with it). **The simulation rate itself was not found**: the only fixed
delay seen is a 17 ms busy-wait in a palette fade. Open question.

## 8. What the runtime port verified (crates/engine/src/creature.rs)

Running Mendel in the live section-0 simulation confirmed or corrected:

* **Z is up** (corrected; the first port assumed Y-down and Mendel
  walked on the sides of pads). World up is +z; in his own frame the
  hull points are (forward, left, up).
* **Model origin**: the posed feet end exactly at `y = 41 + stance*166/256`
  (41 / 124 / 221 measured at stances 0 / 128 / 255): the origin floats
  `height` above the ground. Model axes after the load transform are
  forward `+x`, down `+y` (towards the feet), left `+z` (hull point 0,
  left `= +450`, drives the *left* input). In a Z-up world the model's
  `−y` maps to his up vector. Verified.
* **Floors are flag bit 1** (`0x2`), walls bit 8 (`0x100`): the mover's
  floor finder (`0x411ec3`) casts with mask 2 (`0x4089e0`, nearest of up
  to ten hits); in section 0 every pad top facing +z is `0x2` and every
  wall `0x340`. Save pads (`0x22`) are floors too. A flip-book frame may
  drop the bit (pad #83 shows it on one frame of three), so we treat a
  polygon as a floor if any frame of its current appearance state is one.
* **Portal offsets are polygon-local**: a portal's `(u, v, h)` is measured
  from the first vertex of its polygon `ints[0]`, along that polygon's
  frame (`Math_FrameFromNormal`), `h` along its normal. On every pad on
  the CD this lands on the polygon's centre (Strt #57: `(383, 922) +
  (-387, -922)`; arrival pad #83: exact). Mendel appears standing on that
  polygon, in its frame.
* **One-sided polygons** (inferred, consistent with every floor checked):
  `(v1-v0) x (v2-v0)` points to the solid side's front.
* **The intro is a ride**: Mendel is born standing on the `Strt` disc #57,
  the floor of the birth tube #54. The disc is a stack of four polygons
  whose appearance states hand the floor bit down the stack, then to the
  docked capsule #3 (its floors 25, 27, 29 in turn as it rolls); its
  1,992-frame motion path carries him to the main area and drops him on
  the arrival pad #83. A surface-attached mover is required (he keeps his
  position *and* orientation in the platform's frame). Verified at
  runtime: he lands on #83 after ~134 s.
* **Touch wakes folders**: dropped on platform #77 (folder #75), his event
  1 switches the sleeping upper-level folder #145 back on within 20 ticks.
  Verified (test `mendel_wakes_the_world_in_section0`).
* **Pads repeat**: portal #83 in state 3 requests a teleport *every tick*
  until it feels Mendel; the port ignores requests whose destination is
  within 1,500 units (inferred), otherwise he could never land.

## Open questions

* Game tick rate (and who calls `Creature_Update` with which `frac`).
* Polygon attributes `unk0/unk1` (`+0x5c/+0x5e`); exact trig-table scale
  (the tables at `0x472858`/`0x473058` are built at run time).
* Why only 4 of 15 cached probability tables match a 2-input search
  (stale caches written mid-evolution, or non-zero third input at
  authoring time?).
* Camera profile fields `a, b, c, d`; the `u32 1520`.
* Exact names of the 12 afflictions; the full list of what each one does
  outside the creature code.
* Overlay 0 is unreachable from the age slider: used elsewhere?
