import {
  groupSessions,
  groupKey,
  inheritedProfile,
  nextNameIn,
} from "./src/lib/session-groups";

/**
 * The rules the session rail's grouping has to keep.
 *
 * Pure logic, so this is a plain assertion list rather than a rendering probe.
 * Three of these are the ones worth having a test for at all — the rest would
 * be caught by looking at the rail once.
 */

type Fake = {
  id: string;
  target: { host?: string | null };
  folder: string;
  modelProfile?: string;
};

const s = (id: string, host: string | null, folder: string, modelProfile?: string): Fake => ({
  id,
  target: { host },
  folder,
  modelProfile,
});

export function run(log: (line: string) => void): boolean {
  let failures = 0;
  const check = (name: string, ok: boolean, detail = "") => {
    log(`  ${ok ? "PASS" : "FAIL"}  ${name}${detail && !ok ? ` — ${detail}` : ""}`);
    if (!ok) failures++;
  };

  // ── The one that matters most ────────────────────────────────────────────
  // `~/work/api` exists on every server anyone has set up. Merging by folder
  // alone would file work from two machines under one heading, and the `+` on
  // that heading would then create a session on whichever host sorted first.
  {
    const groups = groupSessions([
      s("a", "contabo2", "/home/x/api"),
      s("b", "csd2", "/home/x/api"),
    ]);
    check("same path on two hosts stays two groups", groups.length === 2, `got ${groups.length}`);
    check(
      "each keeps its own host",
      groups[0]?.host === "contabo2" && groups[1]?.host === "csd2",
    );
  }

  // A folder engineered to look like "host + delimiter + folder" must not
  // collide with a real group. Every printable delimiter is forgeable, because
  // a Unix filename may contain anything except `/` and NUL — so this walks the
  // characters someone would reach for first and asserts none of them work.
  //
  // Testing NUL *inside* a path would prove nothing: it is the one byte that
  // cannot get there, which is exactly why it is the separator. An earlier
  // version of this check did that and failed, demanding something impossible.
  {
    const forgeable = [" ", ":", "|", "\t", "\n", "-", "/", "."];
    const collisions = forgeable.filter(
      (d) => groupKey("a", `b${d}c`) === groupKey(`a${d}b`, "c"),
    );
    check(
      "no printable character can forge a key",
      collisions.length === 0,
      `forgeable with ${JSON.stringify(collisions)}`,
    );
    check("host and folder do not run together", groupKey("ab", "c") !== groupKey("a", "bc"));
  }

  // ── Order is spatial memory ──────────────────────────────────────────────
  // People reach for "the third one down". Sorting would move the target out
  // from under the click whenever a session started working or was renamed.
  {
    const groups = groupSessions([
      s("a", "zeta", "/z"),
      s("b", "alpha", "/a"),
      s("c", "zeta", "/z"),
    ]);
    check(
      "groups keep first-appearance order",
      groups.map((g) => g.host).join(",") === "zeta,alpha",
      groups.map((g) => g.host).join(","),
    );
    check(
      "sessions keep their order inside a group",
      groups[0]?.sessions.map((x) => x.id).join(",") === "a,c",
    );
    check("interleaved sessions still gather", groups[0]?.sessions.length === 2);
  }

  // ── A group must always be identifiable ──────────────────────────────────
  {
    const groups = groupSessions([s("a", null, "/"), s("b", null, "/srv/app/")]);
    check("root folder still gets a label", !!groups[0]?.label, "blank header");
    check("trailing slash still gets a label", !!groups[1]?.label, "blank header");
    check("a null host reads as local", groups[0]?.host === "local");
  }

  // ── Inheriting a provider ────────────────────────────────────────────────
  // Unanimous carries; disagreement is not something to guess at, because the
  // wrong guess routes a credential to a vendor nobody chose.
  {
    check(
      "unanimous profile is inherited",
      inheritedProfile([s("a", "h", "/p", "glm"), s("b", "h", "/p", "glm")]) === "glm",
    );
    check(
      "a split group inherits nothing",
      inheritedProfile([s("a", "h", "/p", "glm"), s("b", "h", "/p", "kimi")]) === undefined,
    );
    check(
      "unanimously unset stays unset",
      inheritedProfile([s("a", "h", "/p"), s("b", "h", "/p")]) === undefined,
    );
    check("an empty group inherits nothing", inheritedProfile([]) === undefined);
  }

  // ── Naming a session the rail can tell apart ─────────────────────────────
  // The folder basename is right for the first session on a project and
  // useless for the fourth; the header already says the folder.
  {
    const g = (names: string[]) => ({ label: "api", sessions: names.map((name) => ({ name })) });
    check("the first takes the folder name", nextNameIn(g([])) === "api");
    check("the second is numbered", nextNameIn(g(["api"])) === "api 2");
    check("it keeps counting", nextNameIn(g(["api", "api 2"])) === "api 3");
    // Closing the middle of a run must not produce a duplicate.
    check("a freed number is reused", nextNameIn(g(["api", "api 3"])) === "api 2");
    check("renamed siblings do not block it", nextNameIn(g(["deploy watcher"])) === "api");
  }

  log("");
  log(failures === 0 ? "ALL CHECKS PASSED" : `${failures} CHECK(S) FAILED`);
  return failures === 0;
}
