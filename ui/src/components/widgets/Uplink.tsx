import { useEffect, useState } from "react";
import { motion, useReducedMotion, useTime, useTransform, type MotionValue } from "motion/react";

import { api, isTauri, type TargetRef, type UplinkInfo } from "../../lib/api";

/**
 * Where the machine is, and how to reach it.
 *
 * Three questions you otherwise answer by hand a dozen times a day: the LAN
 * address for a colleague on the same network, the public one for a firewall
 * rule, and the city — which is usually the explanation for latency you were not
 * expecting.
 *
 * The globe is drawn, not fetched. A map tile would be a network dependency, a
 * licence, and a photograph in an interface made of hairlines; a wireframe
 * sphere with a graticule is the same information in the design's own language,
 * and it costs one SVG.
 *
 * **It rotates, and the site rides it.** The marker keeps its real latitude and
 * longitude and turns with the sphere, sinking round the limb and coming back —
 * so its position is always the reading, never decoration. Nothing here animates
 * a *value*; rule 2 forbids inventing data, not showing a place.
 */
export function Uplink({ target }: { target: TargetRef }) {
  const [info, setInfo] = useState<UplinkInfo | null>(null);
  const [busy, setBusy] = useState(false);

  const load = (refresh = false) => {
    if (!isTauri()) return;
    setBusy(true);
    api
      .hostUplink(target, refresh)
      .then(setInfo)
      .catch(() => setInfo({}))
      .finally(() => setBusy(false));
  };

  // Once per host. Coordinates do not move, and the lookup leaves the target's
  // network — see `uplink.rs` for why that is deliberate and not polled.
  useEffect(() => {
    setInfo(null);
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target.host]);

  const place = [info?.city, info?.country].filter(Boolean).join(", ");

  return (
    <div className="flex flex-col gap-2">
      <Globe lat={info?.lat} long={info?.long} />

      {place && (
        <div className="flex items-baseline justify-between gap-2">
          <span className="micro">SITE</span>
          <span className="data truncate text-[11px]" style={{ color: "var(--text)" }} title={place}>
            {place}
          </span>
        </div>
      )}

      <Row label="LOCAL" value={info?.localIp} busy={busy} />
      <Row label="PUBLIC" value={info?.publicIp} busy={busy} />

      {info?.lat != null && info?.long != null && (
        <div className="flex items-baseline justify-between gap-2">
          <span className="micro">COORD</span>
          <span className="data text-[10.5px]" style={{ color: "var(--text-soft)" }}>
            {info.lat.toFixed(2)}, {info.long.toFixed(2)}
          </span>
        </div>
      )}

      <button
        type="button"
        className="chip self-start"
        disabled={busy}
        style={{ color: "var(--text-faint)" }}
        onClick={() => load(true)}
      >
        {busy ? "READING…" : "REFRESH"}
      </button>
    </div>
  );
}

function Row({ label, value, busy }: { label: string; value?: string; busy: boolean }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <span className="micro shrink-0">{label}</span>
      <span
        className="data truncate text-[11px]"
        style={{ color: value ? "var(--text)" : "var(--text-faint)" }}
        title={value}
      >
        {/* An em dash, not a zero or a guess. A host with no egress genuinely
            has no public address to report, and saying so is the answer. */}
        {value ?? (busy ? "…" : "—")}
      </span>
    </div>
  );
}

/** Meridian longitudes, in degrees, for the wireframe. */
const MERIDIANS = [-60, -30, 0, 30, 60];
/** Latitude circles, as a fraction of the radius. */
const PARALLELS = [-0.66, -0.33, 0, 0.33, 0.66];

/** One full turn, in milliseconds. */
const PERIOD = 48_000;

/**
 * A wireframe globe that actually turns.
 *
 * **It did not, and the reason is worth keeping.** This was a `<motion.g>`
 * animating `rotateY: 360`, which fails twice over: SVG children get no 3D
 * rendering context, so `rotateY` has nothing to project onto — and even where
 * it applied, a full 360° turn of a shape symmetric about its own axis is
 * indistinguishable from no turn at all. The comment above it described a
 * rotation nobody could see.
 *
 * A wireframe sphere spins by *projection*, not by transform. A meridian at
 * longitude λ, seen from outside, is an ellipse whose half-width is
 * `r·cos(λ)` — so advancing every λ by a shared phase and recomputing the width
 * is the rotation. That is what makes a flat drawing read as a sphere rather
 * than a dartboard, and it is why the parallels do not move: a latitude circle
 * is unchanged by rotation about the polar axis.
 *
 * Driven from `useTime` rather than a keyframe animation, because the site
 * marker has to stay on the same sphere — it needs the same phase to know both
 * where it is and whether it is facing us.
 */
