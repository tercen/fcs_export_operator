//! A synthetic export for `ci/flowcore_check.R`: writes `<dir>/sample.zip` and the values each
//! file must hold, `<dir>/expected.csv` (file, then every parameter, one row per event).
//!   cargo run --example sample -- <dir>
use fcs_export_operator::export::{Export, Values, annotation, group_files};
use fcs_export_operator::fcs::DataType;

fn main() -> anyhow::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    let n = 600usize;
    let channels: Vec<String> = ["BUV395-A", "BV421-A", "PE-A", "APC-A"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let descs: Vec<String> = ["CD3", "CD4", "CD8", "CD19"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let data: Vec<f32> = (0..channels.len())
        .flat_map(|j| (0..n).map(move |c| ((c * 37 + j * 101) % 997) as f32 / 97.0 - 2.0))
        .collect();
    let files: Vec<String> = (0..n).map(|c| format!("sample {}", c % 3)).collect();
    let ann = vec![
        annotation(
            "umap.1",
            Values::Num((0..n).map(|c| (c as f64) * 0.013 - 4.0).collect()),
            DataType::F,
        )?,
        annotation(
            "event_id",
            Values::Num((0..n).map(|c| c as f64 + 1.0).collect()),
            DataType::F,
        )?,
        annotation(
            "population",
            Values::Text(
                (0..n)
                    .map(|c| ["T cells", "B cells", "NK, CD16+"][c % 7 % 3].to_string())
                    .collect(),
            ),
            DataType::F,
        )?,
    ];
    let e = Export {
        channels: &channels,
        descs: Some(&descs),
        data: &data,
        n,
        annotations: &ann,
        dtype: DataType::F,
    };
    let groups = group_files(Some(&files), n, "x");
    e.write_zip(
        &groups,
        std::fs::File::create(dir.join("sample.zip"))?,
        4,
        |_, _| {},
    )?;
    let mut csv = String::from("file,BUV395-A,BV421-A,PE-A,APC-A,umap.1,event_id,population\n");
    for (name, cells) in &groups {
        for &c in cells {
            let mut row = vec![name.clone()];
            for j in 0..channels.len() {
                row.push(format!("{}", data[c + j * n]));
            }
            for a in &ann {
                row.push(format!("{}", a.values[c] as f32));
            }
            csv.push_str(&row.join(","));
            csv.push('\n');
        }
    }
    std::fs::write(dir.join("expected.csv"), csv)?;
    Ok(())
}
