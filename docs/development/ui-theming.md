# UI theming

LogScope's desktop UI has one stylesheet, `apps/desktop/src/styles.css`, and no
CSS framework, theme provider, or CSS-in-JS. Components carry class names only —
there are no colour literals in any `.tsx` file, and the handful of inline
`style` attributes set geometry (row offsets, virtual-list heights) rather than
colour. All colour decisions therefore live in that single file.

## The rule

**A rule that sets a background must also establish a foreground.**

This is not a style preference. `styles.css` declares `color-scheme: light dark`,
so when a rule paints a background but omits `color`, the text falls back to the
*user agent's* foreground — which follows the reader's OS theme, not ours. A
panel hardcoded to a dark background then renders black-on-black for anyone whose
OS is in light mode, while the developer who wrote it (OS in dark mode) sees
white-on-black and notices nothing.

That is exactly the defect this token system was introduced to remove: 53 of 131
rendered elements in the Investigation screen failed WCAG AA on a light-mode OS,
with log row text measuring 1.14:1 (`rgb(0,0,0)` on `rgb(16,20,28)`).

## Tokens

`:root` carries the light palette; a `prefers-color-scheme: dark` block overrides
it. Every token is defined in both. Nothing outside those two blocks may contain
a colour literal.

| Group | Tokens |
| --- | --- |
| Surfaces | `--surface`, `--surface-raised`, `--surface-sunken`, `--surface-header`, `--surface-overlay`, `--surface-inset`, `--surface-control`, `--surface-control-active`, `--surface-hover`, `--surface-selected`, `--scrim` |
| Text | `--text-primary`, `--text-secondary`, `--text-muted`, `--text-disabled`, `--text-inverse`, `--text-key` |
| Borders | `--border`, `--border-subtle`, `--border-strong`, `--border-control` |
| Interactive | `--link`, `--focus`, `--accent`, `--accent-soft` |
| Severity | `--sev-{error,warn,debug}-fg`, `--sev-{error,warn,debug,info}-edge` |
| Feedback | `--danger-{bg,border,fg}`, `--brush-{bg,border}` |
| Query syntax | `--hl-{field,keyword,string,regex,operator,paren,term,squiggle}` |
| Chips | `--chip-{neutral,blue,amber,green,purple,red}-{bg,fg}`, always used as a pair |

Two conventions worth knowing:

- **Muting uses a colour token, never `opacity`.** Opacity multiplies against
  whatever contrast the text already has, so it degrades an already-marginal
  pair and cannot be reasoned about locally. `.dim` (155 uses), `.subtitle`,
  `.status`, and archived cards all resolve to `--text-muted` instead.
- **`--border-control` is separate from `--border`** because interactive
  boundaries are held to WCAG 1.4.11 (~3:1) while panel dividers are meant to
  stay subtle.

## The checker

```bash
npm run check:contrast
```

`apps/desktop/scripts/check-contrast.mjs` (no dependencies, runs in CI before the
frontend build) parses the stylesheet and enforces three invariants:

1. **Structure** — no colour literal outside a custom-property definition, and
   every `var(--x)` is defined in *both* palettes.
2. **Pairing** — every rule that sets a background either sets `color` or is
   listed in `INHERITS_FOREGROUND` with the ancestor it inherits from.
3. **Contrast** — every foreground/background combination the UI actually
   renders meets its WCAG 2.1 target, in both palettes. Translucent surfaces are
   composited over the layer beneath before the ratio is taken.

Targets: 4.5:1 normal text, 3:1 large text and non-text UI (focus rings, control
borders, histogram bars), 3:1 floor for disabled text (exempt from 1.4.3 but it
still has to be legible).

## Adding a surface

1. Add the background *and* its foreground token to both palettes.
2. Use them together in the rule.
3. Add the pair to `PAIRS` in the checker, with the UI element it describes.
4. Run `npm run check:contrast`.

If a rule legitimately inherits its foreground (a decorative bar with no text, a
scrim, a `:hover` variant of a rule that already sets `color`), add it to
`INHERITS_FOREGROUND` with a note naming where the colour comes from, rather
than silencing the check.
