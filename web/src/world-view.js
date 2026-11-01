// A WebGL2 viewer for one world section.
//
// The engine gives us a flat array of triangles (x, y, z, u, v, layer, light)
// and a stack of 128×128 textures. WebGL2's TEXTURE_2D_ARRAY holds the whole
// stack in one object, so no texture switches are needed. Vertices are local
// to their object, and each object is drawn with its own model matrix, which
// the simulation can change from frame to frame (moving platforms, doors…).
//
// Camera: drag to orbit, wheel to zoom, right-drag (or shift-drag) to pan.
// A click (press and release without dragging) is turned into a world-space
// ray and handed to `onPick`, which forwards it to the engine's picker.

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
in vec2 vUV; flat in float vLayer; in float vLight; in float vDepth;
out vec4 color;
void main() {
  if (vLayer < -1.5) discard; // hidden in the current appearance state
  // Textures repeat, as in the original's wrap-around texel addressing.
  vec3 c = vLayer < 0.0 ? vec3(0.55) : texture(uTex, vec3(fract(vUV), vLayer)).rgb;
  // Depth cueing: surfaces fade to black with distance, as the original's
  // shade table did (visible in gameplay footage).
  c *= vLight * (1.0 - 0.85 * clamp(vDepth / uFar, 0.0, 1.0));
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

export function createWorldView(canvas, { onFrame = () => {}, onPick = () => {} } = {}) {
  const gl = canvas.getContext("webgl2", { antialias: true });
  if (!gl) throw new Error("WebGL2 is not available");
  const prog = compile(gl, VS, FS);
  const uViewProj = gl.getUniformLocation(prog, "uViewProj");
  const uFar = gl.getUniformLocation(prog, "uFar");
  const uModel = gl.getUniformLocation(prog, "uModel");
  let current = null;
  const vao = gl.createVertexArray();
  const vbo = gl.createBuffer();
  const tex = gl.createTexture();
  let count = 0;

  // Orbit camera state. Galapagos' Y axis points *down*, like screen
  // coordinates (checked visually: with Y-up, every tower hangs from the
  // sky), so "up" is -Y.
  const cam = { yaw: 0.8, pitch: 0.5, dist: 1, target: [0, 0, 0], up: -1 };
  let radius = 1;
  let fadeFar = 1;

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
    if (canvas.width !== w * devicePixelRatio) {
      canvas.width = w * devicePixelRatio;
      canvas.height = h * devicePixelRatio;
    }
    gl.viewport(0, 0, canvas.width, canvas.height);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
    gl.enable(gl.DEPTH_TEST);
    if (!count) return;

    const { yaw, pitch, dist, target } = cam;
    const eye = [
      target[0] + dist * Math.cos(pitch) * Math.sin(yaw),
      target[1] + cam.up * dist * Math.sin(pitch),
      target[2] + dist * Math.cos(pitch) * Math.cos(yaw),
    ];
    const proj = perspective(FOV, w / h, radius / 500, radius * 8);
    eyeNow = eye;
    basis = { target, aspect: w / h };
    gl.useProgram(prog);
    viewProj = mul(proj, lookAt(eye, target, [0, cam.up, 0]));
    gl.uniformMatrix4fv(uViewProj, false, viewProj);
    gl.uniform1f(uFar, fadeFar);
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
  }

  // Input: orbit / pan / zoom.
  let drag = null;
  canvas.addEventListener("pointerdown", (e) => {
    drag = { x: e.clientX, y: e.clientY, x0: e.clientX, y0: e.clientY, pan: e.button === 2 || e.shiftKey };
    canvas.setPointerCapture(e.pointerId);
  });
  canvas.addEventListener("pointerup", (e) => {
    const moved = drag && Math.hypot(e.clientX - drag.x0, e.clientY - drag.y0) > 4;
    if (drag && !moved && !drag.pan && basis) onPick(...rayThrough(e));
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
    if (drag.pan) {
      const s = cam.dist / canvas.clientHeight;
      cam.target[0] -= (Math.cos(cam.yaw) * dx) * s;
      cam.target[2] += (Math.sin(cam.yaw) * dx) * s;
      cam.target[1] += cam.up * dy * s;
    } else {
      cam.yaw -= dx * 0.005;
      cam.pitch = Math.max(-1.5, Math.min(1.5, cam.pitch + dy * 0.005));
    }
  });
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    cam.dist *= Math.exp(e.deltaY * 0.001);
  }, { passive: false });

  let running = true;
  (function loop() {
    if (!running) return;
    draw();
    requestAnimationFrame(loop);
  })();

  return {
    load,
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
    stop() { running = false; },
  };
}
