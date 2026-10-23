# Interface language

Choose **Menu > Edit > Preferences…** (Command-comma on macOS, Ctrl-comma elsewhere) **> Interface language** and select **Auto**, **English**, **日本語**, **简体中文**, **繁體中文**, **Русский**, **Български**, **Čeština**, **Português (Brasil)**, **Deutsch**, **Español**, **Français**, **తెలుగు**, **Magyar**, **Українська** or **Italiano**. The change applies immediately and persists between launches. Command ids, document contents and file names are unchanged.

**Auto** (the default, shown as *Auto (Deutsch)* and so on, naming the language it resolves to) follows the system language: `LC_ALL`, `LC_MESSAGES` or `LANG`, then the preferred-languages list on macOS. Any Portuguese locale (`pt_BR`, `pt_PT`) uses the Brazilian catalog, any French locale (`fr`, `fr_FR`, `fr_CA`, `fr_BE`) the French one, any German locale (`de`, `de_DE`, `de_AT`, `de_CH`) the German one, and Chinese locales for Taiwan, Hong Kong and Macau (`zh_TW`, `zh_HK`, `zh_MO`, `zh-Hant`) the Traditional Chinese one; other Chinese locales (`zh`, `zh_CN`, `zh_SG`, `zh-Hans`) the Simplified Chinese one. Telugu locales (`te`, `te_IN`) use the Telugu catalog, Bulgarian locales (`bg`, `bg_BG`) the Bulgarian one, and Hungarian locales (`hu`, `hu_HU`) the Hungarian one, and Ukrainian locales (`uk`, `uk_UA`) the Ukrainian one, and Italian locales (`it`, `it_IT`, `it_CH`) the Italian one. A system language without a catalog shows English. On Windows, **Auto** uses the Windows display languages, in preference order, when no supported locale is selected by those environment variables. Neutral `C`/`POSIX` locale values (including `C.UTF-8`) inherited from a launcher do not override the Windows display language; explicitly named supported locales such as `fr_FR.UTF-8` still do. The query asks Windows directly (`GetUserPreferredUILanguages`, through the `sys-locale` crate) without starting a process, and is cached for the process. If detection fails or the display language has no catalog, PdfCraft shows English.

The control channel exposes the setting through `ui.set`:

```json
{"method":"ui.set","params":{"key":"language","value":"ja"}}
```

The value is `auto` or a language code (`en`, `ja`, `zh-hans`, `zh-hant`, `ru`, `bg`, `cs`, `pt-br`, `de`, `es`, `fr`, `te`, `hu`, `uk`, `it`), in any case. `ui.state` reports `language` as written in the preferences (`auto`, `en`, `ja`, `zh-hans`, `zh-hant`, `ru`, `bg`, `cs`, `pt-br`, `de`, `es`, `fr`, `te`, `hu`, `uk` or `it`). Unknown values return an error without changing the current setting. Preferences saved before this setting existed follow the system language.

Dialogs, panels, menus, notices and history labels go through the catalog (`tl!`). Every catalog (Japanese, Traditional Chinese, Simplified Chinese, Russian, Bulgarian, Czech, Brazilian Portuguese, German, Spanish, French, Telugu, Hungarian, Ukrainian and Italian, about 2,400 entries each) translates every one of them; a label without an entry would show in English, and the coverage check below keeps that from shipping. Error details that come from the engine or the operating system are shown as they are, inside a translated frame ("操作「…」に失敗しました: …"). The command palette matches the translated label, the English label and the command id. Vertical Japanese PDF rendering is an existing viewer feature; this change does not add vertical text editing.

