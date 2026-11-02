// A WebGL2 viewer for one world section.
//
// The engine gives us a flat array of triangles (x, y, z, u, v, layer, light)
// and a stack of 128×128 textures. WebGL2's TEXTURE_2D_ARRAY holds the whole
// stack in one object, so no texture switches are needed. Vertices are local
// to their object, and each object is drawn with its own model matrix, which
// the simulation can change from frame to frame (moving platforms, doors…).
//
// Mendel is drawn from a second, small vertex buffer that the engine
// rebuilds every tick (his posed mesh, already in world space), with the
// same shader and an identity model matrix.
//
// Camera: drag to orbit, wheel to zoom, shift-drag (or middle-drag) to pan.
// A left click (press and release without dragging) becomes a world-space
// ray handed to `onPick` (the objects' picker); a right click goes to
// `onTouch` (touching Mendel). In follow mode we show the original game
// camera, which the engine runs (crates/engine/src/camera.rs); dragging
// orbits around Mendel for a while, then hands the view back.

const VS = `#version 300 es
layout(location=0) in vec3 aPos;
layout(location=1) in vec2 aUV;
layout(location=2) in float aLayer;
layout(location=3) in float aLight;
uniform mat4 uViewProj;
uniform mat4 uModel;
out vec2 vUV; flat out float vLayer; out float vLight; out float vDepth;
void main() {
  vUV = aUV; vLayer = aLayer; vLight = aLight;
  gl_Position = uViewProj * uModel * vec4(aPos, 1.0);
  vDepth = gl_Position.w;
}`;

const FS = `#version 300 es
precision highp float;
precision highp sampler2DArray;
uniform sampler2DArray uTex;
uniform float uFar;
uniform float uBright; // Preferences ▸ Brightness, 1.0 = Standard
// The palette flash (flash.rs): each channel becomes lo + c * (hi - lo).
uniform vec3 uTintLo;
uniform vec3 uTintHi;
// 0: the opaque pass (glass skipped); 1: the glass pass (only glass).
uniform int uGlass;
in vec2 vUV; flat in float vLayer; in float vLight; in float vDepth;
out vec4 color;
void main() {
  // Layer -4 is glass (world.rs GLASS): in 1997 an untextured grey, added
  // onto what's behind (Direct3D blend ONE, ONE). The engine computed that
  // grey from the polygon's material and passes it in u.
  bool glass = vLayer > -4.5 && vLayer < -3.5;
  if (glass != (uGlass == 1)) discard;
  if (glass) {
    if (vUV.x <= 0.0) discard; // level 0: the exe skips it
    vec3 g = vec3(vUV.x * (1.0 - 0.85 * clamp(vDepth / uFar, 0.0, 1.0)));
    // Additive light takes only the flash's scale, not its offset.
    color = vec4(clamp(g * (uTintHi - uTintLo), 0.0, 1.0), 1.0);
    return;
  }
  if (vLayer < -1.5) discard; // hidden in the current appearance state
  // Textures repeat, as in the original's wrap-around texel addressing.
  vec3 c = vLayer < 0.0 ? vec3(0.55) : texture(uTex, vec3(fract(vUV), vLayer)).rgb;
  // Depth cueing: surfaces fade to black with distance, as the original's
  // shade table did (visible in gameplay footage).
  c *= uBright * vLight * (1.0 - 0.85 * clamp(vDepth / uFar, 0.0, 1.0));
  // The 1997 game remapped its 8-bit palette for a flash; we remap the
  // final colour the same way.
  c = clamp(uTintLo + clamp(c, 0.0, 1.0) * (uTintHi - uTintLo), 0.0, 1.0);
  color = vec4(c, 1.0);
}`;

// ——— tiny matrix kit (column-major, like WebGL expects) ———
function perspective(fovY, aspect, near, far) {
  const f = 1 / Math.tan(fovY / 2), nf = 1 / (near - far);
  return [f / aspect, 0, 0, 0, 0, f, 0, 0, 0, 0, (far + near) * nf, -1, 0, 0, 2 * far * near * nf, 0];
}
function lookAt(eye, at, up) {
  const sub = (a, b) => a.map((v, i) => v - b[i]);
  const norm = (a) => { const l = Math.hypot(...a); return a.map((v) => v / l); };
  const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
  const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
  const z = norm(sub(eye, at)), x = norm(cross(up, z)), y = cross(z, x);
  return [x[0], y[0], z[0], 0, x[1], y[1], z[1], 0, x[2], y[2], z[2], 0, -dot(x, eye), -dot(y, eye), -dot(z, eye), 1];
}
function mul(a, b) {
  const o = new Array(16).fill(0);
  for (let c = 0; c < 4; c++) for (let r = 0; r < 4; r++) for (let k = 0; k < 4; k++) o[c * 4 + r] += a[k * 4 + r] * b[c * 4 + k];
  return o;
}

