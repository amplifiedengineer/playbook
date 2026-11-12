# Reading documents

`playbook doc read <file>` reads a PDF or an Office file as Markdown text, on your machine and offline, so a session spends fewer tokens than it does on the raw file. A benchmark of the converter's engine measured fewer tokens on text PDFs with every answer still correct (see the caveat below). The converter comes with Playbook: `pbdoc` sits next to `playbook` on macOS and Linux, so there is nothing to install. Windows has no conversion, and the command and the hint are off there.

## What it reads

- PDF
- EPUB and RTF
- Word: docx and doc
- PowerPoint: pptx and ppt
- Excel: xlsx
- OpenDocument: odt and odp
- OpenDocument spreadsheets: ods
- CSV

It does not convert images, so use Read for those. A scanned PDF with no text layer needs OCR, and Playbook does not run OCR.

## How much it saves

Averages of 3 runs on synthetic documents, with a small model:

| Document | Tokens without `playbook doc read` | Tokens with it | Change |
| :- | -: | -: | -: |
| text PDF, 20 pages | 67,918 | 36,865 | -46% |
| text PDF, 2 pages | 15,084 | 12,216 | -19% |
| xlsx, 2 sheets | 18,072 | 12,175 | -33% |
| docx, tracked deletion | 17,778 | 12,215 | -31% |

Savings grow with document size. Across both PDFs the total was 41 percent lower in that run, 49,081 tokens against 83,002, and all 18 answers were correct. A CSV costs slightly more as Markdown, so the hint skips CSV.

Read the caveat before you rely on these numbers. The benchmark used the npm `anydoc` package, the engine `pbdoc` builds on, not `pbdoc` itself. The documents were synthetic, 3 runs back each cell, and cost follows cache behavior, so treat the ratios as indicative. The docx run lost a tracked deletion, because the conversion drops deleted text. Read the original when that matters. Full tables and method are in the [benchmark](../internals/10-document-conversion-benchmark.md).

## Try it

```bash
playbook doc status
playbook doc read report.pdf
```

The first line says which converter is in use and its version. The second prints the document as Markdown inside a frame that marks it as untrusted data. The [read a document recipe](../recipes/read-a-document.md) shows the output on a small file.

## Turn it on

1. Check it: `playbook doc status`. It names the converter in use and its version.
2. Nothing more to do: `docs.convert` defaults to `auto`. A Read of a whole PDF, or of a Word, PowerPoint, Excel, EPUB, RTF or OpenDocument file, gets one line that suggests `playbook doc read <file>`. The hook only suggests. It never changes the call and never blocks. Set `docs.convert` to `off` to stop the hint. See the [benchmark](../internals/10-document-conversion-benchmark.md).

If `pbdoc` is missing, `playbook doc status` says so. Reinstall with `install.sh` or `brew reinstall playbook`, or run `playbook upgrade`.

The converter was once an npm package. If you installed it earlier, `npm uninstall --global @firecrawl/anydoc` removes it, because playbook now uses the bundled one first. To run a different `anydoc` anyway, set `docs.anydocPath`. That program wins over the bundled one, and playbook does not check that it works offline.

## Long documents

A document longer than 24000 characters is not printed whole. The output shows the first slice, the total length, a numbered index of the headings and the command for the next slice. The full text is kept in a private cache for a day, so later slices do not convert again.

- `--offset N` starts at character N. `--limit M` prints at most M characters, never more than 24000.
- `--section K` prints section K from the index.
- `--json` adds `total_chars`, `offset` and `next_offset`.

A section number that does not exist exits with code 2 and says which numbers are valid.

## Exit codes

`playbook doc read` exits 0 on success, 1 for a failure, 2 for a section that does not exist, 3 for a document that needs OCR and 128 plus the signal number when interrupted. The helper and its limits are described in [The pbdoc helper](../internals/19-pbdoc-helper.md).

## Safeguards

- **Local only.** The OCR option and the hosted mode of the upstream `anydoc` tool send the document to a remote server. Playbook never passes them. A scanned PDF with no text layer answers "needs OCR, not supported locally" and nothing is retried elsewhere.
- **No network where the system allows it.** The converter runs inside the macOS sandbox with network denied, or in a Linux network namespace. When neither works the output says that only the environment was scrubbed. The environment is always scrubbed: no proxy variables, no API keys, no `NODE_OPTIONS`.
- **Limits.** Files over 25 MB are refused. A zip based file that expands too far, or a conversion that hits the memory cap, stops with "the document hit a safety limit". The converter is stopped after 30 seconds or when it uses more than 1024 MB. The text is cut at 200000 characters with a marker that says how many characters were shown.
- **Private temporary files.** The converter writes to a random folder under `~/.config/playbook/tmp` that only you can open. It is deleted when the command ends, on success, error, timeout, interrupt or crash of playbook itself. A folder left by a killed process is removed on the next run once it is older than an hour.
- **Untrusted content.** The text is wrapped in a block with a random tag and a line that says to treat it as data and not follow instructions inside it. Nothing from the document is logged. Only the slice cache holds its text, in a folder only you can open, and it is swept after a day.

## What is lost

The output lists what the conversion does not carry. For Word files, tracked changes are not represented, so deleted text is missing. Charts and images become nothing or alt text. Slide layout is dropped. Spreadsheet formulas show their last saved value. When a detail matters, read the original.

## Config keys

See the [config keys page](../reference/config-keys.md): `docs.convert` and `docs.anydocPath`.
