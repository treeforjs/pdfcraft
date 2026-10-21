# Contributors in the About window

**About ▸ Contributors** credits the people who contributed to PdfCraft. Human records and self-chosen names are preserved independently of build tooling.

## Decision

- Two files, both **compiled into the binary** by the UI crate's `build.rs` (nothing is read from
  disk or the network at run time; the web build has them too):
  - [`contributors/contributors.json`](../contributors/contributors.json): commit stats per GitHub
    username. **Generated** by the maintainers' `contributors.py` script (kept outside this
    repository); never hand-edit.
  - [`contributors/people.toml`](../contributors/people.toml): the names people chose for
    themselves. **Each contributor adds only their own entry**, in their own PR.
- Each contributor gets one line: GitHub username (always), display name (opt-in, or their synced
  GitHub profile name), real name (only if they told us), merged PRs, commits, lines added, lines
  deleted, binary assets added, binary assets removed, first and last commit date.
- The list is shown as a **grab bag** (names flowing as text) or a **table**, in the same order. It
  sorts by name, PRs, commits, lines added, lines deleted, line delta, binary assets added/removed,
  first or last commit date. The name toggle cycles **Username → Display name → Real name**; a
  missing name falls back to `@username`. Alphabetical sorting is case-insensitive and ignores the `@`.

## In PdfCraft

- The About window is the `Dialog::About` arm in `crates/ui-egui/src/dialogs.rs` (Help ▸ About, `help.about`).

## Credit yourself

Add your entry to `contributors/people.toml` in a PR you commit yourself (every field optional):

```toml
[people.your-github-username]
real_name = "Your Name"
display_name = "Nickname"
sync_github_name = true   # use the name on your public GitHub profile as display name
```

or, with the maintainers' `contributors.py` script at hand:

```sh
python3 contributors.py --add-me . --real-name "Your Name" --sync-github-name
```

Names show after the next refresh: the script checks that each entry was committed by its own
user (or by a maintainer with a `consent_source`). Delete your entry to remove your names.

## Refreshing (maintainers)

```sh
python3 contributors.py .           # regenerate + verify; needs git, Python 3.11+, gh
python3 contributors.py --check .   # verify people.toml only
```

The build consumes only human records; other generation fields are ignored. Commit the updated `contributors/contributors.json`. Git author names and emails are never recorded.
