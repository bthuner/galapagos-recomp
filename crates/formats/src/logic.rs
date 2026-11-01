//! Object logic: the per-object state machines that make the world react
//! to clicks, timers and Mendel. See `docs/re/objects.md` for the full,
//! pedagogic spec (with exe addresses and a confidence label per claim).
//!
//! # Five blocks, one object
//!
//! Every world object (`WorldObject_Read` @ 0x423b50) is a C++ object whose
//! *primary* base class is `CStateMachine` (vtable 0x461018). Around the
//! mesh and the appearance (decoded in [`crate::worlds`]) sit five more
//! sub-blocks, each introduced by a `u16` "present" flag and each ending at
//! one of the object's `block_end` offsets:
//!
//! ```text
//! block_end[1] -> u16 + StateMachine       StateMachine_Read  @ 0x42d580   -> block_end[2]
//! block_end[2] -> u16 + ShapeAnim          vtable 0x460f60 +8 @ 0x42aea0   -> block_end[3]
//! block_end[3] -> u16 + appearance         PolyAppearance_Read @ 0x42e180  -> block_end[4]
//! block_end[4] -> u16 + SoundEmitters      vtable +0xc         @ 0x42b9e0  -> block_end[5]
//! block_end[5] -> u16 + MotionPaths        MotionPaths_Read   @ 0x428cf0   -> block_end[6]
//! block_end[6] -> u16 + ObjectParams       vtable 0x45f310 +8 @ 0x40a0d0   -> block_end[0]
//! ```
//!
//! No class appends anything after the params: the "subclass tails" turned
//! out to be empty for all 2,089 objects. What makes a `'Tele'` a teleporter
//! is not extra bytes but *code*: each class overrides one virtual (the
//! "trigger" action, vtable slot 0x18) that interprets [`ObjectParams`].
//!
//! # How it runs (summary)
//!
//! The state machine is a list of [`State`]s. Each state has *entry
//! actions* and a list of [`Transition`]s. Once per game tick
//! (`StateMachine_Evaluate` @ 0x42d110) every transition of the current
//! state is tested; a transition fires when any of its condition *groups*
//! is entirely true (conditions AND-ed inside a group, groups OR-ed). A
//! firing transition queues its own [`Actions`]; after *all* objects have
//! been evaluated (`ObjectRegistry_TickAll` @ 0x4284f0) the queued actions
//! are applied (`StateMachine_ApplyPending` @ 0x42ce70). The six action
//! slots select, by 1-based index, an entry in the other blocks: a shape
//! animation, an appearance state, a sound emitter, a motion path, a
//! params entry for the class trigger — or jump to another state.

use crate::bytes::{Cursor, OutOfBounds};
use crate::worlds::Error;

fn i16(c: &mut Cursor) -> Result<i16, OutOfBounds> {
    Ok(c.u16()? as i16)
}

/// The six action slots shared by states (entry actions) and transitions.
///
/// On disk: `i16 value[6]` then `u8 flag[8]`; `flag[2 + i]` enables
/// `value[i]`, `flag[1]` resets the state timer, `flag[0]` is always 1 and
/// never read by the interpreter. Indices 0..=4 are **1-based** (the
/// handlers do `value - 1`); the state index is 0-based.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Actions {
    /// Class-specific trigger with [`ObjectParams`] entry `n - 1`
    /// (vtable slot 0x18: teleport, save pad, folder on/off, music ...).
    pub trigger: Option<i16>,
    /// Select [`ShapeAnim`] state `n - 1` (vertex morph; slot 0x08 @ 0x423320).
    pub shape: Option<i16>,
    /// Select appearance state `n - 1` (textures; slot 0x0c @ 0x423350).
    pub appearance: Option<i16>,
    /// Play [`SoundEmitter`] `n - 1` (slot 0x10 @ 0x4233a0).
    pub sound: Option<i16>,
    /// Start [`MotionPath`] `n - 1` (slot 0x14 @ 0x4233c0).
    pub motion: Option<i16>,
    /// Go to state `n` (0-based; `CStateMachine_SetState` slot 0x04 @ 0x42cfd0).
    pub goto_state: Option<i16>,
    /// Reset the ticks-in-state counter to 0.
    pub reset_timer: bool,
}

