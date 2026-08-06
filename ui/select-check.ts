/**
 * Can the operator actually select the text in a file preview?
 *
 * Open http://localhost:5273/select-check.html and read the console.
 *
 * `getComputedStyle` on the *container* is not the check. `user-select: none`
 * inherits, so a container that opts back in still leaves every heading and
 * paragraph inside it refusing — which is exactly what the preview did. The
 * assertion therefore reads the computed value on a **descendant**, and then
 * makes a real selection and reads the text back, because a property that
 * looks right and a selection that works are different claims.
 */
let failures = 0;
function check(what: string, ok: boolean) {
  if (ok) console.log(`%c PASS %c ${what}`, "background:#2b7;color:#000", "");
  else {
    failures += 1;
    console.error(`FAIL  ${what}`);
  }
}

const selectOf = (el: Element) => getComputedStyle(el).webkitUserSelect || getComputedStyle(el).userSelect;

const plain = document.querySelector("#plain")!;
const opted = document.querySelector("#opted")!;

// The shell is an application, so this is the correct default and must stay.
check("an unmarked surface still refuses selection", selectOf(plain.querySelector("p")!) === "none");

check("a .selectable container opts in", selectOf(opted) === "text");
// The one that matters: inheritance would leave the children at `none`.
check("its descendants opt in too", selectOf(opted.querySelector("p")!) === "text");
check("headings as well", selectOf(opted.querySelector("h1")!) === "text");

// Property values are a claim; a selection is the thing itself.
const range = document.createRange();
range.selectNodeContents(opted.querySelector("p")!);
const sel = window.getSelection()!;
sel.removeAllRanges();
sel.addRange(range);
check("text can actually be selected", sel.toString().includes("selectable preview"));
sel.removeAllRanges();

console.log(
  failures ? `%c ${failures} FAILED ` : "%c ALL PASS ",
  failures ? "background:#e63b2e;color:#fff" : "background:#2b7;color:#000",
);
