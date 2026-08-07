/**
 * Theme contract checker for apps/desktop/src/styles.css.
 *
 * This exists because the class of bug it guards against is invisible in a
 * build and invisible in a screenshot taken on the author's machine: a rule
 * sets a dark `background` and omits `color`, so the text falls back to the
 * user agent's foreground, which follows the *reader's* OS theme. On a
 * light-mode OS that renders near-black text on a near-black surface.
 *
 * Three invariants, checked against the stylesheet itself:
 *
 *   1. STRUCTURE  - no colour literal may appear outside a custom-property
 *                   definition, and every `var(--x)` must be defined in both
 *                   the light and the dark palette.
 *   2. PAIRING    - every rule that sets a background must also establish a
 *                   foreground (its own `color`, or one inherited from an
 *                   ancestor rule listed in INHERITS_FOREGROUND).
 *   3. CONTRAST   - every foreground/background pair the UI actually renders
 *                   meets its WCAG 2.1 target in both palettes.
 *
 * Run: npm run check:contrast
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const CSS_PATH = join(here, "..", "src", "styles.css");
const css = readFileSync(CSS_PATH, "utf8");

/* ------------------------------------------------------------------ colour */

/** Parse `#rgb`, `#rrggbb`, `rgb()`, `rgba()` into [r, g, b, a]. */
function parseColor(raw) {
  const v = raw.trim();
  let m = /^#([0-9a-f]{3})$/i.exec(v);
  if (m) {
    const [r, g, b] = [...m[1]].map((c) => parseInt(c + c, 16));
    return [r, g, b, 1];
  }
  m = /^#([0-9a-f]{6})$/i.exec(v);
  if (m) {
    const n = parseInt(m[1], 16);
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255, 1];
  }
  m = /^rgba?\(([^)]+)\)$/i.exec(v);
  if (m) {
    const p = m[1].split(/[,/]/).map((s) => parseFloat(s.trim()));
    return [p[0], p[1], p[2], p.length > 3 && !Number.isNaN(p[3]) ? p[3] : 1];
  }
  throw new Error(`cannot parse colour: ${raw}`);
}

/** Composite a possibly-translucent colour over an opaque backdrop. */
function over([r, g, b, a], backdrop) {
  if (a >= 1) return [r, g, b, 1];
  const [br, bg, bb] = backdrop;
  return [
    r * a + br * (1 - a),
    g * a + bg * (1 - a),
    b * a + bb * (1 - a),
    1,
  ];
}