impl Actions {
    fn read(c: &mut Cursor) -> Result<Self, OutOfBounds> {
        let mut v = [0i16; 6];
        for x in v.iter_mut() {
            *x = i16(c)?;
        }
        let mut f = [0u8; 8];
        for x in f.iter_mut() {
            *x = c.u8()?;
        }
        let pick = |i: usize| (f[2 + i] != 0).then_some(v[i]);
        Ok(Self {
            trigger: pick(0),
            shape: pick(1),
            appearance: pick(2),
            sound: pick(3),
            motion: pick(4),
            goto_state: pick(5),
            reset_timer: f[1] != 0,
        })
    }

    /// Merges `other` over `self`, slot by slot (what the engine does when a
    /// transition's actions meet the entry actions of the target state).
    pub fn overlay(&mut self, other: &Actions) {
        macro_rules! m {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f; } )* };
        }
        m!(trigger, shape, appearance, sound, motion, goto_state);
        self.reset_timer |= other.reset_timer;
    }
}

/// One test of a transition (`StateMachine_Evaluate` @ 0x42d110).
///
/// On disk: `u16 opcode`, then either `u32 value` or, for opcodes 0x16 and
/// 0x17, `u16 object_id, u16 state` (8 bytes in memory either way).
/// "Ticks" counts calls of the evaluator since the state was entered (or
/// since a `reset_timer`). Angles are in 1024ths of a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// 0x01: this object was clicked during this tick.
    Clicked,
    /// 0x02: this object was *not* clicked during this tick.
    NotClicked,
    /// 0x03: ticks in state >= n.
    TicksAtLeast(u32),
    /// 0x04: ticks in state <= n.
    TicksAtMost(u32),
    /// 0x05: all bits of the mask were posted this tick (only mask 1 is
    /// used on the CD: "Mendel touches me / one of my children", posted by
    /// the creature code through vtable slot 0x24, bubbling up the folders).
    Event(u32),
    /// 0x06: not all bits of the mask were posted this tick.
    NoEvent(u32),
    /// 0x07: rising edge: posted this tick and not the previous one.
    EventStarted(u32),
    /// 0x08: falling edge: posted the previous tick, not this one.
    EventEnded(u32),
    /// 0x09: `rand() % 100 <= n` (a percent chance per tick).
    Chance(u32),
    /// 0x0a..0x0f: position component (0 = x, 1 = y, 2 = z) `>=` / `<=` n.
    PositionAtLeast(u8, i32),
    PositionAtMost(u8, i32),
    /// 0x10..0x15: rotation `& 0x3ff` `>=` / `<=` n. Axis order follows the
    /// vtable getters: opcodes 0x10/0x11 read rotation[1], 0x12/0x13
    /// rotation[0], 0x14/0x15 rotation[2].
    RotationAtLeast(u8, i32),
    RotationAtMost(u8, i32),
    /// 0x16: object `id` (1-based object-list index) is in state `state`.
    ObjectInState { id: u16, state: u16 },
    /// 0x17: object `id` is not in state `state`.
    ObjectNotInState { id: u16, state: u16 },
    /// Anything else (the engine asserts "Unknown OpCode in CStateMachine").
    Unknown(u16, u32),
}

impl Condition {
    fn read(c: &mut Cursor) -> Result<Self, OutOfBounds> {
        let op = c.u16()?;
        if op == 0x16 || op == 0x17 {
            let (id, state) = (c.u16()?, c.u16()?);
            return Ok(if op == 0x16 {
                Condition::ObjectInState { id, state }
            } else {
                Condition::ObjectNotInState { id, state }
            });
        }
        let v = c.u32()?;
        let axis_pos = |o: u16| ((o - 0x0a) / 2) as u8;
        // rotation getters: 0x38 -> +0xf2, 0x3c -> +0xf0, 0x40 -> +0xf4
        let axis_rot = |o: u16| [1u8, 0, 2][((o - 0x10) / 2) as usize];
        Ok(match op {
            0x01 => Condition::Clicked,
            0x02 => Condition::NotClicked,
            0x03 => Condition::TicksAtLeast(v),
            0x04 => Condition::TicksAtMost(v),
            0x05 => Condition::Event(v),
            0x06 => Condition::NoEvent(v),
            0x07 => Condition::EventStarted(v),
            0x08 => Condition::EventEnded(v),
            0x09 => Condition::Chance(v),
            0x0a | 0x0c | 0x0e => Condition::PositionAtLeast(axis_pos(op), v as i32),
            0x0b | 0x0d | 0x0f => Condition::PositionAtMost(axis_pos(op), v as i32),
            0x10 | 0x12 | 0x14 => Condition::RotationAtLeast(axis_rot(op), v as i32),
            0x11 | 0x13 | 0x15 => Condition::RotationAtMost(axis_rot(op), v as i32),
            _ => Condition::Unknown(op, v),
        })
    }

