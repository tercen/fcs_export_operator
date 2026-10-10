//! fcs_export_operator — write the projected cells as FCS files, in a ZIP.
//!
//! Projection: rows = channels (the first row factor is `$PnN`, a second one `$PnS`, e.g. the
//! antigen), columns = cells, y = the value. One column factor splits cells into files
//! (`file_factor`, by default `filename`); every other column factor becomes a parameter of every
//! file (see [`export`]).
//!
//! One pass to gather the values into a cells x channels `f32` matrix (4 bytes a value), then
//! the files are built and deflated in parallel and the ZIP is written to disk, then streamed
//! into the result. Peak memory is the matrix plus a batch of files, not the export.
pub mod context;
pub mod export;
pub mod fcs;
pub mod input;
pub mod output;
pub mod pagecache;
pub mod progress;
pub mod props;
pub mod tson;
pub mod upload;
pub mod zipw;

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use tercen_rs::context::ContextBase;
use tercen_rs::{DevContext, TercenClient};

use export::{Annotation, Export, Values};
use progress::Reporter;
use tson::TsonWriter;

const CHUNK: usize = 1_000_000;

pub fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

pub fn require_env(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| anyhow!("{name} is not set"))
}

pub async fn run(task_id: &str) -> Result<()> {
    tracing::info!("fcs_export_operator starting (task_id={task_id})");
    let client = build_client().await?;
    let ctx = context::from_task_id(client, task_id).await?;
    execute(
        &ctx,
        Mode::Production {
            task_id: task_id.to_string(),
        },
    )
    .await
}

pub async fn run_dev(workflow_id: &str, step_id: &str) -> Result<()> {
    tracing::info!("fcs_export_operator starting in dev mode ({workflow_id} / {step_id})");
    let client = build_client().await?;
    let ctx = DevContext::from_workflow_step(client, workflow_id, step_id)
        .await
        .map_err(|e| anyhow!("load workflow {workflow_id} / step {step_id}: {e}"))?;
    execute(
        &ctx,
        Mode::Dev {
            workflow_id: workflow_id.to_string(),
            step_id: step_id.to_string(),
        },
    )
    .await
}

enum Mode {
    Production {
        task_id: String,
    },
    Dev {
        workflow_id: String,
        step_id: String,
    },
}

async fn build_client() -> Result<Arc<TercenClient>> {
    let client = TercenClient::from_env()
        .await
        .map_err(|e| anyhow!("connect to Tercen: {e}"))?;
    tracing::info!("connected to Tercen");
    Ok(Arc::new(client))
}

/// The column factor that names files: the property, or one called `filename`.
pub fn pick_file_factor(names: &[String], wanted: &str) -> Result<Option<String>> {
    if !wanted.is_empty() {
        return names
            .iter()
            .find(|n| n.as_str() == wanted || n.ends_with(&format!(".{wanted}")))
            .cloned()
            .map(Some)
            .ok_or_else(|| {
                anyhow!("file_factor '{wanted}' is not a column factor (columns are {names:?})")
            });
    }
    Ok(names
        .iter()
        .find(|n| n.as_str() == "filename" || n.ends_with(".filename"))
        .cloned())
}

