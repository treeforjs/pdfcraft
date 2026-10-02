# printcraft-fonts

Layer L2. Font helpers for the appearance streams PrintCraft generates (comments, form fields).

Today it holds what generated appearances need without a font program:

- `helvetica_width`: an approximation of Helvetica's proportions by character class. No metrics
  file or font program from any vendor is bundled; widths are PrintCraft's own estimates, good
  enough for line breaking and alignment, not for typesetting.
- `wrap`: greedy line breaking with that measure (paragraphs on newlines, long words split).
- `win_ansi`: Unicode → WinAnsiEncoding bytes (`?` for characters it can't represent), and
  `literal` to write bytes as a PDF literal string.

The full font subsystem (parsing, shaping, subsetting and embedding) arrives with M2.2/M7.