    /// Evaluates the condition against a snapshot of the world, exactly like
    /// the 1997 switch statement (including its `<=` / `>=` choices).
    pub fn eval(&self, w: &impl LogicView) -> bool {
        use Condition::*;
        let rot = |a: u8| (w.rotation()[a as usize] & 0x3ff) as i32;
        match *self {
            Clicked => w.clicked(),
            NotClicked => !w.clicked(),
            TicksAtLeast(n) => n <= w.ticks() as u32,
            TicksAtMost(n) => w.ticks() as u32 <= n,
            Event(m) => w.events() & m == m,
            NoEvent(m) => w.events() & m != m,
            EventStarted(m) => w.events() & m == m && w.previous_events() & m != m,
            EventEnded(m) => w.events() & m != m && w.previous_events() & m == m,
            Chance(n) => w.random() % 100 <= n,
            PositionAtLeast(a, n) => n <= w.position()[a as usize],
            PositionAtMost(a, n) => w.position()[a as usize] <= n,
            RotationAtLeast(a, n) => n <= rot(a),
            RotationAtMost(a, n) => rot(a) <= n,
            ObjectInState { id, state } => w.state_of(id) == state,
            ObjectNotInState { id, state } => w.state_of(id) != state,
            Unknown(..) => false,
        }
    }
}

/// What a condition may look at. The engine implements this for one object
/// at a time; tests can use a plain struct.
pub trait LogicView {
    /// The object was the one picked by the mouse this tick.
    fn clicked(&self) -> bool;
    /// Ticks since the current state was entered (u16 in the original).
    fn ticks(&self) -> u16;
    /// Event bits posted to the object during this tick.
    fn events(&self) -> u32;
    /// Event bits of the previous tick.
    fn previous_events(&self) -> u32;
    /// A fresh random number (`FUN_0043c980`, the CRT `rand`).
    fn random(&self) -> u32;
    /// Current animated position (world units).
    fn position(&self) -> [i32; 3];
    /// Current animated rotation (1024ths of a turn, not wrapped).
    fn rotation(&self) -> [i16; 3];
    /// Current state of the object with 1-based list index `id`.
    fn state_of(&self, id: u16) -> u16;
}

/// One transition: fires when any group is entirely true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub actions: Actions,
    /// Disjunctive normal form: `groups.any(|g| g.all(true))`. A transition
    /// with no group never fires.
    pub groups: Vec<Vec<Condition>>,
}

impl Transition {
    pub fn fires(&self, w: &impl LogicView) -> bool {
        self.groups.iter().any(|g| g.iter().all(|c| c.eval(w)))
    }
}

/// One state (32 bytes in memory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// Applied when the state is entered through a pending state change.
    pub entry: Actions,
    /// `u32` passed to vtable slot 0x20 on entry and stored at `+0x28`;
    /// 0 or 1 on the CD, meaning unknown.
    pub flag: u32,
    pub transitions: Vec<Transition>,
}

/// `StateMachine_Read` @ 0x42d580.
///
/// ```text
/// u16 state_count
/// state_count x {
///   i16 value[6]; u8 flag[8]                 entry Actions
///   u32 flag
///   i16 transition_count
///   transition_count x {
///     i16 value[6]; u8 flag[8]               Actions
///     i16 group_count
///     group_count x { i16 n; n x Condition }
///   }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateMachine {
    pub states: Vec<State>,
}

