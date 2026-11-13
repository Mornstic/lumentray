# Bundled fonts

LumenTray ships three fonts so the type system holds on machines without
HarmonyOS Sans SC installed. HarmonyOS Sans SC itself is **not** bundled; it is
used first when the system provides it.

All bundled fonts are licensed under the SIL Open Font License 1.1. License
texts sit next to the font files.

## Inter (variable, Latin)

- File: `Inter-latin-var.woff2`
- Version: 4.001, `wght` 100–900, Latin subset (518 glyphs)
- Source: Google Fonts CSS API, `family=Inter:wght@100..900`; retrieved 2026-11-12
- License: `OFL-Inter.txt`
- Role: Latin letters, digits and punctuation; first family for both
  `--font-text` and `--font-display`

## Noto Sans SC (variable, subset)

- File: `NotoSansSC-subset.woff2`
- Version: 2.004-H2, `wght` 100–900, 596 characters
- Source: full Noto Sans SC variable TTF (`notofonts/noto-cjk`, distributed via
  Google Fonts); retrieved 2026-11-12. Subset generated locally with fontTools
  `pyftsubset`, keeping every character used by the UI plus the full
  U+3000–303F and U+FF00–FFEF ranges.
- License: `OFL-NotoSansSC.txt`
- Role: Chinese fallback after HarmonyOS Sans SC

## Newsreader (variable, digits subset)

- File: `Newsreader-num-var.woff2`
- Axes: `wght` 200–800, `opsz` 6–72
- Source: `google/fonts` repository, `ofl/newsreader/Newsreader[opsz,wght].ttf`
  (retrieved 2026-11-12). Subset generated locally with fontTools `pyftsubset`
  keeping only `0123456789%.` with `--layout-features='*'` (tabular figures) and
  `--flavor=woff2 --no-hinting`; 12,312 B.
- License: `OFL-Newsreader.txt`
- Role: numeric readouts only — flyout master and row values, settings monitor
  cards, OSD percentage. Declared as `Newsreader Lumen` and referenced through
  `--font-num`. Never used for running text.

## Stack

Defined in `app/src/theme.css`:

```
"Inter Lumen", "HarmonyOS Sans SC", "Noto Sans SC Lumen",
"Segoe UI Variable Text", "Microsoft YaHei UI", "Microsoft YaHei",
system-ui, sans-serif
```

`--font-num` replaces the Latin part of the stack for numeric readouts:

```
"Newsreader Lumen", "Inter Lumen", "HarmonyOS Sans SC",
"Noto Sans SC Lumen", "Segoe UI Variable Display", "Microsoft YaHei UI",
"Microsoft YaHei", system-ui, sans-serif
```
