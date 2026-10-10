//! From the gathered crosstab to FCS files in a ZIP.
//!
//! Rows are channels, columns are cells. One column factor (the file factor) says which file
//! a cell goes to; **every other column factor becomes a parameter** of every file: a number is
//! written as it is (a UMAP coordinate stays a coordinate), text becomes a code 1..k in the
//! sorted order of its values, with the legend in the file's keywords and in `levels.csv`.
//!
//! The 0.x R operator put files on colours and annotations on labels, which Tercen stores with
//! every value: 57.7 M rows of annotation for 1.86 M cells, which is what made its query run out
//! of memory. As column factors they are stored once per cell. It also turned every label into
//! its rank (`as.numeric(as.factor(x))`), so UMAP coordinates came out as 1..n.
//!
//! Files are independent, so they are built and deflated in parallel (rayon), a batch at a time
//! so that at most a batch of compressed files is held before it is written out in order.
use std::io::Write;

use anyhow::{Result, bail};
use rayon::prelude::*;

use crate::fcs::{self, DataType, Param};
use crate::zipw::{Entry, ZipWriter};

/// A column factor's values, one per cell.
#[derive(Debug, Clone)]
pub enum Values {
    Num(Vec<f64>),
    Text(Vec<String>),
}

/// A column factor turned into a parameter: one f64 per cell, and the legend when it was text.
#[derive(Debug, Clone)]
pub struct Annotation {
    pub name: String,
    pub values: Vec<f64>,
    pub levels: Option<Vec<String>>,
}

/// Largest integer a float32 holds exactly.
const F32_EXACT: f64 = 16_777_216.0;

/// Turn a column factor into a parameter.
pub fn annotation(name: &str, values: Values, dtype: DataType) -> Result<Annotation> {
    match values {
        Values::Num(v) => {
            if let Some(bad) = v.iter().find(|x| !x.is_finite()) {
                bail!(
                    "column factor '{name}' has a missing or infinite value ({bad}); FCS cannot hold it"
                );
            }
            if dtype == DataType::F
                && let Some(big) = v.iter().find(|x| x.abs() > F32_EXACT && x.fract() == 0.0)
            {
                bail!(
                    "column factor '{name}' holds {big}, which a 32-bit FCS file cannot store exactly \
                     (largest exact integer is 16,777,216). Set datatype to D, or leave the factor out."
                );
            }
            Ok(Annotation {
                name: name.to_string(),
                values: v,
                levels: None,
            })
        }
        Values::Text(v) => {
            let mut levels: Vec<String> = v.clone();
            levels.sort();
            levels.dedup();
            let codes = v
                .iter()
                .map(|s| (levels.binary_search(s).unwrap() + 1) as f64)
                .collect();
            Ok(Annotation {
                name: name.to_string(),
                values: codes,
                levels: Some(levels),
            })
        }
    }
}

/// File name for a file-factor value: path separators and control characters replaced, `.fcs`
/// added unless it is there.
pub fn file_name(label: &str) -> String {
    let mut s: String = label
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    if s.trim().is_empty() {
        s = "export".to_string();
    }
    if !s.to_ascii_lowercase().ends_with(".fcs") {
        s.push_str(".fcs");
    }
    s
}

/// Cells per file, files in sorted order of their label, cells in crosstab order. Without a
/// file factor every cell goes to one file.
pub fn group_files(labels: Option<&[String]>, n: usize, single: &str) -> Vec<(String, Vec<usize>)> {
    let Some(labels) = labels else {
        return vec![(file_name(single), (0..n).collect())];
    };
    let mut keys: Vec<&String> = labels.iter().collect();
    keys.sort();
    keys.dedup();
    let mut out: Vec<(String, Vec<usize>)> =
        keys.iter().map(|k| (file_name(k), Vec::new())).collect();
    for (i, l) in labels.iter().enumerate() {
        let k = keys.binary_search(&l).unwrap();
        out[k].1.push(i);
    }
    // Two labels can sanitise to the same name ("a/b" and "a_b"): number the later ones.
    let mut seen = std::collections::HashSet::new();
    for (name, _) in out.iter_mut() {
        let base = name.trim_end_matches(".fcs").to_string();
        let mut k = 1;
        while !seen.insert(name.to_ascii_lowercase()) {
            k += 1;
            *name = format!("{base} ({k}).fcs");
        }
    }
    out
}