impl StateMachine {
    pub fn read(c: &mut Cursor) -> Result<Self, OutOfBounds> {
        let n = c.u16()? as usize;
        let mut states = Vec::with_capacity(n);
        for _ in 0..n {
            let entry = Actions::read(c)?;
            let flag = c.u32()?;
            let nt = i16(c)?.max(0) as usize;
            let mut transitions = Vec::with_capacity(nt);
            for _ in 0..nt {
                let actions = Actions::read(c)?;
                let ng = i16(c)?.max(0) as usize;
                let mut groups = Vec::with_capacity(ng);
                for _ in 0..ng {
                    let nc = i16(c)?.max(0) as usize;
                    let mut g = Vec::with_capacity(nc);
                    for _ in 0..nc {
                        g.push(Condition::read(c)?);
                    }
                    groups.push(g);
                }
                transitions.push(Transition { actions, groups });
            }
            states.push(State { entry, flag, transitions });
        }
        Ok(Self { states })
    }

    /// One evaluation pass for the object in `state` (the first half of a
    /// tick). Returns the merged actions of every transition that fired, in
    /// order (later transitions overwrite earlier slots, as in the exe), or
    /// `None`.
    pub fn evaluate(&self, state: u16, w: &impl LogicView) -> Option<Actions> {
        let s = self.states.get(state as usize)?;
        let mut out: Option<Actions> = None;
        for t in &s.transitions {
            if t.fires(w) {
                out.get_or_insert_with(Actions::default).overlay(&t.actions);
            }
        }
        out
    }

    /// The second half of a tick (`StateMachine_ApplyPending` @ 0x42ce70):
    /// if a state change is pending, the target state's entry actions are
    /// merged over the transition's (a target with its own `goto_state`
    /// chains exactly one level). Returns what to execute.
    pub fn resolve(&self, mut pending: Actions) -> Actions {
        if let Some(target) = pending.goto_state {
            if let Some(s) = self.states.get(target as usize) {
                pending.overlay(&s.entry);
            }
        }
        pending
    }
}

/// One frame of a vertex morph: a full vertex set for the mesh plus six
/// floats per polygon (copied to the in-memory polygon at `+0x44`; most
/// likely the texture-mapping gradients — inferred).
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeFrame {
    pub vertices: Vec<[i16; 3]>,
    pub polygon_mapping: Vec<[f32; 6]>,
}

/// One morph state: `frames` play in order, the engine blends consecutive
/// frames with a 1/256 weight (`0x4235d0`).
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeState {
    /// `u16` stored at `+0xe` of the helper (1..45 on the CD; inferred:
    /// playback speed or frame duration).
    pub rate: u16,
    pub frames: Vec<ShapeFrame>,
}

/// The `+0xbc` helper (class 0x42acc0, reader 0x42aea0): vertex-morph
/// animation. Every frame has exactly the mesh's vertex and polygon counts
/// (verified on all 1,679 blocks); frame 0 usually equals the mesh.
///
/// ```text
/// u16 state_count
/// state_count x { u16 rate; u16 frame_count; u16 key_count;
///   key_count x { Frame; u16 m; m x Frame } }          frame_count = total
/// Frame = u32 nv; nv x i16[3]; u32 np; np x f32[6]
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeAnim {
    pub states: Vec<ShapeState>,
}

fn read_frame(c: &mut Cursor) -> Result<ShapeFrame, OutOfBounds> {
    let nv = c.u32()? as usize;
    let mut vertices = Vec::with_capacity(nv.min(1 << 16));
    for _ in 0..nv {
        vertices.push([i16(c)?, i16(c)?, i16(c)?]);
    }
    let np = c.u32()? as usize;
    let mut polygon_mapping = Vec::with_capacity(np.min(1 << 16));
    for _ in 0..np {
        let mut f = [0f32; 6];
        for x in f.iter_mut() {
            *x = f32::from_bits(c.u32()?);
        }
        polygon_mapping.push(f);
    }
    Ok(ShapeFrame { vertices, polygon_mapping })
}

impl ShapeAnim {
    pub fn read(c: &mut Cursor) -> Result<Self, OutOfBounds> {
        let n = c.u16()? as usize;
        let mut states = Vec::with_capacity(n);
        for _ in 0..n {
            let rate = c.u16()?;
            let total = c.u16()? as usize;
            let keys = c.u16()?;
            let mut frames = Vec::with_capacity(total.min(4096));
            for _ in 0..keys {
                frames.push(read_frame(c)?);
                let m = c.u16()?;
                for _ in 0..m {
                    frames.push(read_frame(c)?);
                }
            }
            states.push(ShapeState { rate, frames });
        }
        Ok(Self { states })
    }
}