function compile(gl, vs, fs) {
  const p = gl.createProgram();
  for (const [type, src] of [[gl.VERTEX_SHADER, vs], [gl.FRAGMENT_SHADER, fs]]) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
    gl.attachShader(p, s);
  }
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p));
  return p;
}

const IDENTITY = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);

// The original's field of view: 110° in `View_Init` @ 0x4033bb, turned
// into a focal length of width / tan(55°) with square pixels (0x403580).
// inferred: "width" is the full viewport width, which makes the vertical
// field 2·atan(0.375·tan 55°) ≈ 56° on a 4:3 screen.
const ORIGINAL_FOV = 2 * Math.atan(0.375 * Math.tan((55 * Math.PI) / 180));
// Drag-to-look in follow mode (not in the original): how long the user's
// view stays after the last drag, and how long it takes to hand back.
const LOOK = { idle: 2.5, back: 1.2 };

const v3 = {
  lerp: (a, b, t) => a.map((x, i) => x + (b[i] - x) * t),
  norm: (a) => { const l = Math.hypot(...a) || 1; return a.map((x) => x / l); },
  cross: (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]],
};

export function createWorldView(canvas, { onFrame = () => {}, onPick = () => {}, onTouch = () => {}, onFollowChange = () => {}, camera = () => null } = {}) {
  const gl = canvas.getContext("webgl2", { antialias: true });
  if (!gl) throw new Error("WebGL2 is not available");
  const prog = compile(gl, VS, FS);
  const uViewProj = gl.getUniformLocation(prog, "uViewProj");
  const uFar = gl.getUniformLocation(prog, "uFar");
  const uBright = gl.getUniformLocation(prog, "uBright");
  const uTintLo = gl.getUniformLocation(prog, "uTintLo");
  const uTintHi = gl.getUniformLocation(prog, "uTintHi");
  const uGlass = gl.getUniformLocation(prog, "uGlass");
  // Preferences: brightness multiplier and render resolution. `pixels` is
  // the canvas height in device pixels to aim for (0 = native).
  let brightness = 1, pixels = 0;
  const uModel = gl.getUniformLocation(prog, "uModel");
  let current = null;
  const vao = gl.createVertexArray();
  const vbo = gl.createBuffer();
  const tex = gl.createTexture();
  let count = 0;
  // Mendel's buffer: same attribute layout, refilled every frame.
  const mvao = gl.createVertexArray();
  const mvbo = gl.createBuffer();
  gl.bindVertexArray(mvao);
  gl.bindBuffer(gl.ARRAY_BUFFER, mvbo);
  for (const [loc, size, off] of [[0, 3, 0], [1, 2, 3], [2, 1, 5], [3, 1, 6]]) {
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, size, gl.FLOAT, false, 7 * 4, off * 4);
  }

  // Orbit camera state. Galapagos is Z-up: floors face +z, gravity pulls
  // along -z, and the original renderer uses camera z as screen-up
  // (0x4302e0). `yaw` turns about +z, `pitch` lifts the eye towards +z.
  // (Our first viewer guessed Y-down from a distant orbit view; the
  // camera port and the 1997 footage proved Z-up.)
  const UP = [0, 0, 1];
  const cam = { yaw: 0.8, pitch: 0.5, dist: 1, target: [0, 0, 0], flip: 1 };
  let radius = 1;
  let fadeFar = 1;
  // Follow mode: the engine's camera, and an optional user look-around
  // (`look`: an orbit around Mendel that fades back to the original).
  const follow = { on: false, creature: null, look: null, cut: false };
  let lastTime = performance.now();

  function load(world) {
    gl.bindVertexArray(vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
    gl.bufferData(gl.ARRAY_BUFFER, world.vertices, gl.STATIC_DRAW);
    const stride = 7 * 4;
    const attr = (loc, size, off) => {
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribPointer(loc, size, gl.FLOAT, false, stride, off * 4);
    };
    attr(0, 3, 0); attr(1, 2, 3); attr(2, 1, 5); attr(3, 1, 6);
    count = world.vertices.length / 7;
    current = world;

    gl.bindTexture(gl.TEXTURE_2D_ARRAY, tex);
    const layers = Math.max(1, world.textureCount);
    gl.texImage3D(gl.TEXTURE_2D_ARRAY, 0, gl.RGBA8, 128, 128, layers, 0, gl.RGBA, gl.UNSIGNED_BYTE,
      world.textureCount ? world.textures : new Uint8Array(128 * 128 * 4));
    gl.generateMipmap(gl.TEXTURE_2D_ARRAY);
    gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR);

    const [x0, y0, z0, x1, y1, z1] = world.bounds;
    cam.target = [(x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2];
    radius = Math.hypot(x1 - x0, y1 - y0, z1 - z0) / 2;
    cam.dist = radius * 1.6;
    // A far clip of 999999 means "no limit"; fall back to the world's size.
    fadeFar = world.farClip > 0 && world.farClip < 900000 ? world.farClip : radius * 4;
  }

  // The camera basis of the last frame, for turning clicks into rays.
  let basis = null, viewProj = null;
  const ORBIT_FOV = 1.0;

  function draw() {
    const w = canvas.clientWidth, h = canvas.clientHeight;
    const scale = pixels ? pixels / h : devicePixelRatio;
    if (canvas.width !== Math.round(w * scale)) {
      canvas.width = Math.round(w * scale);
      canvas.height = Math.round(h * scale);
      // Low resolutions are shown with hard pixels, like a 1997 mode switch.
      canvas.style.imageRendering = pixels ? "pixelated" : "auto";
    }
    gl.viewport(0, 0, canvas.width, canvas.height);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
    gl.enable(gl.DEPTH_TEST);
    if (!count) return;

    const now = performance.now();
    const dt = Math.min(0.1, (now - lastTime) / 1000);
    lastTime = now;
    // Run the simulation first: the camera we draw is the one it produced.
    const alpha = onFrame() ?? 1;
    const view = (follow.on && engineView(alpha, dt)) || orbitView();
    // Close to Mendel the near plane must be close too.
    const near = follow.on ? 40 : radius / 500;
    const fov = follow.on ? ORIGINAL_FOV : ORBIT_FOV;
    const proj = perspective(fov, w / h, near, radius * 8);
    basis = { ...view, aspect: w / h, fov };
    gl.useProgram(prog);
    viewProj = mul(proj, lookAt(view.eye, view.eye.map((e, i) => e + view.forward[i]), view.up));
    gl.uniformMatrix4fv(uViewProj, false, viewProj);
    gl.uniform1f(uFar, fadeFar);
    gl.uniform1f(uBright, brightness);
    const t = current.tint?.() ?? [0, 255, 0, 255, 0, 255];
    gl.uniform3f(uTintLo, t[0] / 255, t[2] / 255, t[4] / 255);
    gl.uniform3f(uTintHi, t[1] / 255, t[3] / 255, t[5] / 255);
    gl.bindVertexArray(vao);
    // Upload only what the simulation patched (appearance changes).
    const dirty = current.takeDirty?.();
    if (dirty) {
      gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
      const [a, b] = dirty;
      gl.bufferSubData(gl.ARRAY_BUFFER, a * 7 * 4, current.liveVertices().subarray(a * 7, b * 7));
    }
    const { draws } = current;
    const transforms = current.transforms();
    const visible = current.visible?.();
    const drawWorld = () => {
      for (let i = 0; i < draws.length / 2; i++) {
        if (visible && !visible[i]) continue; // hidden by a -5 appearance frame
        gl.uniformMatrix4fv(uModel, false, transforms.subarray(i * 16, i * 16 + 16));
        gl.drawArrays(gl.TRIANGLES, draws[i * 2], draws[i * 2 + 1]);
      }
    };
    gl.uniform1i(uGlass, 0);
    drawWorld();
    // Mendel: a few hundred triangles, re-uploaded every frame.
    const mendel = current.creatureVertices?.();
    if (mendel && mendel.length) {
      gl.bindVertexArray(mvao);
      gl.bindBuffer(gl.ARRAY_BUFFER, mvbo);
      gl.bufferData(gl.ARRAY_BUFFER, mendel, gl.DYNAMIC_DRAW);
      gl.uniformMatrix4fv(uModel, false, IDENTITY);
      gl.drawArrays(gl.TRIANGLES, 0, mendel.length / 7);
    }
    // Glass last, over everything it covers (Mendel in the capsule too):
    // depth-tested but not depth-writing, added onto the picture, as the
    // 1997 Direct3D renderer drew its glass bucket. A few thousand
    // triangles drawn twice cost nothing next to sorting them out.
    gl.bindVertexArray(vao);
    gl.uniform1i(uGlass, 1);
    gl.depthMask(false);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE);
    drawWorld();
    gl.disable(gl.BLEND);
    gl.depthMask(true);
  }

  // The orbit camera: eye on a sphere around `cam.target`.
  function orbitAt(target, yaw, pitch, dist) {
    const eye = [
      target[0] + dist * Math.cos(pitch) * Math.sin(yaw),
      target[1] + dist * Math.cos(pitch) * Math.cos(yaw),
      target[2] + cam.flip * dist * Math.sin(pitch),
    ];
    const forward = v3.norm(target.map((t, i) => t - eye[i]));
    const right = v3.norm(v3.cross(forward, UP.map((u) => u * cam.flip)));
    return { eye, forward, up: v3.cross(right, forward) };
  }
  const orbitView = () => orbitAt(cam.target, cam.yaw, cam.pitch, cam.dist);

  // The original camera, from the engine: it moves at 24 Hz, so we draw
  // the previous tick's view blended towards the current one by `alpha`,
  // the fraction of the next tick already elapsed. not in the original:
  // it rendered whatever the last tick left (its tick takes a sub-tick
  // fraction argument, 0x4070b0, but Game_Tick passes a full tick).
  function engineView(alpha, dt) {
    const s = camera();
    if (!s) return null;
    const a = follow.cut ? 1 : Math.max(0, Math.min(1, alpha));
    follow.cut = false;
    let v = {
      eye: v3.lerp(s.prev.eye, s.cur.eye, a),
      forward: v3.norm(v3.lerp(s.prev.forward, s.cur.forward, a)),
      up: v3.norm(v3.lerp(s.prev.up, s.cur.up, a)),
    };
    const look = follow.look;
    if (look) {
      // not in the original: the user's look-around, an orbit around
      // Mendel that fades back into the original camera when left alone.
      const c = follow.creature?.();
      if (!drag) look.idle += dt;
      if (look.idle > LOOK.idle) look.weight -= dt / LOOK.back;
      if (!c || look.weight <= 0) {
        follow.look = null;
      } else {
        const o = orbitAt(c.position, look.yaw, look.pitch, look.dist);
        const k = look.weight * look.weight * (3 - 2 * look.weight); // smoothstep
        v = {
          eye: v3.lerp(v.eye, o.eye, k),
          forward: v3.norm(v3.lerp(v.forward, o.forward, k)),
          up: v3.norm(v3.lerp(v.up, o.up, k)),
        };
      }
    }
    return v;
  }

  // Start (or keep) looking around Mendel from where the camera is now.
  function lookAround() {
    if (!follow.look) {
      const c = follow.creature?.();
      if (!c || !basis) return null;
      const d = basis.eye.map((e, i) => e - c.position[i]);
      const dist = Math.max(600, Math.hypot(...d));
      follow.look = {
        yaw: Math.atan2(d[0], d[1]),
        pitch: Math.asin(Math.max(-1, Math.min(1, d[2] / (cam.flip * dist)))),
        dist, idle: 0, weight: 1,
      };
    }
    follow.look.idle = 0;
    follow.look.weight = 1;
    return follow.look;
  }

  // Input: orbit / pan / zoom.
  let drag = null;
  canvas.addEventListener("pointerdown", (e) => {
    drag = { x: e.clientX, y: e.clientY, x0: e.clientX, y0: e.clientY, button: e.button, pan: e.button === 1 || e.shiftKey };
    if (e.button === 1) e.preventDefault(); // no auto-scroll
    canvas.setPointerCapture(e.pointerId);
  });
  canvas.addEventListener("pointerup", (e) => {
    const moved = drag && Math.hypot(e.clientX - drag.x0, e.clientY - drag.y0) > 4;
    if (drag && !moved && !drag.pan && basis) {
      if (drag.button === 0) onPick(...rayThrough(e));
      if (drag.button === 2) onTouch(...rayThrough(e));
    }
    drag = null;
  });

  // The ray from the eye through the clicked pixel, in world space.
  function rayThrough(e) {
    const r = canvas.getBoundingClientRect();
    const nx = ((e.clientX - r.left) / r.width) * 2 - 1;
    const ny = 1 - ((e.clientY - r.top) / r.height) * 2;
    const norm = (a) => { const l = Math.hypot(...a); return a.map((v) => v / l); };
    const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    const fwd = norm(basis.forward);
    const right = norm(cross(fwd, basis.up));
    const up = cross(right, fwd);
    const k = Math.tan(basis.fov / 2);
    const dir = norm(fwd.map((f, i) => f + nx * k * basis.aspect * right[i] + ny * k * up[i]));
    return [basis.eye, dir];
  }
  canvas.addEventListener("contextmenu", (e) => e.preventDefault());
  canvas.addEventListener("pointermove", (e) => {
    if (!drag) return;
    const dx = e.clientX - drag.x, dy = e.clientY - drag.y;
    [drag.x, drag.y] = [e.clientX, e.clientY];
    if (drag.pan) {
      // Panning means "I want to look elsewhere": leave follow mode.
      if (follow.on) setFollow(false);
      const s = cam.dist / canvas.clientHeight;
      cam.target[0] -= (Math.cos(cam.yaw) * dx) * s;
      cam.target[1] += (Math.sin(cam.yaw) * dx) * s;
      cam.target[2] += cam.flip * dy * s;
    } else if (follow.on) {
      const look = lookAround();
      if (look) {
        look.yaw -= dx * 0.005;
        look.pitch = Math.max(-1.5, Math.min(1.5, look.pitch + dy * 0.005));
      }
    } else {
      cam.yaw -= dx * 0.005;
      cam.pitch = Math.max(-1.5, Math.min(1.5, cam.pitch + dy * 0.005));
    }
  });
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    const look = follow.on && lookAround();
    if (look) look.dist = Math.max(600, Math.min(40000, look.dist * Math.exp(e.deltaY * 0.001)));
    else if (!follow.on) cam.dist *= Math.exp(e.deltaY * 0.001);
  }, { passive: false });

  function setFollow(on) {
    if (follow.on === on) return;
    follow.on = on;
    follow.look = null;
    follow.cut = true;
    // Leaving follow mode: orbit around where the camera was looking.
    if (!on && basis) {
      const c = follow.creature?.();
      if (c) cam.target = [...c.position];
      const d = basis.eye.map((e, i) => e - cam.target[i]);
      cam.dist = Math.max(600, Math.hypot(...d));
      cam.yaw = Math.atan2(d[0], d[1]);
      cam.pitch = Math.asin(Math.max(-1, Math.min(1, d[2] / (cam.flip * cam.dist))));
    }
    onFollowChange(on);
  }

  let running = true;
  (function loop() {
    if (!running) return;
    draw();
    requestAnimationFrame(loop);
  })();

  return {
    load,
    setBrightness(b) { brightness = b; },
    // Render height in pixels (240, 480), or 0 for the display's native.
    setResolution(h) { pixels = h; },
    // Where the camera is and where it looks, for the audio listener.
    listener() {
      if (!basis) return null;
      return { eye: basis.eye, forward: v3.norm(basis.forward), up: v3.norm(basis.up) };
    },
    // World point → CSS pixel coordinates in the page (null if behind).
    project(p) {
      if (!viewProj) return null;
      const m = viewProj;
      const x = m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12];
      const y = m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13];
      const w = m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15];
      if (w <= 0) return null;
      const r = canvas.getBoundingClientRect();
      return [r.left + ((x / w + 1) / 2) * r.width, r.top + ((1 - y / w) / 2) * r.height];
    },
    // The world-space ray under a page coordinate: [origin, direction].
    rayAt: (clientX, clientY) => rayThrough({ clientX, clientY }),
    // Aim the orbit camera at a world point from a given distance.
    focus(point, dist = cam.dist) {
      cam.target = [...point];
      cam.dist = dist;
    },
    // Flip the up axis, for exploring the geometry upside down.
    flipUp() { cam.flip = -cam.flip; },
    // Show the original camera; `creature` returns engine.creature.state()
    // (drag-to-look orbits around him).
    follow(on, creature = follow.creature) {
      follow.creature = creature;
      setFollow(on);
    },
    get following() { return follow.on; },
    // A camera cut (teleports, camera-snap pads): no blending this frame,
    // and drop any look-around.
    snap() { follow.cut = true; follow.look = null; },
    stop() { running = false; },
  };
}