function relativeLuminance([r, g, b]) {
  const f = (c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

function contrast(fg, bg) {
  const a = relativeLuminance(fg);
  const b = relativeLuminance(bg);
  const [hi, lo] = a > b ? [a, b] : [b, a];
  return (hi + 0.05) / (lo + 0.05);
}

/* ------------------------------------------------------------- css parsing */

/** Split the sheet into `{ selector, body }` rules (flattening @media). */
function rules(source) {
  const out = [];
  const re = /([^{}]+)\{([^{}]*)\}/g;
  let m;
  while ((m = re.exec(source))) {
    const selector = m[1].trim().replace(/\s+/g, " ");
    if (selector.startsWith("@")) continue; // @media wrapper; its inner rules match separately
    out.push({ selector, body: m[2] });
  }
  return out;
}

/** Declarations of one rule as `[property, value]`, comments stripped. */
function declarations(body) {
  return body
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split(";")
    .map((d) => d.trim())
    .filter(Boolean)
    .map((d) => {
      const i = d.indexOf(":");
      return i < 0 ? null : [d.slice(0, i).trim(), d.slice(i + 1).trim()];
    })
    .filter(Boolean);
}

const withoutComments = css.replace(/\/\*[\s\S]*?\*\//g, "");

/** Custom properties per palette. The dark palette is the one inside @media. */
const DARK_BLOCK_RE = /@media \(prefers-color-scheme: dark\) \{\s*:root \{([\s\S]*?)\}\s*\}/;
const darkBlock = DARK_BLOCK_RE.exec(withoutComments);
if (!darkBlock) fail("could not locate the dark palette block");

/* The dark palette must be excised before parsing rules: `rules()` flattens
   @media, so an in-media `:root` would otherwise be collected as a light
   `:root` and silently overwrite the light palette with dark values. */
const lightSource = withoutComments.replace(DARK_BLOCK_RE, "");
const allRules = rules(lightSource);

function tokensFrom(bodies) {
  const t = {};
  for (const body of bodies) {
    for (const [prop, value] of declarations(body)) {
      if (prop.startsWith("--")) t[prop] = value;
    }
  }
  return t;
}

const rootBodies = allRules.filter((r) => r.selector === ":root").map((r) => r.body);
const lightTokens = tokensFrom(rootBodies);
const darkTokens = { ...lightTokens, ...tokensFrom([darkBlock[1]]) };

/** Resolve a token to a colour, following `var()` chains. */
function resolve(name, tokens, seen = new Set()) {
  if (seen.has(name)) throw new Error(`circular token: ${name}`);
  seen.add(name);
  const raw = tokens[name];
  if (raw === undefined) throw new Error(`undefined token: ${name}`);
  const v = raw.trim();
  const m = /^var\((--[\w-]+)\)$/.exec(v);
  if (m) return resolve(m[1], tokens, seen);
  return parseColor(v);
}

/* -------------------------------------------------------------- the checks */

const problems = [];
function fail(msg) {
  problems.push(msg);
}

/* 1. STRUCTURE ------------------------------------------------------------ */

const COLOR_LITERAL =
  /#[0-9a-fA-F]{3,8}\b|\brgba?\(|\bhsla?\(|\b(?:white|black|red|blue|green|gray|grey|silver|navy|teal|olive|maroon|purple|fuchsia|lime|aqua|yellow|orange)\b/;
const COLOR_PROPS =
  /^(?:color|background|background-color|border|border-color|border-[a-z]+-color|border-[a-z]+|outline|outline-color|caret-color|accent-color|scrollbar-color|fill|stroke|box-shadow|text-decoration|text-decoration-color)$/;

for (const { selector, body } of allRules) {
  for (const [prop, value] of declarations(body)) {
    if (prop.startsWith("--")) continue; // token definitions are the one place literals belong
    // `var(--chip-red-bg)` is a token reference, not a literal named colour.
    const literalPart = value.replace(/var\(\s*--[\w-]+\s*\)/g, "");
    if (!COLOR_LITERAL.test(literalPart)) continue;
    if (!COLOR_PROPS.test(prop) && !/color/.test(prop)) continue;
    fail(
      `STRUCTURE: \`${selector}\` sets \`${prop}: ${value}\` with a colour literal; use a token instead`,
    );
  }
}

const referenced = new Set();
for (const m of withoutComments.matchAll(/var\((--[\w-]+)\)/g)) referenced.add(m[1]);
for (const name of [...referenced].sort()) {
  if (!(name in lightTokens)) fail(`STRUCTURE: ${name} is used but missing from the light palette`);
  else if (!(name in darkTokens)) fail(`STRUCTURE: ${name} is used but missing from the dark palette`);
}

/* 2. PAIRING -------------------------------------------------------------- */

/* Selectors whose foreground legitimately comes from an ancestor rule that
   already establishes one. Each entry names where the colour comes from. */
const INHERITS_FOREGROUND = {
  ".histogram-bar": "decorative bar, no text",
  ".histogram-brush": "decorative overlay, no text",
  ".modal": "scrim only; .modal-body / .modal-overlay .modal set the pair",
  ".modal-overlay": "scrim only; .modal-overlay .modal sets the pair",
  ".table-row:hover": ".table-row sets color",
  ".table-row.selected": "sets its own color",
  "tbody tr:hover": "table cells inherit body/panel foreground",
  ".context-list .context-anchor": "inherits .event-table / panel foreground",
  ".case-list .link.selected": "sets its own color",
  "button:not(.link):not(:disabled):hover": "button rule sets color",
  ".side-tabs button.active": "sets its own color",
};

for (const { selector, body } of allRules) {
  if (selector === ":root" || selector === "body") continue;
  const decls = declarations(body);
  const setsBg = decls.some(
    ([p, v]) =>
      (p === "background" || p === "background-color") &&
      v !== "none" &&
      v !== "transparent" &&
      !/^transparent/.test(v),
  );
  if (!setsBg) continue;
  const setsFg = decls.some(([p]) => p === "color");
  if (setsFg) continue;
  if (selector in INHERITS_FOREGROUND) continue;
  fail(
    `PAIRING: \`${selector}\` sets a background but no color, so its text falls back to the user agent foreground`,
  );
}

/* 3. CONTRAST ------------------------------------------------------------- */

const AA_TEXT = 4.5; // WCAG 1.4.3 normal text
const AA_LARGE = 3.0; // WCAG 1.4.3 large text
const AA_NONTEXT = 3.0; // WCAG 1.4.11 UI components / focus indicators
const DISABLED_MIN = 3.0; // exempt from 1.4.3, but must stay legible

/**
 * Every foreground/background combination the UI actually renders.
 * `on` may be a chain: a translucent surface is composited over the one
 * beneath it before the ratio is taken.
 */
const PAIRS = [
  // -- application background -------------------------------------------
  ["--text-primary", ["--surface"], AA_TEXT, "body text"],
  ["--text-secondary", ["--surface"], AA_TEXT, ".subtitle / .status / .jobline / labels"],
  ["--text-muted", ["--surface"], AA_TEXT, ".dim (155 uses)"],
  ["--text-disabled", ["--surface"], DISABLED_MIN, "disabled link buttons"],
  ["--link", ["--surface"], AA_TEXT, "button.link"],
  ["--text-key", ["--surface"], AA_TEXT, ".kv .k field names in detail panel"],
  ["--sev-error-fg", ["--surface"], AA_TEXT, ".diag-error / .error-inline"],
  ["--sev-warn-fg", ["--surface"], AA_TEXT, ".diag-warn"],
  ["--focus", ["--surface"], AA_NONTEXT, "focus ring"],
  ["--border-control", ["--surface"], AA_NONTEXT, "input/select/button outline"],
  ["--border-strong", ["--surface"], 2.0, "editor + modal border, must stay visible"],

  // -- query editor (highlight layer paints on the app surface) ----------
  ["--hl-field", ["--surface"], AA_TEXT, "query field token"],
  ["--hl-keyword", ["--surface"], AA_TEXT, "query keyword token"],
  ["--hl-string", ["--surface"], AA_TEXT, "query string token"],
  ["--hl-regex", ["--surface"], AA_TEXT, "query regex token"],
  ["--hl-operator", ["--surface"], AA_TEXT, "query operator token"],
  ["--hl-paren", ["--surface"], AA_TEXT, "query paren token"],
  ["--hl-term", ["--surface"], AA_TEXT, "query bare term"],
  ["--hl-squiggle", ["--surface"], AA_NONTEXT, "query error underline"],

  // -- timeline / histogram ---------------------------------------------
  ["--text-primary", ["--surface-raised"], AA_TEXT, "histogram text"],
  ["--text-muted", ["--surface-raised"], AA_TEXT, ".histogram-empty status text"],
  ["--text-secondary", ["--surface-raised"], AA_TEXT, "histogram meta"],
  ["--accent", ["--surface-raised"], AA_NONTEXT, ".histogram-bar"],
  ["--brush-border", ["--surface-raised"], AA_NONTEXT, ".histogram-brush edge"],

  // -- log results table -------------------------------------------------
  ["--text-primary", ["--surface-sunken"], AA_TEXT, "log row text incl. the em-dash marker"],
  ["--text-muted", ["--surface-sunken"], AA_TEXT, ".table-empty / loading / idle states"],
  ["--sev-error-fg", ["--surface-sunken"], AA_TEXT, "ERROR severity cell"],
  ["--sev-warn-fg", ["--surface-sunken"], AA_TEXT, "WARN severity cell"],
  ["--sev-debug-fg", ["--surface-sunken"], AA_TEXT, "DEBUG/TRACE row text"],
  ["--text-secondary", ["--surface-header"], AA_TEXT, ".table-header column headers"],
  ["--focus", ["--surface-sunken"], AA_NONTEXT, ".event-table focus ring"],

  // -- hovered row -------------------------------------------------------
  ["--text-primary", ["--surface-hover"], AA_TEXT, "hovered row text"],
  ["--sev-debug-fg", ["--surface-hover"], AA_TEXT, "hovered DEBUG row"],
  ["--text-muted", ["--surface-hover"], AA_TEXT, "hovered muted text"],

  // -- selected row ------------------------------------------------------
  ["--text-primary", ["--surface-selected"], AA_TEXT, "selected row text"],
  ["--sev-error-fg", ["--surface-selected"], AA_TEXT, "selected ERROR row"],
  ["--sev-warn-fg", ["--surface-selected"], AA_TEXT, "selected WARN row"],
  ["--sev-debug-fg", ["--surface-selected"], AA_TEXT, "selected DEBUG row"],
  ["--text-muted", ["--surface-selected"], AA_TEXT, "selected row metadata"],

  // -- facet sidebar / tabs / controls -----------------------------------
  ["--text-secondary", ["--surface-control"], AA_TEXT, ".side-tabs button"],
  ["--text-primary", ["--surface-control"], AA_TEXT, "input/select/button text"],
  ["--text-muted", ["--surface-control"], AA_TEXT, "::placeholder"],
  ["--text-primary", ["--surface-control-active"], AA_TEXT, "active tab"],
  ["--focus", ["--surface-control-active"], AA_NONTEXT, "active tab border"],
  ["--text-disabled", ["--surface-raised"], DISABLED_MIN, "disabled control text"],

  // -- record detail panel, modals, snapshots ----------------------------
  ["--text-primary", ["--surface-overlay"], AA_TEXT, "modal body"],
  ["--text-secondary", ["--surface-overlay"], AA_TEXT, "modal labels"],
  ["--text-muted", ["--surface-overlay"], AA_TEXT, "modal metadata"],
  ["--text-primary", ["--surface-inset"], AA_TEXT, ".snapshot / .pin-summary"],
  ["--text-muted", ["--surface-inset"], AA_TEXT, ".snapshot metadata"],
  ["--text-key", ["--surface-overlay"], AA_TEXT, ".kv .k inside a modal"],

  // -- error block (translucent tint over the page) ----------------------
  ["--danger-fg", ["--danger-bg", "--surface"], AA_TEXT, ".error banner"],

  // -- chips / badges ----------------------------------------------------
  ["--chip-neutral-fg", ["--chip-neutral-bg"], AA_TEXT, ".badge / neutral chips"],
  ["--chip-blue-fg", ["--chip-blue-bg"], AA_TEXT, "status-open / kind-task chips"],
  ["--chip-amber-fg", ["--chip-amber-bg"], AA_TEXT, "badge-warn / kind-finding chips"],
  ["--chip-green-fg", ["--chip-green-bg"], AA_TEXT, "save-saved / status-resolved chips"],
  ["--chip-purple-fg", ["--chip-purple-bg"], AA_TEXT, "kind-question chips"],
  ["--chip-red-fg", ["--chip-red-bg"], AA_TEXT, "badge-error / save-failed chips"],

  // chips sit on panels, so their tint must not vanish into the surface
  ["--chip-neutral-bg", ["--surface"], 1.05, "neutral chip must remain distinguishable"],

  // -- resolver states ---------------------------------------------------
  ["--chip-green-fg", ["--surface"], AA_TEXT, ".rs-verified"],
];

const palettes = [
  ["light", lightTokens],
  ["dark", darkTokens],
];

const report = [];
for (const [mode, tokens] of palettes) {
  for (const [fgName, bgChain, min, what] of PAIRS) {
    let fg;
    let bg;
    try {
      fg = resolve(fgName, tokens);
      const chain = bgChain.map((n) => resolve(n, tokens));
      bg = chain.reduceRight((backdrop, layer) => over(layer, backdrop));
      if (bg[3] < 1) throw new Error(`background chain ${bgChain.join(" over ")} is translucent`);
      fg = over(fg, bg);
    } catch (e) {
      fail(`CONTRAST[${mode}]: ${fgName} on ${bgChain.join(" over ")} - ${e.message}`);
      continue;
    }
    const ratio = contrast(fg, bg);
    const ok = ratio >= min;
    report.push({ mode, fgName, bgChain, ratio, min, ok, what });
    if (!ok) {
      fail(
        `CONTRAST[${mode}]: ${fgName} on ${bgChain.join(" over ")} = ${ratio.toFixed(2)}:1, need ${min}:1 (${what})`,
      );
    }
  }
}

/* ----------------------------------------------------------------- output */

const verbose = process.argv.includes("--verbose");
if (verbose) {
  for (const [mode] of palettes) {
    console.log(`\n${mode} palette`);
    for (const r of report.filter((x) => x.mode === mode)) {
      console.log(
        `  ${r.ok ? "ok  " : "FAIL"} ${r.ratio.toFixed(2).padStart(6)}:1 (min ${String(r.min).padEnd(4)}) ` +
          `${r.fgName} on ${r.bgChain.join(" over ")}  - ${r.what}`,
      );
    }
  }
}

if (problems.length > 0) {
  console.error(`\ncheck-contrast: ${problems.length} problem(s)\n`);
  for (const p of problems) console.error("  - " + p);
  console.error("");
  process.exit(1);
}

const worst = report.reduce((a, b) => (a.ratio / a.min < b.ratio / b.min ? a : b));
console.log(
  `check-contrast: ${report.length} foreground/background pairs verified across ` +
    `${palettes.length} palettes, ${Object.keys(lightTokens).length} tokens. ` +
    `Tightest: ${worst.fgName} on ${worst.bgChain.join(" over ")} ` +
    `= ${worst.ratio.toFixed(2)}:1 (min ${worst.min}) in ${worst.mode}.`,
);
