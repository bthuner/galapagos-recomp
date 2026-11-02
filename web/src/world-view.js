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
// `onTouch` (touching Mendel). In follow mode the camera chases Mendel from
// behind and above; dragging orbits around him for a while.

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
in vec2 vUV; flat in float vLayer; in float vLight; in float vDepth;
out vec4 color;
void main() {
  if (vLayer < -1.5) discard; // hidden in the current appearance state
  // Textures repeat, as in the original's wrap-around texel addressing.
  vec3 c = vLayer < 0.0 ? vec3(0.55) : texture(uTex, vec3(fract(vUV), vLayer)).rgb;
  // Depth cueing: surfaces fade to black with distance, as the original's
  // shade table did (visible in gameplay footage).
  c *= uBright * vLight * (1.0 - 0.85 * clamp(vDepth / uFar, 0.0, 1.0));
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

// Follow camera, not in the original (whose camera jumps between per-floor
// keys, docs/re/objects.md §6): a chase cam that eases towards a point
// behind and above Mendel. Distances are world units (Mendel is ~900 long).
const CHASE = { dist: 2600, pitch: 0.32, lift: 250, ease: 2.5, yawEase: 1.6, idle: 2.5 };

export function createWorldView(canvas, { onFrame = () => {}, onPick = () => {}, onTouch = () => {}, onFollowChange = () => {}, clearance = () => 1 } = {}) {
  const gl = canvas.getContext("webgl2", { antialias: true });
  if (!gl) throw new Error("WebGL2 is not available");
  const prog = compile(gl, VS, FS);
  const uViewProj = gl.getUniformLocation(prog, "uViewProj");
  const uFar = gl.getUniformLocation(prog, "uFar");
  const uBright = gl.getUniformLocation(prog, "uBright");
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

  // Orbit camera state. Galapagos' Y axis points *down*, like screen
  // coordinates (checked visually: with Y-up, every tower hangs from the
  // sky), so "up" is -Y.
  const cam = { yaw: 0.8, pitch: 0.5, dist: 1, target: [0, 0, 0], up: -1 };
  let radius = 1;
  let fadeFar = 1;
  // Follow mode: `creature()` returns the engine's creature state.
  const follow = { on: false, creature: null, userYaw: 0, userPitch: 0, dist: CHASE.dist, idle: 0, snap: true };
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
  let eyeNow = [0, 0, 0], basis = null, viewProj = null;
  const FOV = 1.0;

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
    if (follow.on) chase(dt);
    const { yaw, pitch, dist, target } = cam;
    const eye = [
      target[0] + dist * Math.cos(pitch) * Math.sin(yaw),
      target[1] + cam.up * dist * Math.sin(pitch),
      target[2] + dist * Math.cos(pitch) * Math.cos(yaw),
    ];
    // Close to Mendel the near plane must be close too.
    const near = follow.on ? 40 : radius / 500;
    const proj = perspective(FOV, w / h, near, radius * 8);
    eyeNow = eye;
    basis = { target, aspect: w / h };
    gl.useProgram(prog);
    viewProj = mul(proj, lookAt(eye, target, [0, cam.up, 0]));
    gl.uniformMatrix4fv(uViewProj, false, viewProj);
    gl.uniform1f(uFar, fadeFar);
    gl.uniform1f(uBright, brightness);
    gl.bindVertexArray(vao);
    onFrame();
    // Upload only what the simulation patched (appearance changes).
    const dirty = current.takeDirty?.();
    if (dirty) {
      gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
      const [a, b] = dirty;
      gl.bufferSubData(gl.ARRAY_BUFFER, a * 7 * 4, current.liveVertices().subarray(a * 7, b * 7));
    }
    const { draws } = current;
    const transforms = current.transforms();
    for (let i = 0; i < draws.length / 2; i++) {
      gl.uniformMatrix4fv(uModel, false, transforms.subarray(i * 16, i * 16 + 16));
      gl.drawArrays(gl.TRIANGLES, draws[i * 2], draws[i * 2 + 1]);
    }
    // Mendel: a few hundred triangles, re-uploaded every frame.
    const mendel = current.creatureVertices?.();
    if (mendel && mendel.length) {
      gl.bindVertexArray(mvao);
      gl.bindBuffer(gl.ARRAY_BUFFER, mvbo);
      gl.bufferData(gl.ARRAY_BUFFER, mendel, gl.DYNAMIC_DRAW);
      gl.uniformMatrix4fv(uModel, false, IDENTITY);
      gl.drawArrays(gl.TRIANGLES, 0, mendel.length / 7);
    }
  }

  // The chase: ease the orbit target onto Mendel and the orbit angle to
  // "behind him". Exponential easing (k = 1 - e^(-rate·dt)) is frame-rate
  // independent. After a drag the user's offset fades back to zero.
  function chase(dt) {
    const c = follow.creature?.();
    if (!c || c.mode === "absent") return;
    const [x, y, z] = c.position;
    const goal = [x, y - CHASE.lift, z]; // -Y is up
    const h = (c.heading / 1024) * 2 * Math.PI;
    // The eye sits at target + dist·(cos p·sin yaw, …, cos p·cos yaw): put
    // it opposite his forward (cos h, 0, sin h).
    const behind = Math.atan2(-Math.cos(h), -Math.sin(h));
    if (!drag) follow.idle += dt;
    if (follow.idle > CHASE.idle) {
      const k = 1 - Math.exp(-dt * 0.8);
      follow.userYaw -= follow.userYaw * k;
      follow.userPitch -= follow.userPitch * k;
    }
    const wantYaw = behind + follow.userYaw;
    const wantPitch = Math.max(-0.2, Math.min(1.4, CHASE.pitch + follow.userPitch));
    // Too far behind (a long fall, a teleport): cut instead of chasing.
    if (Math.hypot(...goal.map((g, i) => g - cam.target[i])) > 5000) follow.snap = true;
    if (follow.snap) {
      cam.target = goal;
      cam.yaw = wantYaw;
      cam.pitch = wantPitch;
      cam.dist = follow.dist;
      follow.snap = false;
      return;
    }
    const k = 1 - Math.exp(-dt * CHASE.ease);
    cam.target = cam.target.map((t, i) => t + (goal[i] - t) * k);
    // Shortest way round the circle.
    const dy = Math.atan2(Math.sin(wantYaw - cam.yaw), Math.cos(wantYaw - cam.yaw));
    cam.yaw += dy * (1 - Math.exp(-dt * CHASE.yawEase));
    cam.pitch += (wantPitch - cam.pitch) * k;
    // Walls between Mendel and the eye pull the camera in (fast), then it
    // drifts back out (slowly) once the view is clear.
    const dir = [Math.cos(cam.pitch) * Math.sin(cam.yaw), cam.up * Math.sin(cam.pitch), Math.cos(cam.pitch) * Math.cos(cam.yaw)];
    const free = clearance(cam.target, cam.target.map((t, i) => t + dir[i] * follow.dist));
    const want = Math.max(350, follow.dist * free - 150);
    cam.dist += (want - cam.dist) * (want < cam.dist ? 1 - Math.exp(-dt * 12) : k);
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
    const fwd = norm(basis.target.map((t, i) => t - eyeNow[i]));
    const right = norm(cross(fwd, [0, cam.up, 0]));
    const up = cross(right, fwd);
    const k = Math.tan(FOV / 2);
    const dir = norm(fwd.map((f, i) => f + nx * k * basis.aspect * right[i] + ny * k * up[i]));
    return [eyeNow, dir];
  }
  canvas.addEventListener("contextmenu", (e) => e.preventDefault());
  canvas.addEventListener("pointermove", (e) => {
    if (!drag) return;
    const dx = e.clientX - drag.x, dy = e.clientY - drag.y;
    [drag.x, drag.y] = [e.clientX, e.clientY];
    follow.idle = 0;
    if (drag.pan) {
      // Panning means "I want to look elsewhere": leave follow mode.
      if (follow.on) setFollow(false);
      const s = cam.dist / canvas.clientHeight;
      cam.target[0] -= (Math.cos(cam.yaw) * dx) * s;
      cam.target[2] += (Math.sin(cam.yaw) * dx) * s;
      cam.target[1] += cam.up * dy * s;
    } else if (follow.on) {
      follow.userYaw -= dx * 0.005;
      follow.userPitch += dy * 0.005;
      cam.yaw -= dx * 0.005;
      cam.pitch = Math.max(-1.5, Math.min(1.5, cam.pitch + dy * 0.005));
    } else {
      cam.yaw -= dx * 0.005;
      cam.pitch = Math.max(-1.5, Math.min(1.5, cam.pitch + dy * 0.005));
    }
  });
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    if (follow.on) follow.dist = Math.max(600, Math.min(40000, follow.dist * Math.exp(e.deltaY * 0.001)));
    else cam.dist *= Math.exp(e.deltaY * 0.001);
  }, { passive: false });

  function setFollow(on) {
    if (follow.on === on) return;
    follow.on = on;
    follow.snap = on;
    follow.userYaw = follow.userPitch = 0;
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
      const d = basis.target.map((t, i) => t - eyeNow[i]);
      const l = Math.hypot(...d) || 1;
      return { eye: eyeNow, forward: d.map((v) => v / l), up: [0, cam.up, 0] };
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
    flipUp() { cam.up = -cam.up; },
    // Chase Mendel: `creature` returns engine.creature.state().
    follow(on, creature = follow.creature) {
      follow.creature = creature;
      setFollow(on);
    },
    get following() { return follow.on; },
    // Jump straight to the chase position (teleports, camera-snap pads).
    snap() { follow.snap = true; },
    stop() { running = false; },
  };
}