/// A positional sound (34 bytes, reader 0x42b9e0, helper class 0x42b7c0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundEmitter {
    /// Up to three indices into the section sound set (`-1` = none).
    pub sounds: [i16; 3],
    /// 16.16 fixed; 65536 = 1.0 (pitch, inferred).
    pub pitch: u32,
    /// 16.16 fraction (0, 1310 = 0.02, 3276 = 0.05, 13107 = 0.2): random
    /// variation, inferred.
    pub variation: u32,
    /// Full volume inside this distance (inferred).
    pub near: i32,
    /// Silent beyond this distance (inferred; 300000 = everywhere).
    pub far: i32,
    /// 256 = full volume.
    pub volume: i16,
    /// 1 = loop (inferred).
    pub looping: i16,
    pub reserved: [u32; 2],
}

impl SoundEmitter {
    pub fn read_list(c: &mut Cursor) -> Result<Vec<Self>, OutOfBounds> {
        let n = c.u16()? as usize;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(Self {
                sounds: [i16(c)?, i16(c)?, i16(c)?],
                pitch: c.u32()?,
                variation: c.u32()?,
                near: c.i32()?,
                far: c.i32()?,
                volume: i16(c)?,
                looping: i16(c)?,
                reserved: [c.u32()?, c.u32()?],
            });
        }
        Ok(v)
    }
}

/// A motion key (28 bytes): an offset/pose and how many in-between frames
/// to generate towards the next key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionKey {
    pub position: [i32; 3],
    /// 1024ths of a turn (values beyond 1024 make several turns).
    pub rotation: [i32; 3],
    /// Interpolated frames inserted between this key and the next.
    pub steps: i32,
}

/// A motion path (`MotionPaths_Read` @ 0x428cf0). The loader expands the
/// keys into one pose per tick by linear interpolation
/// (`MotionKey_Lerp` @ 0x428a90: `a + (b - a) * i / (steps + 1)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionPath {
    /// 1 = wrap to frame 0 at the end; 0 = stop on the last frame.
    pub looping: u16,
    /// 1 = poses are relative to the object's rest pose, 0 = absolute
    /// (`0x423a30` calls slot 0x68 when 0). Inferred from the code path.
    pub relative: u16,
    /// Number of expanded frames: `sum(steps + 1) + 1` (verified, 1,516 paths).
    pub frame_count: u32,
    pub keys: Vec<MotionKey>,
}

impl MotionPath {
    pub fn read_list(c: &mut Cursor) -> Result<Vec<Self>, OutOfBounds> {
        let n = c.u16()? as usize;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let looping = c.u16()?;
            let relative = c.u16()?;
            let frame_count = c.u32()?;
            let k = c.u16()? as usize;
            let mut keys = Vec::with_capacity(k);
            for _ in 0..k {
                keys.push(MotionKey {
                    position: [c.i32()?, c.i32()?, c.i32()?],
                    rotation: [c.i32()?, c.i32()?, c.i32()?],
                    steps: c.i32()?,
                });
            }
            v.push(Self { looping, relative, frame_count, keys });
        }
        Ok(v)
    }

    /// Expands the keys to one `(position, rotation)` per tick, like the
    /// loader does (integer division truncating toward zero, as in C).
    pub fn frames(&self) -> Vec<([i32; 3], [i32; 3])> {
        let mut out = Vec::with_capacity(self.frame_count as usize);
        for (i, a) in self.keys.iter().enumerate() {
            out.push((a.position, a.rotation));
            let Some(b) = self.keys.get(i + 1) else { break };
            let d = a.steps + 1;
            for s in 1..=a.steps {
                let l = |x: i32, y: i32| x + (y - x) * s / d;
                out.push((
                    [l(a.position[0], b.position[0]), l(a.position[1], b.position[1]), l(a.position[2], b.position[2])],
                    [l(a.rotation[0], b.rotation[0]), l(a.rotation[1], b.rotation[1]), l(a.rotation[2], b.rotation[2])],
                ));
            }
        }
        out
    }
}