async fn execute(ctx: &ContextBase, mode: Mode) -> Result<()> {
    let t_start = Instant::now();
    let rep = match &mode {
        Mode::Production { task_id } => Reporter::spawn(Arc::clone(ctx.client()), task_id.clone()),
        Mode::Dev { .. } => Reporter::silent(),
    };
    let s = props::read(ctx)?;

    // Channels: the first row factor names them, a second describes them.
    let rnames = ctx
        .rnames()
        .await
        .map_err(|e| anyhow!("read row factor names: {e}"))?;
    if rnames.is_empty() {
        bail!("put the channels on rows (and, optionally, the marker as a second row factor)");
    }
    let as_text = |v: Values| -> Vec<String> {
        match v {
            Values::Text(t) => t,
            Values::Num(n) => n.iter().map(|x| x.to_string()).collect(),
        }
    };
    let channels = as_text(input::factor_values(ctx, ctx.row_hash(), &rnames[0]).await?);
    let descs = match rnames.get(1) {
        Some(n) => Some(as_text(input::factor_values(ctx, ctx.row_hash(), n).await?)),
        None => None,
    };
    let p = channels.len();

    // Cells: the file factor and the annotations, one value per column.
    let cnames = ctx
        .cnames()
        .await
        .map_err(|e| anyhow!("read column factor names: {e}"))?;
    let file_factor = pick_file_factor(&cnames, &s.file_factor)?;
    let mut file_labels: Option<Vec<String>> = None;
    let mut annotations: Vec<Annotation> = Vec::new();
    for name in &cnames {
        let v = input::factor_values(ctx, ctx.column_hash(), name).await?;
        if Some(name) == file_factor.as_ref() {
            file_labels = Some(as_text(v));
        } else {
            annotations.push(export::annotation(name, v, s.dtype)?);
        }
    }
    let n_values = input::cell_count(ctx).await?;
    let n_cells = match (&file_labels, annotations.first()) {
        (Some(l), _) => l.len(),
        (None, Some(a)) => a.values.len(),
        (None, None) => n_values / p.max(1),
    };
    if annotations.iter().any(|a| a.values.len() != n_cells) {
        bail!("the column factors do not all have {n_cells} values");
    }
    if n_values != n_cells * p {
        bail!(
            "{n_values} values for {n_cells} cells x {p} channels: every cell needs a value for every \
             channel. Filter the projection, or fill the gaps upstream."
        );
    }
    tracing::info!(n_values, n_cells, channels = p, annotations = annotations.len(), file_factor = ?file_factor, "projection");

    rep.at(0, "Reading the crosstab");
    let data = gather(ctx, n_values, n_cells, p, &rep).await?;
    let files = export::group_files(file_labels.as_deref(), n_cells, "Tercen_FCS_Export");
    drop(file_labels);

    let work_root = std::env::temp_dir().join(format!(
        "fcs_export_{}_{}",
        ctx.workflow_id(),
        ctx.step_id()
    ));
    std::fs::create_dir_all(&work_root)
        .with_context(|| format!("create {}", work_root.display()))?;
    let _guard = TempDirGuard(work_root.clone());
    let zip_path = work_root.join("export.zip");

    rep.at(
        progress::WRITE.0,
        format!("Writing {} FCS files", files.len()),
    );
    let t = Instant::now();
    let threads = rayon::current_num_threads();
    let (n_files, raw) = {
        let f = std::fs::File::create(&zip_path)
            .with_context(|| format!("create {}", zip_path.display()))?;
        let w = std::io::BufWriter::with_capacity(4 << 20, f);
        let e = Export {
            channels: &channels,
            descs: descs.as_deref(),
            data: &data,
            n: n_cells,
            annotations: &annotations,
            dtype: s.dtype,
        };
        e.write_zip(&files, w, 2 * threads, |done, total| {
            rep.at(
                progress::band(progress::WRITE, done, total),
                format!("Wrote {done} of {total} files"),
            );
        })?
    };
    drop(data);
    let zip_len = std::fs::metadata(&zip_path)?.len();
    tracing::info!(
        files = n_files,
        raw_bytes = raw,
        zip_bytes = zip_len,
        threads,
        secs = format!("{:.1}", t.elapsed().as_secs_f64()),
        "zip written"
    );
    rep.info(format!(
        "{n_files} FCS files, {n_cells} cells x {} parameters, {:.1} MB zipped",
        p + annotations.len(),
        zip_len as f64 / 1e6
    ));

    let result_path = work_root.join("result.tson");
    {
        let out = std::fs::File::create(&result_path)?;
        let w =
            std::io::BufWriter::with_capacity(4 << 20, pagecache::Releasing::new(out, 256 << 20));
        let mut w = TsonWriter::new(w)?;
        let zip = std::io::BufReader::new(std::fs::File::open(&zip_path)?);
        output::write_result(&mut w, ctx.namespace(), &s.zip_name, zip, zip_len)?;
    }
    std::fs::remove_file(&zip_path).ok();
    pagecache::release_path(&result_path);

    rep.at(progress::UPLOAD.0, "Uploading the result");
    match mode {
        Mode::Production { task_id } => {
            upload::save_production(ctx, &task_id, &result_path, &rep).await?
        }
        Mode::Dev {
            workflow_id,
            step_id,
        } => {
            let saved = upload::save_dev(ctx, &workflow_id, &step_id, &result_path).await?;
            tracing::info!(task_id = saved.task_id, "dev result saved");
        }
    }
    rep.at(100, "Done");
    tracing::info!(
        total_secs = format!("{:.1}", t_start.elapsed().as_secs_f64()),
        peak_rss_kb = peak_rss_kb().unwrap_or(0),
        "done"
    );
    Ok(())
}

/// Gather the crosstab into a column-major cells x channels `f32` matrix. Scattered by
/// `(.ri, .ci)`, so no order is assumed; a gap is an error, because an FCS event has a value for
/// every parameter.
async fn gather(
    ctx: &ContextBase,
    n_values: usize,
    n_cells: usize,
    p: usize,
    rep: &Reporter,
) -> Result<Vec<f32>> {
    let mut data = vec![f32::NAN; n_cells * p];
    let mut seen = 0usize;
    let mut out_of_range = 0usize;
    input::for_each_chunk(ctx, &[".ri", ".ci", ".y"], n_values, CHUNK, |c| {
        for k in 0..c.y.len() {
            let (ri, ci) = (c.ri[k] as usize, c.ci[k] as usize);
            if ri >= p || ci >= n_cells {
                out_of_range += 1;
                continue;
            }
            data[ci + ri * n_cells] = c.y[k] as f32;
        }
        seen += c.len();
        rep.at(
            progress::band(progress::READ, seen, n_values.max(1)),
            format!("Read {seen} of {n_values}"),
        );
        Ok(())
    })
    .await?;
    if out_of_range > 0 {
        bail!(
            "{out_of_range} values fell outside the {n_cells} x {p} crosstab; the projection changed under the run"
        );
    }
    let missing = data.iter().filter(|v| v.is_nan()).count();
    if missing > 0 {
        bail!(
            "{missing} of {} cell-channel pairs have no value (or NaN). Every FCS event needs a value \
             for every channel: filter the projection, or fill the gaps upstream.",
            data.len()
        );
    }
    Ok(data)
}

struct TempDirGuard(std::path::PathBuf);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn peak_rss_kb() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_factor_by_property_or_name() {
        let names: Vec<String> = ["event_id", "filename", "ds6.umap.1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            pick_file_factor(&names, "").unwrap(),
            Some("filename".into())
        );
        assert_eq!(
            pick_file_factor(&names, "umap.1").unwrap(),
            Some("ds6.umap.1".into())
        );
        assert!(pick_file_factor(&names, "sample").is_err());
        assert_eq!(pick_file_factor(&names[..1], "").unwrap(), None);
    }
}