/// Everything a file needs besides its cells.
pub struct Export<'a> {
    /// `$PnN` of each channel (the first row factor).
    pub channels: &'a [String],
    /// `$PnS` of each channel (the second row factor), when projected.
    pub descs: Option<&'a [String]>,
    /// The values, column-major: `n` cells per channel.
    pub data: &'a [f32],
    pub n: usize,
    pub annotations: &'a [Annotation],
    pub dtype: DataType,
}

impl Export<'_> {
    fn params(&self) -> Result<Vec<Param>> {
        let p = self.channels.len();
        let mut out: Vec<Param> = self
            .channels
            .iter()
            .enumerate()
            .map(|(j, c)| Param {
                name: c.replace(',', "_"),
                desc: self
                    .descs
                    .map(|d| d[j].clone())
                    .filter(|d| !d.is_empty() && d != c),
                extra: vec![],
            })
            .collect();
        for (k, a) in self.annotations.iter().enumerate() {
            let i = p + k + 1;
            let extra = match &a.levels {
                None => vec![],
                Some(lv) => vec![(
                    format!("TERCEN_P{i}_LEVELS"),
                    lv.iter()
                        .enumerate()
                        .map(|(c, l)| format!("{}={l}", c + 1))
                        .collect::<Vec<_>>()
                        .join("; "),
                )],
            };
            out.push(Param {
                name: a.name.replace(',', "_"),
                desc: Some(a.name.clone()),
                extra,
            });
        }
        let mut names: Vec<String> = out.iter().map(|q| q.name.to_ascii_lowercase()).collect();
        names.sort();
        if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
            bail!(
                "two parameters would both be called '{}' in the FCS files; rename one factor",
                w[0]
            );
        }
        Ok(out)
    }

    /// One FCS file of the given cells.
    pub fn file_bytes(&self, params: &[Param], name: &str, cells: &[usize]) -> Result<Vec<u8>> {
        let p = self.channels.len();
        let width = params.len();
        let mut rows = Vec::with_capacity(cells.len() * width);
        for &c in cells {
            for j in 0..p {
                rows.push(self.data[c + j * self.n] as f64);
            }
            for a in self.annotations {
                rows.push(a.values[c]);
            }
        }
        fcs::encode(params, &rows, self.dtype, name, &[])
    }

    /// The legend of every text factor, as CSV: parameter, code, value.
    pub fn levels_csv(&self) -> Option<String> {
        let mut s = String::from("parameter,code,value\n");
        let mut any = false;
        for a in self.annotations {
            if let Some(lv) = &a.levels {
                any = true;
                for (c, l) in lv.iter().enumerate() {
                    s.push_str(&format!("{},{},{}\n", csv(&a.name), c + 1, csv(l)));
                }
            }
        }
        any.then_some(s)
    }

    /// Write every file into a ZIP on `out`, building and deflating `batch` files at a time in
    /// parallel. `progress(done, total)` is called after each batch. Returns (files, raw bytes).
    pub fn write_zip<W: Write>(
        &self,
        files: &[(String, Vec<usize>)],
        out: W,
        batch: usize,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<(usize, u64)> {
        let params = self.params()?;
        let mut z = ZipWriter::new(out);
        let mut raw = 0u64;
        let mut done = 0;
        for chunk in files.chunks(batch.max(1)) {
            let entries: Vec<Result<Entry>> = chunk
                .par_iter()
                .map(|(name, cells)| {
                    Entry::deflate(name.clone(), &self.file_bytes(&params, name, cells)?)
                })
                .collect();
            for e in entries {
                let e = e?;
                raw += e.size;
                z.add(&e)?;
            }
            done += chunk.len();
            progress(done, files.len());
        }
        if let Some(csv) = self.levels_csv() {
            z.add(&Entry::deflate("levels.csv".into(), csv.as_bytes())?)?;
        }
        z.finish()?;
        Ok((files.len(), raw))
    }
}