/// One entry of the class-trigger parameters (`0x40a0d0`, 124 bytes on
/// disk). Each array is preceded by its `u16` length, which the engine
/// asserts (10, 10, 10, 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectParams {
    /// `ObjectParams_GetInt` @ 0x40a040.
    pub ints: [i32; 10],
    /// `ObjectParams_GetShort` @ 0x40a080; `shorts[0]` is usually the kind.
    pub shorts: [i16; 10],
    pub ints2: [i32; 10],
    pub shorts2: [i16; 8],
}

impl ObjectParams {
    pub fn read_list(c: &mut Cursor) -> Result<Vec<Self>, OutOfBounds> {
        let n = c.u16()? as usize;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let mut p = ObjectParams { ints: [0; 10], shorts: [0; 10], ints2: [0; 10], shorts2: [0; 8] };
            c.u16()?;
            for x in p.ints.iter_mut() {
                *x = c.i32()?;
            }
            c.u16()?;
            for x in p.shorts.iter_mut() {
                *x = i16(c)?;
            }
            c.u16()?;
            for x in p.ints2.iter_mut() {
                *x = c.i32()?;
            }
            c.u16()?;
            for x in p.shorts2.iter_mut() {
                *x = i16(c)?;
            }
            v.push(p);
        }
        Ok(v)
    }

    /// For `'Tele'` / `'Strt'` objects (class 0x41b0a0, trigger 0x41b180):
    /// what this params entry does.
    pub fn portal(&self) -> PortalAction {
        let i = &self.ints;
        match self.shorts[0] {
            1 | 2 | 7 => PortalAction::Teleport {
                snap_camera: self.shorts[0] == 2,
                mode: i[0],
                offset: [i[1], i[2], i[3]],
                heading: i[4],
            },
            3 => PortalAction::ChangeSection { section: i[0] as u16, level: i[1] as u16 },
            4 => PortalAction::Spawn { mode: i[0], offset: [i[1], i[2], i[3]], heading: i[4], extra: [i[5], i[6], i[7]] },
            5 | 6 => PortalAction::ReactOnly,
            k => PortalAction::Other(k),
        }
    }
}

/// Decoded portal trigger (see [`ObjectParams::portal`]). Kinds 1, 2, 5, 6
/// and 7 also make Mendel play a reaction (`Game_CreatureReact` @ 0x41cb80
/// with a per-kind constant set by `Portal_InitReactions` @ 0x41b140).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalAction {
    /// Move Mendel next to this object (`Game_RequestTeleport` @ 0x41c690):
    /// `offset` from the object, facing `heading` (1024ths of a turn).
    /// Kind 2 also snaps the camera.
    Teleport { snap_camera: bool, mode: i32, offset: [i32; 3], heading: i32 },
    /// Dimensional gate (`Game_RequestSectionChange` @ 0x41c6e0): show the
    /// time screen of `level`, then load world section `section`.
    ChangeSection { section: u16, level: u16 },
    /// Level start (`Game_SpawnCreature` @ 0x41c590), used by `'Strt'`.
    Spawn { mode: i32, offset: [i32; 3], heading: i32, extra: [i32; 3] },
    ReactOnly,
    Other(i16),
}

/// The logic blocks of one world object, all optional.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectLogic {
    pub states: Option<StateMachine>,
    pub shape: Option<ShapeAnim>,
    pub sounds: Vec<SoundEmitter>,
    pub motions: Vec<MotionPath>,
    pub params: Vec<ObjectParams>,
}