function Globe({ lat, long }: { lat?: number; long?: number }) {
  const size = 108;
  const r = size / 2 - 4;
  const cx = size / 2;
  const cy = size / 2;

  const time = useTime();
  const reduced = useReducedMotion();
  // Modulo before scaling: `useTime` counts up for the life of the app, and a
  // phase built from an ever-growing float loses precision in a window left
  // open overnight.
  const phase = useTransform(time, (t) => (reduced ? 0 : ((t % PERIOD) / PERIOD) * Math.PI * 2));

  return (
    <div className="grid place-items-center py-1">
      <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} aria-hidden="true">
        {/* The limb. Everything else lives inside it. */}
        <circle cx={cx} cy={cy} r={r} fill="none" stroke="var(--border-strong)" strokeWidth="1" />

        {MERIDIANS.map((deg) => (
          <Meridian key={deg} deg={deg} phase={phase} cx={cx} cy={cy} r={r} />
        ))}

        {PARALLELS.map((f) => (
          <ellipse
            key={f}
            cx={cx}
            cy={cy + f * r}
            rx={r * Math.sqrt(Math.max(0, 1 - f * f))}
            ry={r * 0.12 * Math.sqrt(Math.max(0, 1 - f * f))}
            fill="none"
            stroke="var(--border)"
            strokeWidth="0.75"
          />
        ))}

        {lat != null && long != null && <Site lat={lat} long={long} phase={phase} cx={cx} cy={cy} r={r} />}
      </svg>
    </div>
  );
}

/**
 * One meridian, as the ellipse it projects to at the current phase.
 *
 * A component rather than a loop body so the hooks live at a component's top
 * level — `MERIDIANS` is a constant, but hooks inside a `.map` is a rule worth
 * not bending for a saving of six lines.
 */
function Meridian({
  deg,
  phase,
  cx,
  cy,
  r,
}: {
  deg: number;
  phase: MotionValue<number>;
  cx: number;
  cy: number;
  r: number;
}) {
  const radians = (deg * Math.PI) / 180;
  const rx = useTransform(phase, (p) => Math.abs(r * Math.cos(radians + p)));
  return (
    <motion.ellipse
      cx={cx}
      cy={cy}
      rx={rx}
      ry={r}
      fill="none"
      stroke="var(--border)"
      strokeWidth="0.75"
    />
  );
}

/**
 * Where the machine is, riding the sphere.
 *
 * It moves *with* the globe rather than sitting on top of it, and it fades
 * through the far side — a marker that stayed put while the wireframe turned
 * would read as a sticker on the glass rather than a place on a planet. The
 * facing test is the sign of `cos(λ + phase)`: positive is the hemisphere
 * turned toward us.
 */
function Site({
  lat,
  long,
  phase,
  cx,
  cy,
  r,
}: {
  lat: number;
  long: number;
  phase: MotionValue<number>;
  cx: number;
  cy: number;
  r: number;
}) {
  const radius = r * 0.92;
  const latitude = (lat * Math.PI) / 180;
  const longitude = (long * Math.PI) / 180;

  const x = useTransform(phase, (p) => cx + radius * Math.cos(latitude) * Math.sin(longitude + p));
  // Latitude is unaffected by the spin, so this is constant — but it is written
  // out rather than folded away, because the two coordinates belong together.
  const y = cy - radius * Math.sin(latitude);
  // Eased rather than clipped, so the point sinks round the limb instead of
  // blinking out. Rule 2: blinking is for cursors.
  const facing = useTransform(phase, (p) => {
    const z = Math.cos(longitude + p);
    return z <= 0 ? 0 : Math.min(1, z * 2.2);
  });
  const haloOpacity = useTransform(facing, (f) => f * 0.45);

  return (
    <>
      {/* Amber, not red — this is where the machine is, not something the
          operator must act on. */}
      <motion.circle cx={x} cy={y} r={2.6} fill="rgb(var(--busy))" style={{ opacity: facing }} />
      <motion.circle
        cx={x}
        cy={y}
        r={6}
        fill="none"
        stroke="rgb(var(--busy))"
        strokeWidth="0.75"
        style={{ opacity: haloOpacity }}
      />
    </>
  );
}
