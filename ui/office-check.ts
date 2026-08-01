/**
 * Runs the office readers against real Office-produced fixtures, in a real
 * browser engine.
 *
 * Not a unit test: the readers depend on `DOMParser`, which Node does not have,
 * so testing them outside a browser would mean testing a different
 * implementation than the one that ships. This page is loaded by the dev server
 * and its results are read from the console.
 */
import { readDocument, readSlides, readSpreadsheet, sanitize } from "./src/lib/office";

const results: string[] = [];
let failures = 0;

function check(name: string, condition: boolean, detail?: unknown) {
  results.push(`${condition ? "PASS" : "FAIL"}  ${name}${condition ? "" : ` — got ${JSON.stringify(detail)}`}`);
  if (!condition) failures += 1;
}

const load = async (name: string) =>
  new Uint8Array(await (await fetch(`/__fixtures/${name}`)).arrayBuffer());

async function run() {
  // --- spreadsheet --------------------------------------------------------
  try {
    const sheets = await readSpreadsheet(await load("report.xlsx"));
    check("xlsx: both sheets found", sheets.length === 2, sheets.map((s) => s.name));
    check("xlsx: sheet names", sheets[0]?.name === "Metrics" && sheets[1]?.name === "Notes", sheets.map((s) => s.name));
    check("xlsx: header row", JSON.stringify(sheets[0]?.rows[0]) === '["host","cpu","ram"]', sheets[0]?.rows[0]);
    check("xlsx: numbers kept", JSON.stringify(sheets[0]?.rows[1]) === '["singapore","41.5","8192"]', sheets[0]?.rows[1]);
    // The row with no B cell: "4096" must stay in column C, not slide into B.
    check("xlsx: column gap preserved", JSON.stringify(sheets[0]?.rows[2]) === '["tokyo","","4096"]', sheets[0]?.rows[2]);
    check("xlsx: repeated string resolved", sheets[0]?.rows[3]?.[0] === "singapore", sheets[0]?.rows[3]);
    check("xlsx: second sheet content", sheets[1]?.rows[0]?.[0] === "second sheet", sheets[1]?.rows[0]);
  } catch (e) {
    check("xlsx: parsed without throwing", false, String(e));
  }

  // --- document -----------------------------------------------------------
  try {
    const html = await readDocument(await load("notes.docx"));
    check("docx: heading present", /<h1>Quarterly Report<\/h1>/.test(html), html.slice(0, 200));
    check("docx: bold run kept", /<strong>bold text<\/strong>/.test(html), html.slice(0, 400));
    check("docx: list rendered", /<ul>[\s\S]*First bullet[\s\S]*Second bullet[\s\S]*<\/ul>/.test(html), html.slice(0, 600));
    check("docx: table rendered", /<table>[\s\S]*uptime[\s\S]*99\.9%[\s\S]*<\/table>/.test(html), html.slice(0, 900));
    check("docx: no script survived sanitising", !/<script|onerror=|onload=/i.test(html));
  } catch (e) {
    check("docx: parsed without throwing", false, String(e));
  }

  // --- slides -------------------------------------------------------------
  try {
    const slides = await readSlides(await load("deck.pptx"));
    check("pptx: both slides found", slides.length === 2, slides.length);
    check("pptx: titles in order", slides[0]?.title === "Slide One Title" && slides[1]?.title === "Slide Two Title", slides.map((s) => s.title));
    check("pptx: body lines", JSON.stringify(slides[0]?.lines) === '["first bullet","second bullet"]', slides[0]?.lines);
    check("pptx: second slide body", JSON.stringify(slides[1]?.lines) === '["only line"]', slides[1]?.lines);
  } catch (e) {
    check("pptx: parsed without throwing", false, String(e));
  }

  // --- sanitiser, against input a converter would never emit ---------------
  const hostile: [string, string, (out: string) => boolean][] = [
    ["strips script tags and their source", "<p>ok</p><script>fetch('/steal')</script>", (o) => !/script|steal|fetch/i.test(o) && o.includes("ok")],
    ["strips style blocks and their rules", "<p>ok</p><style>body{display:none}</style>", (o) => !/style|display:none/i.test(o) && o.includes("ok")],
    ["strips inline handlers", '<img src="data:image/png;base64,AA" onerror="alert(1)">', (o) => !/onerror/i.test(o)],
    ["strips javascript: hrefs", '<a href="javascript:alert(1)">x</a>', (o) => !/javascript:/i.test(o)],
    ["strips data:text/html hrefs", '<a href="data:text/html,<script>1</script>">x</a>', (o) => !/data:text\/html/i.test(o)],
    ["strips style attributes", '<p style="position:fixed;inset:0">x</p>', (o) => !/style=/i.test(o)],
    ["strips iframes but keeps text", "<iframe src=\"https://evil\">inner</iframe>", (o) => !/iframe/i.test(o) && o.includes("inner")],
    ["keeps http links", '<a href="https://example.com">x</a>', (o) => o.includes('href="https://example.com"')],
    ["keeps embedded images", '<img src="data:image/png;base64,AAAA" alt="a">', (o) => o.includes("data:image/png")],
    ["keeps formatting", "<p><strong>b</strong><em>i</em></p>", (o) => o.includes("<strong>b</strong>") && o.includes("<em>i</em>")],
    ["unwraps unknown tags, keeps prose", "<marquee>important prose</marquee>", (o) => !/marquee/i.test(o) && o.includes("important prose")],
  ];
  for (const [name, input, ok] of hostile) {
    let out = "";
    try {
      out = sanitize(input);
      check(`sanitise: ${name}`, ok(out), out);
    } catch (e) {
      check(`sanitise: ${name}`, false, String(e));
    }
  }

  const summary = `[office-check] ${failures === 0 ? "ALL PASS" : `${failures} FAILED`}\n${results.join("\n")}`;
  document.getElementById("out")!.textContent = summary;
  console.log(summary);
}

void run();
