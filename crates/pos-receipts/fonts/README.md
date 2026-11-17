# Embedded fonts (E13-T04, ADR-014)

The PDF renderer embeds these fonts in the binary so a receipt never depends on the fonts
of the computer that makes it. All are from the Noto project and licensed under the
SIL Open Font License 1.1 (`OFL.txt`, which must ship with the app).

| File | Source | Used for |
|---|---|---|
| `NotoSansMono-Regular.ttf`, `NotoSansMono-Bold.ttf` | notofonts.github.io `fonts/NotoSansMono/unhinted/ttf/` | Receipt-width PDFs (the 32/48-column grid) |
| `NotoSans-Regular.ttf`, `NotoSans-Bold.ttf` | `fonts/NotoSans/unhinted/ttf/` | A4 PDFs |
| `NotoSansArabic-Regular.ttf` | `fonts/NotoSansArabic/unhinted/ttf/` | Arabic fallback |
| `NotoSansDevanagari-Regular.ttf` | `fonts/NotoSansDevanagari/unhinted/ttf/` | Devanagari fallback |
| `NotoSansCJK-Subset.ttf` | google/fonts `ofl/notosanssc/NotoSansSC[wght].ttf`, weight 400, subset | Chinese and Japanese: GB 2312 (simplified Chinese) + JIS X 0208 (kana, kanji) + CJK punctuation and full-width forms (10,709 characters) |
| `NotoSansKR-Subset.ttf` | google/fonts `ofl/notosanskr/NotoSansKR[wght].ttf`, weight 400, subset | Korean: the 2,350 KS X 1001 Hangul syllables and compatibility jamo |

Unhinted builds are used: hinting does nothing in a PDF and costs size. The two subsets
keep the binary small (the full CJK fonts are 10–18 MB); a name using a character outside
them is reported by `pos_receipts::pdf::missing_glyphs`.

## Regenerating the subsets

```sh
pip install fonttools
python3 cjk_charsets.py   # writes han.txt and hangul.txt
fonttools varLib.instancer "NotoSansSC[wght].ttf" wght=400 --update-name-table -o sc400.ttf
fonttools varLib.instancer "NotoSansKR[wght].ttf" wght=400 --update-name-table -o kr400.ttf
pyftsubset sc400.ttf --text-file=han.txt --layout-features='*' --no-hinting --name-IDs='*' --output-file=NotoSansCJK-Subset.ttf
pyftsubset kr400.ttf --text-file=hangul.txt --layout-features='*' --no-hinting --name-IDs='*' --output-file=NotoSansKR-Subset.ttf
```
