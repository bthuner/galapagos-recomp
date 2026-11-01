// Scroll choreography for the making-of, plus the interactive hex figure.

const chapters = [...document.querySelectorAll(".chapter")];
const links = new Map([...document.querySelectorAll(".toc a")].map((a) => [a.hash.slice(1), a]));

// Reveal each chapter once, and highlight it in the table of contents.
const io = new IntersectionObserver(
  (entries) => {
    for (const e of entries) {
      if (!e.isIntersecting) continue;
      e.target.classList.add("is-in");
      for (const a of links.values()) a.classList.remove("is-active");
      links.get(e.target.id)?.classList.add("is-active");
    }
  },
  { rootMargin: "-40% 0px -50% 0px" },
);
chapters.forEach((c) => io.observe(c));

const bar = document.getElementById("progress");
addEventListener("scroll", () => {
  const max = document.documentElement.scrollHeight - innerHeight;
  bar.style.transform = `scaleX(${scrollY / max})`;
}, { passive: true });

// The `worlds` header: u16 section count, u16 default section, then eight
// u32 section offsets.
// These 36 bytes are facts about the file layout, not game content.
const HEADER = [8, 0x4601b5, 0x13a6a81, 0x20f0c23, 0x2fe2f38, 0x3fa1cfb, 0x4cf984c, 0x5c488b1, 0x68fe9cd];
const MB = (n) => (n / 1048576).toFixed(1) + " MB";

function hexFigure(fig) {
  const grid = document.createElement("div");
  grid.className = "hex__grid";
  const info = document.createElement("p");
  info.className = "hex__info";
  info.textContent = "Little-endian: the least significant byte comes first.";

  HEADER.forEach((value, i) => {
    for (let b = 0; b < 4; b++) {
      // The first u32 is really two u16s: count (bytes 0-1), default section (2-3).
      const field = i > 0 ? `off${i - 1}` : b < 2 ? "count" : "default";
      const text = i > 0
        ? `section[${i - 1}] starts at 0x${value.toString(16)} (${MB(value)} into the file)`
        : b < 2 ? "u16 section_count = 8" : "u16 default_section = 0";
      const span = document.createElement("span");
      span.className = "hex__b";
      span.dataset.f = field;
      span.textContent = ((value >>> (8 * b)) & 0xff).toString(16).padStart(2, "0");
      span.addEventListener("mouseenter", () => {
        grid.querySelectorAll(`[data-f="${field}"]`).forEach((s) => s.classList.add("is-hot"));
        info.textContent = text;
      });
      span.addEventListener("mouseleave", () => {
        grid.querySelectorAll(".is-hot").forEach((s) => s.classList.remove("is-hot"));
      });
      grid.append(span);
    }
  });
  fig.prepend(grid);
  fig.append(info);
}

const fig = document.getElementById("hex-worlds");
if (fig) hexFigure(fig);
