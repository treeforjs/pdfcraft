# pdfcraft-arlington

The [Arlington PDF Model](https://github.com/pdf-association/arlington-pdf-model) (PDF
Association) as data, for structural validation (`core.arlington-validation`). L0, standalone: no
workspace dependencies, builds for wasm32.

- `data/arlington-model.tsv`: the model's `tsv/latest` files at commit
  `d37f4c8b11f54ae0d81a278bdeb91d00d6ccde26`, concatenated (one `#Object` line per file, header row
  and the free-text Note column removed; 594 object types, about 390 KB). Apache-2.0: licence in
  `data/LICENSE-arlington.txt`, upstream notice in `data/NOTICE-arlington.txt`, attribution in
  the repository's `ATTRIBUTION.toml`.
- `Model` / `model()`: the parsed definitions (keys or array elements, types, SinceVersion,
  DeprecatedIn, Required, IndirectReference, Inheritable, PossibleValues, Link).
- `expr`: the predicate language, evaluated with three-valued logic. Anything it can't decide is
  `Unknown`, which callers report as "not checked", never as a problem.

The document walk that applies the model lives in `pdfcraft-preflight` (`arlington` module),
exposed as the `doc_validate_arlington` tool.

## Predicates

Evaluated (`expr::EVALUATED`): `fn:SinceVersion`, `fn:BeforeVersion`, `fn:IsPDFVersion`,
`fn:Deprecated`, `fn:Not`, `fn:IsPresent` (one and two arguments), `fn:NotPresent`, `fn:Eval`,
`fn:ArrayLength`, `fn:Contains`, `fn:RectWidth`, `fn:RectHeight`, `fn:FileSize`,
`fn:StringLength`, `@key` values (with `::` paths and `trailer::`), `==`, `!=`, `<`, `<=`, `>`,
`>=`, `&&`, `||`, `+`, `-`, `*`, `mod`. In PossibleValues also `fn:RequiredValue`, and version
predicates and `fn:Extension` wrapped around types, links and values.

Not evaluated (the rule is counted as not checked): `parent::` paths, `fn:Extension` as a
condition (extensions are assumed to be in use, so their keys are known and not version-checked
when no version is given), and every other predicate, among them `fn:NotStandard14Font`,
`fn:FontHasLatinChars`, `fn:InNameTree`, `fn:IsEncryptedWrapper`, `fn:ImageIsStructContentItem`,
`fn:PageContainsStructContentItems`, `fn:IsPDFTagged`, `fn:HasSpotColorants`,
`fn:HasProcessColorants`, `fn:PageProperty`, `fn:NumberOfPages`, `fn:BitsClear` and the other bit
predicates. The SpecialCase column is not evaluated at all, nor are DefaultValue and possible
values of array-typed keys.

## Regenerating the data

```sh
git clone https://github.com/pdf-association/arlington-pdf-model /tmp/arlington
cd /tmp/arlington && COMMIT=$(git rev-parse HEAD) && cd tsv/latest
{ echo "## Arlington PDF Model (PDF Association), Apache-2.0: https://github.com/pdf-association/arlington-pdf-model commit $COMMIT, tsv/latest. Generated: one '#Object' line per TSV file, header row and Note column removed."
  for f in $(ls *.tsv | LC_ALL=C sort); do echo "#${f%.tsv}"; tail -n +2 "$f" | cut -f1-11; done
} > crates/arlington/data/arlington-model.tsv
```

Then update `MODEL_COMMIT` in `src/lib.rs`, the commit and `sha256` of the entry in
`ATTRIBUTION.toml`, run `cargo xtask assets --write`, and run this crate's tests (they check that
every row parses and every link names a definition).