The Ukrainian catalog has 2,047 entries, written from the meaning of the English labels using Ukrainian computing vocabulary. Commands use infinitives; fixed-count messages use count-neutral wording. Plural-aware messages use the three [CLDR Ukrainian integer forms](https://www.unicode.org/cldr/charts/48/supplemental/language_plural_rules.html#uk): `1 сторінка`, `2 сторінки`, `5 сторінок`; 11–14 take the third form, while 21 and 22 return to the first and second. Calendar dates use genitive month names (`9 жовтня 2026`), while headings use standalone names (`Жовтень 2026`). Home recommendation cards grow to fit translated text; the toolbar search hint truncates before the shortcut, with the full label available on hover.

Japanese interface text uses BIZ UDPGothic from [craft-fonts](https://github.com/storytold/craft-fonts), an optional build input that every release includes (`CRAFT_FONTS_DIR`; see the README). A build made without it has no Japanese face, so Japanese labels show replacement boxes. Chinese is drawn with the same craft-fonts CJK faces (in Simplified Chinese the `Hans` faces come first, so a line never mixes faces with different baselines), so a character none of them has shows a replacement box (and every Chinese character does in a build without craft-fonts). Russian, Bulgarian, Czech, Brazilian Portuguese, German, Spanish, French, Hungarian and Italian need only the bundled Latin faces (tested in `tests/fonts.rs`). Hungarian uses the same one/other plural rule as English: 1 is singular, and every other count — including 0 — is the other form. After a numeral the noun stays singular (`1 oldal`, `2 oldal`). Telugu is drawn with a craft-fonts `Telu` face (Noto Sans Telugu, kept in the web build too), whose conjuncts egui shapes; a build without one shows Telugu labels as replacement boxes, and the release-pinned craft-fonts input has no Telugu face yet.
The Brazilian Portuguese catalog has 2,000 entries: the ~1,826-entry gist André Cardoso posted in [#288](https://github.com/storytold/pdfcraft/issues/288), the labels added since, and a review of the existing wording. Portuguese, like French, treats 0 and 1 as singular (`1 página`, `2 páginas`).


There is no right-to-left interface language yet. File names and document titles in Arabic or Hebrew are drawn with a craft-fonts `Arab` face when the build input has one; on desktop, a character no embedded face has is drawn with one font already installed on the machine (`PDFCRAFT_SYSTEM_FONTS=0` turns this off). The tab strip, the recent-files list and Properties show such names in display order; other places (bookmarks, comments, search results) have the glyphs but may order mixed-direction text wrongly.

Japanese, Simplified Chinese, French, German and Brazilian Portuguese coverage tests, following PhotoCraft's complete-catalog approach, check every registered command, All tools label and `tl!("literal")` in the UI source. Simplified Chinese also checks direct `i18n::t` calls and multiline literals, plus the About tabs, contributor name modes, sort options, table headers and model columns. Generated history labels, contributor summaries and diagnostics are tested with user-supplied names and error details. Catalog coverage does not guarantee glyph coverage: the release-pinned craft-fonts input currently has no `Hans` face, and the web build embeds only BIZ UDPGothic Regular. Chinese font delivery remains separate work, as discussed in [#112](https://github.com/storytold/pdfcraft/pull/112) and [#140](https://github.com/storytold/pdfcraft/pull/140).

German labels keep nouns capitalized, including tool names inserted into notices. The certificate viewer uses the `certificate` context for `Subject` (the certificate holder), while document properties keep the plain translation (the topic). Search hints reserve room for the shortcut, and Home cards limit their title and description to the available rows; hovering shows the full text when it is shortened.

The command `app.language` (*Interface language…* in the command palette) opens Preferences on that page.

## Numbers, dates and plurals

`pdfcraft_engine::locale` holds each language's conventions, shared by the app and the automation tools:

- **Plurals** follow the CLDR integer rules: one/other (English, German, Spanish, Italian, Bulgarian, Telugu, Hungarian), 0 and 1 singular (French, Brazilian Portuguese), one/few/other (Czech), one/few/many (Russian, Ukrainian) and a single form (Japanese, Chinese). `trn` picks the catalog's form and writes the count with the language's digit grouping.
- **Numbers**: `i18n::decimal` and `i18n::integer` use the language's decimal separator (`.` or `,`) and group separator (`,`, `.` or a no-break space), Indian grouping for Telugu (12,34,567) and no grouping of four-digit numbers in Spanish and Bulgarian. File sizes (`i18n::file_size`) translate their units (`Ko`/`Mo` in French, `КБ`/`МБ` in Russian, Bulgarian and Ukrainian) and pluralise bytes.
- **Dates**: `i18n::date` writes the short numeric date (`2026-10-08` in English, `08.10.2026` in German, `8. 10. 2026` in Czech, `2026/10/08` in Japanese …) and `i18n::pdf_date` a PDF date with a 24-hour time. Fill & Sign dates keep their own pattern (Preferences → Forms → Date format) with month and weekday names in the interface language.
- **Direction**: every language offered is left to right. No right-to-left language is offered yet: Arabic or Hebrew would need the mirrored interface first (`misc.rtl-ui`), and the registry test refuses a right-to-left language until it exists.

Formatting runs only where a number or a date is drawn; catalog lookups per frame are unchanged (one hash lookup per string).

## Agents and scripts

Command ids, English labels, document contents and file names never change with the language. The control channel adds what the person at the app reads: `ui.commands` lists `label_localized` and `menu_localized` next to `label` and `menu`, and `ui.state` reports `interface_language` (code, name, direction) besides the `language` preference. Headless, the preference is `general.language` (`prefs_get`/`prefs_set`), and three tools cover the rest: `ui_language_get` (the preference, the language in effect, what Auto resolves to from the environment, and every language with its separators, plural categories and samples), `ui_language_set` (a code, `auto` or a locale tag such as `de_AT.UTF-8`) and `locale_format` (a number, integer, date, PDF date or count written the way a language writes it).

## Coverage check

`i18n::tests::every_catalog_is_complete` fails when any bundled catalog has a parse error (a duplicate key, or a placeholder, trailing ellipsis or plural-form count that differs from the English source), lacks an entry for a registered command, an All tools group, section or item, a `tl!` or `tl_ctx!` literal or a `trn` plural message in the UI source, or leaves English text in a translation. Run it with `cargo test -p pdfcraft-ui-egui --lib i18n::`; the glyph tests in the same module also run when `CRAFT_FONTS_DIR` is set.

## How translations work

PdfCraft uses the same system as PhotoCraft (`crates/ui-egui/src/i18n/`):

- Strings in the code stay in English and serve as lookup keys. At draw time, `tl!("Save")` returns the text in the current language, and falls back to English when there is no entry.
- Each language has a catalog, `crates/ui-egui/src/i18n/<code>.tsv`, with one entry per line: `context<TAB>English<TAB>translation`. An empty context marks a plain string. `@id` keys an entry by command id (for example `file.saveAs`), so one menu item can read differently from another with the same English label. `@plural` holds plural forms. Any other context disambiguates an English word with several meanings (`tl_ctx!` in UI code, for example the signature pad's "Type" button against the "Type" column). The header of `ja.tsv` documents the format and escapes.
- Placeholders (`{name}`) must appear in both columns. A trailing `…` must be kept. `i18n::fmt` fills placeholders in one pass, so a value such as a file name containing `{n}` is inserted as written.
- Catalogs are validated strictly: unknown escapes, unknown `@` contexts, placeholder or ellipsis mismatches, wrong plural form counts and duplicates are errors. The tests require the bundled catalogs to have none; at run time a bad line is skipped (and logged) and that string shows in English.
- Catalogs are assets: each `.tsv` has an `ATTRIBUTION.toml` entry with `kind = "translation"` and its SHA-256, checked by `cargo xtask assets`.
- Translations are clean-room: write them from the meaning of the English text, never from another product's string tables.

## Adding a language

1. Copy the header of `ja.tsv` into `xx.tsv` and translate entries.
2. Add one row to `LANGUAGES` in `crates/ui-egui/src/i18n/mod.rs`: code, native name, catalog and plural rule.
3. Add an `ATTRIBUTION.toml` entry for the catalog (`kind = "translation"`), then run `cargo xtask assets --write` and `cargo xtask assets`.

The Preferences dropdown, the system-language match and the catalog tests (format, duplicates, placeholders, plural forms, command ids) then pick it up.
