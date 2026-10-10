# Export FCS

Writes the projected cells as **FCS 3.1** files, one per sample, in a ZIP. Version 1 is a Rust
implementation; the R operator (0.x) is kept on the
[`r-legacy`](https://github.com/tercen/fcs_export_operator/tree/r-legacy) branch.

## Projection

| | |
|---|---|
| rows | the channel (`$PnN`), and optionally the marker as a second row factor (`$PnS`), e.g. `channel_name` and `c.antigen` |
| columns | the cells, with the file factor (`filename`) and any annotations: UMAP coordinates, cluster, population, event id |
| y | the value |

The file factor splits cells into files; **every other column factor becomes a parameter** of every
file:
- a **number is written as it is**: a UMAP coordinate stays a coordinate;
- **text becomes a code** 1..k in the sorted order of its values. The legend goes in each file's
  `TERCEN_Pn_LEVELS` keyword (`1=B cells; 2=NK cells; ...`) and in `levels.csv` inside the ZIP.

Every cell needs a value for every channel, because an FCS event has one for every parameter. A
gap is an error rather than a silent zero.

## Output

One row: `filename` (the ZIP), `mimetype` (`application/zip`) and `.content`. The table is joined
on nothing, as a file a step returns. Inside the ZIP the names are flat (`<sample>.fcs`), with no
folders. Files are list mode, `$DATATYPE F` (or `D`), little-endian, with `$PnN`/`$PnS`/`$PnR`
and flowCore's range keywords, so flowCore's `read.FCS`, FlowJo and FCS Express read them as
they read a cytometer's.

## Properties

| name | default | |
|---|---|---|
| `file_factor` | (auto) | column factor giving one file per value; empty = the one called `filename`, or one file if there is none |
| `datatype` | `F` | `F` 32-bit float; `D` 64-bit, needed for integer ids above 16,777,216 (refused under `F` rather than rounded) |
| `zip_name` | `Tercen_FCS_Export.zip` | the archive's name |

## Changes from 0.x (R)

- **Transposed projection.** 0.x put the sample on colours and annotations on labels. Tercen stores
  those with every value: 57.7 M rows of annotation for 1.86 M cells on a 20k-per-file Lyme
  workflow. Here they are column factors, stored once per cell.
- **UMAP coordinates are coordinates.** 0.x ran every label through `as.numeric(as.factor(x))`,
  which turns a number into its rank: `umap.1` came out as integers from 1 to ~1.4 M.
- **A legend for text annotations**: 0.x wrote bare codes.
- **No `.ri`** (Tercen's row index) in the files, and **no temporary folders** in the ZIP (0.x
  stored `tmp/Rtmp.../fcs/`).
- **Cells are grouped by the file factor itself.** 0.x joined annotations on the row index and so
  assumed event ids unique across files.
- **Memory:** one pass gathers the values into a cells x channels `f32` matrix (4 bytes a value).
  Files are built and deflated in parallel, a batch at a time. The ZIP is written to disk and
  streamed into the result as base64, never held. 0.x selected every annotation for every value
  and booked 20 GB.

## Tests

- `cargo test`: the FCS writer and reader round trip, the ZIP read back by the `zip` crate,
  grouping, codes and legends, and the streamed result decoded back to the ZIP's bytes.
- `ci/flowcore_check.R` (CI job `flowcore`): `examples/sample.rs` writes three files, and flowCore
  reads every value, name, marker description and legend back.
- `tests/test.json`: the platform test, three samples on synthetic data. `.content` is not
  compared there; the flowCore check covers it.

## Licence

Apache-2.0.
