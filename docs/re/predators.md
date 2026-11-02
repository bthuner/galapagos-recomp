# Predators and hazards

> Source: `crates/engine/src/predator.rs` (runtime), `crates/formats/examples/hazard_probe.rs`
> (lists every predator and hazard polygon of the CD). Reverse engineered from
> `galapagos.exe` (Ghidra via REA plus the objdump listing: most predator
> methods are vtable-only). No game data lives in this repository.

Confidence labels as elsewhere: **observed** (code and data agree),
**inferred**, **unknown**.

## The short version

There is no "predator AI" in Galapagos, and no second creature. A predator
is an ordinary world object (its own mesh, appearance states and state
machine, drawn by the world renderer) with a tiny class behaviour bolted on:

* `Pred` can **vanish** and **appear**;
* `PBal`, `PFrk`, `PMan` add a **lunge**: a straight-line mover with a
  trapezoidal speed profile (the camera's) aims *past* Mendel, a tracker
  smooths it, six probes touch what is around;
* `PMan` is `PFrk`'s twin with `PBal`'s manners (it spares a camouflaged
  Mendel), a long body of probes (a nose, two arms, two fins, a tail) and
  **harmless** flags: its bite only scares him (§3.1);
* touching Mendel means touching his invisible click hull `MEnc`, which
  hands him the predator's **surface flags**: a lethal surface. He dies
  exactly as on a lethal floor.
* `PPol`, the "launcher", circles its home on Mendel's side; `PBal`
  appears where it is.

On the CD: one `PBal` (section 2, #54), one `PPol` (#55), one `PFrk`
(#130) and four `Pred` (section 7, #115..#118). No `PMan` in any section
(the test `no_pman_on_the_cd` scans every object's FourCC); its class is
ported from the exe alone and tested on the fork re-classed.

## 1. Classes — observed

| Class | ctor | vtable | trigger (0x18) | update (0x28) | notes |
|---|---|---|---|---|---|
| `Pred` | `0x42a1a0` | `0x460ea8` | `0x42a2d0` | `0x42a350` | modes, effect, park |
| hunter base | `0x415e20` | `0x45f990` | (Pred's) | (Pred's) | mover `+0x2b8`, probes; init `0x415f50` |
| `PBal` | `0x415020` | `0x45f6c0` | (Pred's) | (Pred's) | init `0x4150d0`, chase `0x415220`, setup `0x4153a0` |
| `PFrk` | `0x415480` | `0x45f7b0` | (Pred's) | (Pred's) | init `0x415590`, chase `0x4156a0`, setup `0x416300` |
| `PMan` | `0x415930` | `0x45f8a0` | (Pred's) | (Pred's) | init `0x415a40`, chase `0x415c40`, setup `0x416300`, probe step `0x415c20` (slot 0xb0) |
| `PPol` | `0x4292b0` | `0x460de8` | `0x429430` | `0x429600` | not a `Pred`; stored in `Game+0x17efc0` by the factory (0x41c29e) |

Hunter slots past `Pred`'s: `0x9c` setup, `0xa0` chase (mode 3), `0xa4`
mode 4 (`PBal`/`PMan` `0x415df0` jumps to `0xa0`; `PFrk` `0x415830`
only turns to face Mendel; nothing sets mode 4), `0xb0` the probe step
that `0x416360` calls (`0x4160d0`; `PMan`'s `0x415c20` first rebuilds
its matrix, `0x4096a0`). `PFrk` and `PMan` share save/load `0x4158d0` /
`0x4158f0`.

`Pred` fields: `+0x208` mode, `+0x20c` contact flags, `+0x210` its own
transition effect (class `0x424520`, the same as Mendel's
dematerialisation), `+0x218` effect started, `+0x21a..+0x21e` = trigger
`shorts[1..3]`, `+0x220` park flag (1), `+0x224` park position
(100000, 100000, 100000), `+0x23a/+0x23c` sound silences, two sound cues
at `+0x240` and `+0x27c` (`0x42a5a0`). Save/load: `0x42a950` / `0x42a860`
(mode, flags, effect fields), hunters add `0x416640` / `0x4165e0`.

## 2. Modes and triggers — observed

Trigger `0x42a2d0`: `a = shorts[0]`; if `a` is 1 or 2 and the mode is not
1 or 2 already, `mode = a`, and `shorts[1]` (play the effect), `shorts[2]`
(effect kind), `shorts[3]` (variant) are stored.

Update `0x42a350` dispatches on the mode through the vtable:

| Mode | Slot | `Pred` | Meaning |
|---|---|---|---|
| 0 | — | — | idle |
| 1 | `0x94` → `0x42a3e0` | effect `0x424870(kind, variant, 0x88)` + `0x424940`, then mode 0 and **parked** at `+0x224` | vanish |
| 2 | `0x98` → `0x42a4b0` | position = home (`+0xd0`), slot `0x9c` (class setup), effect reversed (`0x4248c0`: variant + 0x40), then mode 3 | appear |
| 3 | `0xa0` | nothing for `Pred`; the chase for hunters | active |
| 4, 5 | `0xa4`, `0xa8` | never set by any code | — |

Without `shorts[1]` the change is instant, and appearing then skips
slot `0x9c` (`0x42a55b`): the hunter is put home but its mover is not
reset, so it resumes from where the mover was. No CD trigger appears
without the effect; the port runs the setup either way (inferred, a
harmless simplification: our mover starts uninitialised, the exe's from
the ctor). Visibility itself is the
appearance's business: the load (`0x42a740`) clears the object's draw flag
(`+0x34+0x28`, `0x4022e0(0)`), and every appearance change re-sets it
(`-5` frames clear it: `0x4237e3`), so the state machine decides.

**The effect** (observed; the class is mendel.md §2.3.1): the `Pred`
ctor builds its own effect mesh (`0x4105d0(obj+0x34, obj+0x74)`: the
short a burst spins is the object's `+0x74`, our `rot[2]`) and manager
(`+0x210`, height 0x88, so its floor is 126 in object y). Mode 1 starts
it on the first tick (`+0x218`), mode 2 starts it backwards; both step it
that same tick, then every tick until it returns 0 (vanish: the object
gets its mesh back and is parked; appear: mode 3). The sounds play from
the predator (cue `+0x98` = owner `+0x30`, its position). On the CD:

| Object | Vanish | Appear (backwards) | Ticks |
|---|---|---|---|
| s2 `PBal` #54 | 1 v1: all polygons fade at once | 1 v0x41: all fade in from black | 7 / 7 |
| s2 `PFrk` #130 | 2 v0: burst, flash 6 | 2 v0x40: shards fly in, spinning `rot[2]` | 100 / 60 |
| s7 `Pred` #115..#117 | 2 v2: burst, vertical spread / 8 | — | 100 |
| s7 `Pred` #118 | 2 v0 | — | 100 |

Port: predator.rs `run_effect` drives transition.rs; `Skin` writes the
effect mesh into the object's own vertices (corners moved, hidden
polygons on layer −2, replaced brightness pre-lit with the shader's
level formula and the object's turned normal) and restores them when it
ends (and before a snapshot is restored).

## 3. The chase — observed (constants), inferred (bounce)

Per tick in mode 3 (`0x416360`, called by each class's mode-3 handler):

```text
target = Mendel's position (creature+0xa34)        PBal: home instead if his affliction is 4
mover.step(); out = tracker(mover.pos)             0x414c90 (mover 0x4278c0 + alpha-beta 0x406b90)
if mover stopped:
    d = target - position;  aim = target + overshoot * d / approx_len(d)
    mover.set_target(aim)                          0x414c10 → 0x427840
probes (0x4160d0): for (±r,0,0), (0,±r,0), (0,0,±r) rotated by the object,
    segment centre → centre+offset, World_Cast mask 0x103 (MEnc bit 0, floors, walls)
    hit: contact = 1; PostEvent(hit object, flags +0x20c);
         push = 0x408c50 response + hit normal(×1024) >> shift; move body, mover, tracker
position = out;  rotation: PBal spins (+0x70, +0x72, +0x74) += (−20, +20, +20)/tick;
    PFrk/PMan face +0x32c = (target − pos) *as of the last re-aim*, smoothed by
    their own tracker (+0x354, d 0xf333, built by the ctor and never reset),
    0x430c50 → yaw +0x72, pitch +0x70; then +0x74 rolls +10/tick
sounds: growl (cue 1) if approx_len(Mendel − new pos) < 1000, then silent N ticks;
        bite (cue 2) on contact, then silent 30 ticks
```

Before the probes, `0x4084e0` gathers candidates: every object whose tag
(`0x4232c0`) is `'Coll'` and whose box meets `pos ± (+0x346 + 400)`
(`0x41648f`), the predator itself excluded. Every probe tip lies inside
that box, so the port's own gather (sized by each probe) finds the same
polygons. Each probe starts from the *current* centre: a push moves it
for the probes after it.

| | accel | max | overshoot | tracker d | bounce shift | probes | box `+0x346` | flags | park | growl every |
|---|---|---|---|---|---|---|---|---|---|---|
| base (`0x415f50`) | 10 | 100 | 5000 | 0xcccc | 5 | ±300 on each axis | 300 | 0 | 100000 | — |
| `PBal` (`0x4150d0`) | 30 | 300 | 500 | 0xe666 | 7 | ±700 on each axis | 1000 | `0x405b2100` | 100000 | 200 |
| `PFrk` (`0x415590`) | 15 | 150 | 4000 | 0xc000 | 5 | (base) | (base) | `0x40582100` | 1500000 | 15 |
| `PMan` (`0x415a40`) | 15 | 150 | 4000 | 0xd999 | 5 | the body below | 1600 | `0x40000100` | 150000 | 15 |

**Angles.** The object matrix is `Math_RotationMatrix(yaw = +0x72,
pitch = +0x70, roll = +0x74)`: `0x4096a0` builds it with
`0x430ce0(+0xf2, +0xf0, +0xf4)` from the copy of the angles, and
`0x409880` applies it transposed (`0x431ad0`), so local +y is the
direction `Math_VectorAngles` (`0x430c50`, yaw → `+0x72`, pitch →
`+0x70`) turned it to. Our `sim::transform` reads `rot` as (yaw, pitch,
roll), so predator.rs keeps `rot` = (`+0x72`, `+0x70`, `+0x74`) for every
predator and `PPol`. Observed.

### 3.1 `PMan` — observed

The chase `0x415c40` is `PFrk`'s `0x4156a0` instruction for instruction
(the same `0x416360` step, the same heading tracker and +10 roll, the
same growl/bite timers 15 and 30) except for the target, which is
`PBal`'s (`0x415c47..0x415ca2`): Mendel (`0x4183a0` on the creature at
`+0x358`) while the mode is 3 and his affliction (`0x4198e0`) is not 4,
else home (`+0xd0`). Its constructor `0x415a40` is `PFrk`'s with:

* tracker d **0xd999** (`PFrk` 0xc000): a stiffer follow of the mover;
* park **150,000** (`PFrk` 1,500,000);
* flags **`0x40000100`**: bit 30 (a hazard his eyes see, the "saw a
  hazard" cue) and bit 8, the creature's contact class; no lethal kind,
  no respawn reason, no pain. Its bite reaches him (`MEnc` → `0x41cb80`)
  and does nothing deadly: **`PMan` only scares**;
* its own probe table (`+0x318`, local, +y = the nose):

```text
(0, 1554, 0)      nose            (0, 925, ±296)   fins, up and down
(±999, 1295, 0)   two arms        (0, −1406, 0)    tail
```

  and a box `+0x346` = 1600 that holds them all;
* its probe step (slot 0xb0, `0x415c20`) rebuilds the matrix from
  `+0xf0` before probing: last tick's copy of the angles, taken before
  that tick's turn, so the asymmetric body probes one tick behind the
  pose it is drawn with.

The `0x40000100` mask is exactly the creature's "what do my eyes see"
pick mask (objects.md §3). inferred: a fright, a creature that dives at
Mendel to teach his brain to flee, cut before release.

Tested on section 2's fork re-classed (predator.rs tests): the first
re-aim is `target + 4000·d / approx_len(d)` and `+0x32c` = `d` holds
until the next one; a camouflaged Mendel sends `PMan` home but not
`PFrk`; it growls at tick 0 and 16 with Mendel 900 away (none at 1,100);
in the running game its nose strikes `MEnc` (its flags land there), it
bites, faces its lunge, and Mendel lives, where `PFrk` in the same scene
kills him; the fork's own effects take 61 ticks to appear and 101 to
vanish, then it waits at (150,000)³; a saved game carries it.

Appearing (slot `0x9c`): `PBal` (`0x4153a0`) starts at the `PPol`'s
current position (`0x41cbd0` reads `Game+0x17efc0`); `PFrk`/`PMan`
(`0x416300`) where they are. Both reset the mover and prime the tracker.

**The bounce — observed.** `0x408c50` rotates the hit polygon's normal
into the world (`0x409880`) and casts a 0x400-long segment from the probe's
tip along it, against that polygon alone (`0x408670`, `0x407740`). If the
segment crosses (the tip lies behind the polygon, at most 1,024 deep) it
returns `crossing − tip`, the perpendicular way back onto the plane, and
the caller adds the polygon's *stored* normal (`puVar3[0xc]`: object-local,
×1024, not rotated) `>> shift`. If it doesn't cross there is no push. The
touch itself (contact flag `+0x31c`, `PostEvent`) happens on any hit of
`World_Cast` (`0x4089e0`), which accepts polygons facing either way. The
camera's wall probes (`0x413cb0`) use the same response with shift 7
(8 units).

## 4. How a predator hurts Mendel — observed

1. A probe hits `MEnc`, the click hull glued to Mendel (its polygons carry
   bit 0, which the probe mask 0x103 includes).
2. `WorldObject_PostEvent` (0x4232f0) ORs the predator's flags into
   `MEnc`'s event bits.
3. `MEnc`'s update (0x426150) passes non-zero event bits to
   `Game_CreatureReact` @ 0x41cb80 → `creature+0xae8 |= flags` (0x4198b0).
4. The creature ORs `+0xae8` into his surface flags with his next touch,
   and `Creature_HandleSurfaceEvents` @ 0x419550 does the rest:

```text
0x405b2100 (PBal) = 0x40000000  hazard: his eyes see it, cue "saw a hazard"
                  + 0x00002000  lethal: transition effect kind 2
                  + 0x00030000  variant 3                (PFrk: variant 0)
                  + 0x00180000  then respawn reason 3 (death, back to the save point)
                  + 0x00400000  harmless under affliction 1 (the shield)
                  + 0x00000100  wall
                  no 0x1000000: no pain for the brain
```

So a predator kill restores the last snapshot like any death; the brain
is not punished. `PBal` leaves a camouflaged Mendel (affliction 4) alone.
`Pred` (section 7) hurts only through its polygon flags (`0x15c2380`,
`0x1584380`: ordinary lethal walls with pain).

## 5. `PPol` — observed

Trigger `0x429430` reads `ints[0..8]` into `+0x20c..+0x224`: mode,
radius, angle, bob amplitude, bob phase, wobble amplitude, wobble phase,
lift; it then jumps to `home + (cos a·R, sin a·R, sin a·2000 + lift) >> 14`
and resets its tracker (d 0xf333). Update `0x429600`, if mode ≠ 0:

```text
bearing = Math_Angle2(Mendel - home)                  (0x430ab0)
diff    = wrap((bearing + 0x400 & 0x3ff) - (angle & 0x3ff))
mode 1: angle += sign(diff) * 2;  |diff| <= 2 → mode 2      mode 2: |diff| > 60 → mode 1
z = home.z + sin(bob)·amp >> 14 + lift;  bob += 20
a = angle + sin(wobble)·wobble_amp >> 14; wobble += 4
pos = tracker(home + (cos a, sin a)·R >> 14, z);  faces Mendel (0x430c50)
```

Section 2: `ints = [1, 22500, 520, 1000, 0, 30, 0, -1000]` (and a second
orbit with R 30000, lift −4000); entry 3 (all zeros) stops it.

## 6. The state machines on the CD — observed

* `PBal` #54: state 0 hidden (appearance 2 has a `-5` frame) → when `PPol`
  #55 is in state 2 → state 3: trigger 1 = `[2, 1, 1, 1]` (appear, effect
  kind 1 variant 1) → 2 ticks → state 1 (visible, flags `0x40000000`):
  hunts for **100 ticks**, or until #96 reaches state 1, or until the
  player **clicks it** → state 2: trigger 2 = `[1, 1, 1, 1]` (vanish) →
  8 ticks → state 0.
* `PFrk` #130: hidden until #133 is in state 1, 35 ticks, then appears
  (`[2, 1, 2, 0]`) and hunts until #6 is in state 1 (vanish `[1, 1, 2, 0]`).
* `Pred` #115..#118 (section 7): visible from the start; when the
  section's #111 reaches state 3/4/5 they vanish (`[1, 1, 2, 2]`, effect
  kind 2 = the explosion) with their own world sounds.

## 7. Sounds — observed

The predator sound set (header `+0xf8`, 2 sounds, loaded once by
`0x42a5a0`): cue 1 = sound 1, pitch 1.0, distances 5000/500/10000;
cue 2 = sound 2, pitch 0.5, 5000/500/5000. Host event 9
(`EV_PREDATOR_SOUND`: sound, object id, pitch).

## Open questions

* The rotation order outside the predators: `0x402370` stores a pose's
  three angles at `+0x70, +0x72, +0x74`, and the motion paths put them
  into our `rot[0..3]` in that order, while `sim::transform` reads
  `rot[0]` as the yaw, which the exe takes from `+0x72` (§3, Angles).
  Either the paths' angles are swapped in our objects or their data
  never exercises it; the predators are now consistent with the exe.
* Appearing without an effect (no slot `0x9c`, §2) is not ported.