impl ObjectLogic {
    /// Reads the blocks of an object given its `block_end` table, checking
    /// that every block ends exactly where the table says the next begins.
    pub fn read(data: &[u8], block_end: &[u32; 7]) -> Result<Self, Error> {
        let at = |i: usize| Cursor::at(data, block_end[i] as usize);
        let check = |c: &Cursor, next: usize, what| {
            if c.pos == block_end[next] as usize { Ok(()) } else { Err(Error::Malformed(what, c.pos)) }
        };
        let mut c = at(1);
        let states = if c.u16()? != 0 { Some(StateMachine::read(&mut c)?) } else { None };
        check(&c, 2, "state machine does not end at block_end[2]")?;
        let mut c = at(2);
        let shape = if c.u16()? != 0 { Some(ShapeAnim::read(&mut c)?) } else { None };
        check(&c, 3, "shape animation does not end at block_end[3]")?;
        let mut c = at(4);
        let sounds = if c.u16()? != 0 { SoundEmitter::read_list(&mut c)? } else { Vec::new() };
        check(&c, 5, "sound emitters do not end at block_end[5]")?;
        let mut c = at(5);
        let motions = if c.u16()? != 0 { MotionPath::read_list(&mut c)? } else { Vec::new() };
        check(&c, 6, "motion paths do not end at block_end[6]")?;
        let mut c = at(6);
        let params = if c.u16()? != 0 { ObjectParams::read_list(&mut c)? } else { Vec::new() };
        check(&c, 0, "params do not end at block_end[0]")?;
        Ok(Self { states, shape, sounds, motions, params })
    }
}

/// One camera style (14 bytes; `Camera_ReadStyles` @ 0x414060). The
/// 468-byte section trailer is exactly `u32 (ignored) + u16 33 + 33 x 14`,
/// identical in all eight sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraStyle {
    /// 16.16 fractions (0.6 .. 0.95): smoothing factors of the camera
    /// position / orientation chase (inferred).
    pub damping: [u32; 2],
    /// e.g. 15 / 300, 30 / 700: speed limits or distances (unknown).
    pub limits: [i16; 2],
    /// 0..=11: what the camera position is relative to
    /// (`Camera_ApplyPolygonKey` @ 0x412f60): 0/7 fixed world point;
    /// 1/8 and 4 relative to the object Mendel stands on; 2/3/9 and 5/6
    /// relative to Mendel; 10/11 x, y from the object and z from Mendel.
    pub mode: i16,
}

impl CameraStyle {
    pub fn read_table(data: &[u8], at: usize) -> Result<Vec<Self>, OutOfBounds> {
        let mut c = Cursor::at(data, at);
        c.u32()?;
        let n = c.u16()? as usize;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(Self { damping: [c.u32()?, c.u32()?], limits: [i16(&mut c)?, i16(&mut c)?], mode: i16(&mut c)? });
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worlds::{Archive, Object};

    fn data() -> Option<Vec<u8>> {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        std::fs::read(p).ok()
    }

    struct View {
        clicked: bool,
        ticks: u16,
        states: Vec<u16>,
    }
    impl LogicView for View {
        fn clicked(&self) -> bool {
            self.clicked
        }
        fn ticks(&self) -> u16 {
            self.ticks
        }
        fn events(&self) -> u32 {
            0
        }
        fn previous_events(&self) -> u32 {
            1
        }
        fn random(&self) -> u32 {
            42
        }
        fn position(&self) -> [i32; 3] {
            [0, 10, 0]
        }
        fn rotation(&self) -> [i16; 3] {
            [0, 1024 + 5, 0]
        }
        fn state_of(&self, id: u16) -> u16 {
            self.states[id as usize]
        }
    }

    #[test]
    fn synthetic_state_machine() {
        // 1 state, entry: appearance 1; flag 0; 1 transition: goto 0 + reset,
        // groups: [Clicked] OR [Ticks>=24, ObjectInState{1, 2}]
        let mut b: Vec<u8> = vec![1, 0];
        let acts = |b: &mut Vec<u8>, v: [i16; 6], f: [u8; 8]| {
            for x in v {
                b.extend_from_slice(&x.to_le_bytes());
            }
            b.extend_from_slice(&f);
        };
        acts(&mut b, [0, 0, 1, 0, 0, 0], [1, 0, 0, 0, 1, 0, 0, 0]);
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&1i16.to_le_bytes());
        acts(&mut b, [0, 0, 0, 0, 0, 0], [1, 1, 0, 0, 0, 0, 0, 1]);
        b.extend_from_slice(&2i16.to_le_bytes());
        b.extend_from_slice(&1i16.to_le_bytes());
        b.extend_from_slice(&[1, 0, 0, 0, 0, 0]);
        b.extend_from_slice(&2i16.to_le_bytes());
        b.extend_from_slice(&[3, 0, 24, 0, 0, 0]);
        b.extend_from_slice(&[0x16, 0, 1, 0, 2, 0]);
        let m = StateMachine::read(&mut Cursor::new(&b)).unwrap();
        assert_eq!(m.states[0].entry.appearance, Some(1));
        let t = &m.states[0].transitions[0];
        assert_eq!(t.actions.goto_state, Some(0));
        assert!(t.actions.reset_timer);
        assert_eq!(t.groups[1][1], Condition::ObjectInState { id: 1, state: 2 });
        let mut v = View { clicked: false, ticks: 23, states: vec![0, 2] };
        assert!(m.evaluate(0, &v).is_none());
        v.ticks = 24;
        let a = m.resolve(m.evaluate(0, &v).unwrap());
        assert_eq!((a.goto_state, a.appearance), (Some(0), Some(1)));
        assert!(Condition::EventEnded(1).eval(&v));
        assert!(Condition::RotationAtMost(1, 5).eval(&v));
    }