fn csv(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn text_becomes_sorted_codes_and_numbers_stay() {
        let a = annotation(
            "pop",
            Values::Text(vec!["T".into(), "B".into(), "T".into()]),
            DataType::F,
        )
        .unwrap();
        assert_eq!(a.values, vec![2.0, 1.0, 2.0]);
        assert_eq!(a.levels.unwrap(), vec!["B", "T"]);
        let u = annotation("umap.1", Values::Num(vec![-3.25, 7.5]), DataType::F).unwrap();
        assert_eq!(u.values, vec![-3.25, 7.5], "a coordinate is not a rank");
        assert!(annotation("id", Values::Num(vec![20_000_001.0]), DataType::F).is_err());
        assert!(annotation("id", Values::Num(vec![20_000_001.0]), DataType::D).is_ok());
        assert!(annotation("x", Values::Num(vec![f64::NAN]), DataType::F).is_err());
    }

    #[test]
    fn files_are_grouped_named_and_deduplicated() {
        let l: Vec<String> = ["b", "a/b", "a_b.fcs", "b"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let g = group_files(Some(&l), 4, "x");
        let names: Vec<&str> = g.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["a_b.fcs", "a_b (2).fcs", "b.fcs"]);
        assert_eq!(g[2].1, vec![0, 3]);
        assert_eq!(
            group_files(None, 3, "Tercen")[0],
            ("Tercen.fcs".to_string(), vec![0, 1, 2])
        );
    }

    /// The whole path: values per file in the right place, legend in keywords and levels.csv.
    #[test]
    fn zip_holds_each_files_cells() {
        let (n, channels) = (5usize, vec!["FL1-A".to_string(), "FL2-A".to_string()]);
        let descs = vec!["CD3".to_string(), "CD4".to_string()];
        // value of cell c, channel j = 10*j + c
        let data: Vec<f32> = (0..2)
            .flat_map(|j| (0..n).map(move |c| (10 * j + c) as f32))
            .collect();
        let ann = vec![
            annotation(
                "umap.1",
                Values::Num(vec![0.5, 1.5, 2.5, 3.5, 4.5]),
                DataType::F,
            )
            .unwrap(),
            annotation(
                "pop",
                Values::Text(vec![
                    "T".into(),
                    "B".into(),
                    "T".into(),
                    "NK".into(),
                    "B".into(),
                ]),
                DataType::F,
            )
            .unwrap(),
        ];
        let e = Export {
            channels: &channels,
            descs: Some(&descs),
            data: &data,
            n,
            annotations: &ann,
            dtype: DataType::F,
        };
        let files = group_files(
            Some(&["s1", "s2", "s1", "s2", "s1"].map(String::from)),
            n,
            "x",
        );
        let mut buf = Vec::new();
        let (nf, _) = e.write_zip(&files, &mut buf, 2, |_, _| {}).unwrap();
        assert_eq!(nf, 2);
        let mut ar = zip::ZipArchive::new(std::io::Cursor::new(buf)).unwrap();
        let mut b = Vec::new();
        ar.by_name("s2.fcs").unwrap().read_to_end(&mut b).unwrap();
        let f = fcs::read(&b).unwrap();
        assert_eq!((f.n_events, f.n_params), (2, 4));
        // s2 holds cells 1 and 3: FL1 = c, FL2 = 10 + c, umap, pop code (B=1, NK=2, T=3)
        assert_eq!(f.data, vec![1.0, 11.0, 1.5, 1.0, 3.0, 13.0, 3.5, 2.0]);
        assert_eq!(f.get("$P1S"), Some("CD3"));
        assert_eq!(f.get("$P4N"), Some("pop"));
        assert_eq!(f.get("TERCEN_P4_LEVELS"), Some("1=B; 2=NK; 3=T"));
        let mut s = String::new();
        ar.by_name("levels.csv")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert_eq!(s, "parameter,code,value\npop,1,B\npop,2,NK\npop,3,T\n");
    }
}
