import { loadCached, importIso, forget } from "./disc.js";
import { loadEngine } from "./engine.js";

const $ = (id) => document.getElementById(id);
const log = (msg) => ($("log").textContent += msg + "\n");

async function boot(files) {
  $("boot").hidden = true;
  $("stage").hidden = false;
  const engine = await loadEngine();
  const worlds = new Uint8Array(await files.worlds.arrayBuffer());
  log(`worlds: ${(worlds.length / 1e6).toFixed(1)} MB, ${engine.worldsSectionCount(worlds)} sections`);
}

async function onIso(file) {
  $("drop").hidden = true;
  $("progress").hidden = false;
  const bar = $("progress").querySelector("i");
  const label = $("progress").querySelector(".progress__label");
  try {
    const files = await importIso(file, (p, name) => {
      bar.style.transform = `scaleX(${p})`;
      label.textContent = `Copied ${name}`;
    });
    await boot(files);
  } catch (e) {
    label.textContent = e.message;
    $("drop").hidden = false;
  }
}

$("iso").addEventListener("change", (e) => e.target.files[0] && onIso(e.target.files[0]));
const drop = $("drop");
drop.addEventListener("dragover", (e) => (e.preventDefault(), drop.classList.add("is-over")));
drop.addEventListener("dragleave", () => drop.classList.remove("is-over"));
drop.addEventListener("drop", (e) => {
  e.preventDefault();
  drop.classList.remove("is-over");
  const f = e.dataTransfer.files[0];
  if (f) onIso(f);
});
$("forget").addEventListener("click", async () => (await forget(), location.reload()));

const cached = await loadCached();
if (cached) {
  $("forget").hidden = false;
  boot(cached);
}