    #[test]
    fn motion_expansion() {
        let k = |x, s| MotionKey { position: [x, 0, 0], rotation: [0; 3], steps: s };
        let p = MotionPath { looping: 1, relative: 1, frame_count: 5, keys: vec![k(0, 3), k(-4000, 0)] };
        let f = p.frames();
        assert_eq!(f.len(), 5);
        assert_eq!(f[2].0[0], -2000);
    }

    #[test]
    fn every_object_logic_parses_and_ends_on_its_block() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let mut ops = [0usize; 0x18];
        let mut gates = Vec::new();
        for i in 0..a.sections.len() {
            let s = a.world(i).unwrap();
            let n = s.objects.len();
            let styles = CameraStyle::read_table(&d, s.trailer.start).unwrap();
            assert_eq!(styles.len(), 33);
            assert!(styles.iter().all(|st| (0..=11).contains(&st.mode)));
            for (idx, o) in s.objects.iter().enumerate() {
                let Object::World(o) = o else { continue };
                let l = &o.logic;
                // every non-camera object has a state machine, and its
                // initial state is in range
                let sm = l.states.as_ref().unwrap();
                assert!((o.initial_state() as usize) < sm.states.len());
                for st in &sm.states {
                    for t in &st.transitions {
                        for c in t.groups.iter().flatten() {
                            if let Condition::ObjectInState { id, state } | Condition::ObjectNotInState { id, state } = *c {
                                assert!(id >= 1 && (id as usize) <= n);
                                if let Object::World(r) = &s.objects[id as usize - 1] {
                                    assert!((state as usize) < r.logic.states.as_ref().unwrap().states.len());
                                }
                            }
                            let op = match c {
                                Condition::Unknown(op, _) => panic!("unknown opcode {op}"),
                                Condition::ObjectInState { .. } => 0x16,
                                Condition::ObjectNotInState { .. } => 0x17,
                                _ => 1,
                            };
                            ops[op] += 1;
                        }
                    }
                }
                if let (Some(shape), Some(mesh)) = (&l.shape, &o.mesh) {
                    for f in shape.states.iter().flat_map(|s| &s.frames) {
                        assert_eq!(f.vertices.len(), mesh.vertices.len());
                        assert_eq!(f.polygon_mapping.len(), mesh.polygons.len());
                    }
                }
                for p in &l.motions {
                    assert_eq!(p.frames().len(), p.frame_count as usize);
                }
                if &o.kind == b"Tele" {
                    for p in &l.params {
                        if let PortalAction::ChangeSection { section, level } = p.portal() {
                            gates.push((i, idx, section, level));
                        }
                    }
                }
                if &o.kind == b"Strt" {
                    assert!(matches!(l.params[0].portal(), PortalAction::Spawn { .. }));
                }
            }
        }
        assert_eq!(ops[0x16], 2195);
        assert_eq!(ops[0x17], 368);
        let g: Vec<_> = gates.iter().map(|&(s, _, to, lvl)| (s, to, lvl)).collect();
        assert_eq!(g, vec![(0, 1, 0), (1, 2, 1), (2, 3, 2), (2, 6, 6), (3, 4, 3), (4, 5, 4), (5, 2, 5), (6, 7, 7)]);
    }
}
