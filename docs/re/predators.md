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
* touching Mendel means touching his invisible click hull `MEnc`, which
  hands him the predator's **surface flags**: a lethal surface. He dies
  exactly as on a lethal floor.
* `PPol`, the "launcher", circles its home on Mendel's side; `PBal`
  appears where it is.

On the CD: one `PBal` (section 2, #54), one `PPol` (#55), one `PFrk`
(#130) and four `Pred` (section 7, #115..#118). No `PMan`.

## 1. Classes — observed

| Class | ctor | vtable | trigger (0x18) | update (0x28) | notes |
|---|---|---|---|---|---|
| `Pred` | `0x42a1a0` | `0x460ea8` | `0x42a2d0` | `0x42a350` | modes, effect, park |
| hunter base | `0x415e20` | `0x45f990` | (Pred's) | (Pred's) | mover `+0x2b8`, probes; init `0x415f50` |
| `PBal` | `0x415020` | `0x45f6c0` | (Pred's) | (Pred's) | init `0x4150d0`, chase `0x415220`, setup `0x4153a0` |
| `PFrk` | `0x415480` | `0x45f7b0` | (Pred's) | (Pred's) | init `0x415590`, chase `0x4156a0`, setup `0x416300` |
| `PMan` | `0x415930` | `0x45f8a0` | (Pred's) | `0x415c20` | chase `0x415c40` (not read) |
| `PPol` | `0x4292b0` | `0x460de8` | `0x429430` | `0x429600` | not a `Pred`; stored in `Game+0x17efc0` by the factory (0x41c29e) |

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

Without `shorts[1]` the change is instant. Visibility itself is the
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
position = out;  rotation: PBal spins (−20, +20, +20)/tick; PFrk faces (target − pos)
    smoothed by its own tracker (d 0xf333, 0x430c50) and rolls +10/tick
sounds: growl (cue 1) if approx_len(Mendel − pos) < 1000, then silent N ticks;
        bite (cue 2) on contact, then silent 30 ticks
```

| | accel | max | overshoot | tracker d | bounce shift | probe | flags | park | growl every |
|---|---|---|---|---|---|---|---|---|---|
| base (`0x415f50`) | 10 | 100 | 5000 | 0xcccc | 5 | 300 | 0 | 100000 | — |
| `PBal` (`0x4150d0`) | 30 | 300 | 500 | 0xe666 | 7 | 700 | `0x405b2100` | 100000 | 200 |
| `PFrk` (`0x415590`) | 15 | 150 | 4000 | 0xc000 | 5 | 300 | `0x40582100` | 1500000 | 15 |

Appearing (slot `0x9c`): `PBal` (`0x4153a0`) starts at the `PPol`'s
current position (`0x41cbd0` reads `Game+0x17efc0`); `PFrk`/`PMan`
(`0x416300`) where they are. Both reset the mover and prime the tracker.

**inferred**: `0x408c50`'s response vector (a second cast along the hit
normal) is approximated by pulling the probe tip back to the surface.

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

* `0x408c50`'s exact response; `0x4084e0` (called before the probes).
* The object rotation order (`0x402370` only stores the three angles);
  we write `(+0x70, +0x72, +0x74)` into our `rot[0..3]` as the motion
  paths do.
* `PMan`'s chase (`0x415c40`), absent from the CD.
