/**
 * Does the backdrop colour the app's own chrome unevenly?
 *
 * ## The bug this catches
 *
 * Reported as "big color differences from sidebar and main panel", and two
 * rounds of reading the leaf classes found nothing — because there was nothing
 * there to find. Every `.panel` computes the same background, identical to the
 * byte. The difference came from the layer *behind* them: `.atmosphere::after`
 * carried a vignette (`transparent` at 50%/42% → `rgba(6,6,6,0.14)` at the edge)
 * and a red bloom centred at 22%/24%.
 *
 * The rails are the edge. Panels are translucent. So a gradient across the
 * window printed itself onto the furniture standing on it — the left rail
 * measured twenty levels darker than the session deck, and its top was
 * additionally red-cast.
 *
 * ## Why it samples pixels instead of reading the CSS
 *
 * A string check would pass the exact output that is wrong. `background:` could
 * name any number of gradients that happen to cancel, or none that happen not
 * to; what matters is the colour that lands behind a panel. So this renders the
 * real rule with the browser's own renderer and reads the pixels back.
 *
 * White, not a dark backdrop, for the reason `xterm-glass-check` uses stripes:
 * a subtle field hides exactly the failure being looked for.
 */

/** How far apart two sample points may read before it is visible as a seam. */
const TOLERANCE = 3;

type Sample = { label: string; x: number; y: number; rgb: [number, number, number] };

async function paintAtmosphere(w: number, h: number): Promise<CanvasRenderingContext2D> {
  // The rule under test, lifted from `signal-room.css`. Rendered through an SVG
  // foreignObject so it is the browser resolving the gradients, not us.
  const after = getComputedStyle(
    document.querySelector(".atmosphere") as Element,
    "::after",
  ).background;

  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}">
    <foreignObject width="100%" height="100%">
      <div xmlns="http://www.w3.org/1999/xhtml"
           style="position:absolute;inset:0;background:#fff">
        <div style="position:absolute;inset:0;background:${after.replace(/"/g, "'")}"></div>
      </div>
    </foreignObject></svg>`;

  const img = new Image();
  await new Promise<void>((resolve, reject) => {
    img.onload = () => resolve();
    img.onerror = () => reject(new Error("could not rasterise the atmosphere"));
    img.src = "data:image/svg+xml;charset=utf-8," + encodeURIComponent(svg);
  });

  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("no 2d context");
  ctx.drawImage(img, 0, 0);
  return ctx;
}

export async function run(log: (line: string) => void): Promise<boolean> {
  const w = 1791;
  const h = 1056;
  const ctx = await paintAtmosphere(w, h);

  // Sample away from the registration dots, which are meant to be there. The
  // grid repeats every 26px, so a point offset from the lattice reads the field.
  const at = (label: string, x: number, y: number): Sample => {
    const d = ctx.getImageData(Math.round(x), Math.round(y), 1, 1).data;
    return { label, x, y, rgb: [d[0] ?? 0, d[1] ?? 0, d[2] ?? 0] };
  };

  const samples: Sample[] = [
    at("left rail · middle", 108, h / 2 + 7),
    at("left rail · top", 108, 253),
    at("session deck · middle", 880, h / 2 + 7),
    at("session deck · top", 880, 253),
    at("right rail · middle", w - 122, h / 2 + 7),
  ];

  for (const s of samples) log(`  ${s.label.padEnd(24)} rgb(${s.rgb.join(", ")})`);

  const levels = samples.flatMap((s) => s.rgb);
  const spread = Math.max(...levels) - Math.min(...levels);

  // The red bloom showed up as a *channel* split, not a brightness one — the
  // rail read 231,227,227 while the deck read a neutral 247. Checking overall
  // spread alone would miss a tint that happened to be equally bright.
  const worstCast = Math.max(
    ...samples.map((s) => Math.max(...s.rgb) - Math.min(...s.rgb)),
  );

  log("");
  log(`  brightness spread across surfaces : ${spread} (max ${TOLERANCE})`);
  log(`  worst per-sample colour cast      : ${worstCast} (max ${TOLERANCE})`);

  const flat = spread <= TOLERANCE;
  const neutral = worstCast <= TOLERANCE;

  log("");
  log(flat ? "  PASS  the backdrop is flat — rails and deck sit on the same field" : "  FAIL  the backdrop darkens one part of the app more than another");
  log(neutral ? "  PASS  no surface is tinted relative to another" : "  FAIL  a surface carries a colour cast the others do not");

  return flat && neutral;
}
