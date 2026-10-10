//! An FCS 3.1 writer, and the small reader the tests check it with.
//!
//! One data set per file: HEADER, TEXT, DATA, list mode, every parameter 32-bit float
//! (`$DATATYPE F`) or 64-bit (`D`), little-endian (`$BYTEORD 1,2,3,4`). What a reader needs and
//! nothing that varies run to run (no `$DATE`), so the same data gives the same bytes.
//!
//! Two details are easy to get wrong:
//! * **Offsets past 99,999,999 do not fit the header's 8-character fields.** FCS 3.1 then writes
//!   0 there and the reader takes `$BEGINDATA`/`$ENDDATA` from TEXT, which is always written.
//! * **TEXT holds its own data offset**, so its length depends on the number it contains. The
//!   offsets are computed until they stop changing (two rounds at most: a digit more moves the
//!   data by one byte).
use anyhow::{Result, bail};

/// `$DATATYPE`: F (32-bit float) or D (64-bit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    F,
    D,
}

impl DataType {
    pub fn bytes(self) -> usize {
        match self {
            DataType::F => 4,
            DataType::D => 8,
        }
    }
    pub fn keyword(self) -> &'static str {
        match self {
            DataType::F => "F",
            DataType::D => "D",
        }
    }
}

/// One parameter (channel) of the file.
#[derive(Debug, Clone)]
pub struct Param {
    /// `$PnN`, the short name; FCS 3.1 forbids commas in it.
    pub name: String,
    /// `$PnS`, the long name (the marker), when there is one.
    pub desc: Option<String>,
    /// Extra keywords for this parameter, written as given (e.g. a level legend).
    pub extra: Vec<(String, String)>,
}

const DELIM: char = '|';
const HEADER_LEN: usize = 58;

/// Escape a keyword value: the delimiter is doubled; a value may not be empty, so an empty one
/// becomes a single space.
fn escape(v: &str) -> String {
    if v.is_empty() {
        return " ".to_string();
    }
    v.replace(DELIM, "||")
}

fn fmt_num(v: f64) -> String {
    if v.is_finite() && v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Encode one FCS file. `data` is row-major: event after event, each with every parameter's
/// value in `params` order. `file_name` goes in `$FIL`; `keywords` are added to TEXT as given.
pub fn encode(
    params: &[Param],
    data: &[f64],
    dtype: DataType,
    file_name: &str,
    keywords: &[(String, String)],
) -> Result<Vec<u8>> {
    let p = params.len();
    if p == 0 {
        bail!("an FCS file needs at least one parameter");
    }
    if !data.len().is_multiple_of(p) {
        bail!(
            "{} values do not split into events of {p} parameters",
            data.len()
        );
    }
    let n = data.len() / p;
    for prm in params {
        if prm.name.contains(',') {
            bail!(
                "parameter name '{}' contains a comma, which FCS 3.1 forbids in $PnN",
                prm.name
            );
        }
    }

    // Per-parameter range, for $PnR and the flowCore range keywords.
    let mut min = vec![f64::INFINITY; p];
    let mut max = vec![f64::NEG_INFINITY; p];
    for ev in data.chunks_exact(p) {
        for (j, v) in ev.iter().enumerate() {
            if *v < min[j] {
                min[j] = *v;
            }
            if *v > max[j] {
                max[j] = *v;
            }
        }
    }
    if n == 0 {
        min.fill(0.0);
        max.fill(0.0);
    }

    // Everything but the data offsets, which depend on TEXT's own length.
    let mut kw: Vec<(String, String)> = vec![
        ("$BEGINANALYSIS".into(), "0".into()),
        ("$ENDANALYSIS".into(), "0".into()),
        ("$BEGINSTEXT".into(), "0".into()),
        ("$ENDSTEXT".into(), "0".into()),
        ("$BYTEORD".into(), "1,2,3,4".into()),
        ("$DATATYPE".into(), dtype.keyword().into()),
        ("$MODE".into(), "L".into()),
        ("$NEXTDATA".into(), "0".into()),
        ("$PAR".into(), p.to_string()),
        ("$TOT".into(), n.to_string()),
        ("$FIL".into(), file_name.to_string()),
        ("$CYT".into(), "Tercen".into()),
    ];
    for (j, prm) in params.iter().enumerate() {
        let i = j + 1;
        kw.push((format!("$P{i}N"), prm.name.clone()));
        if let Some(d) = &prm.desc {
            kw.push((format!("$P{i}S"), d.clone()));
        }
        kw.push((format!("$P{i}B"), (dtype.bytes() * 8).to_string()));
        kw.push((format!("$P{i}E"), "0,0".into()));
        // $PnR: the upper end of the parameter's range, an integer by convention.
        kw.push((format!("$P{i}R"), fmt_num(max[j].ceil().max(1.0))));
        // flowCore reads these back as the parameter's exact range.
        kw.push((format!("flowCore_$P{i}Rmin"), fmt_num(min[j])));
        kw.push((format!("flowCore_$P{i}Rmax"), fmt_num(max[j])));
        kw.extend(prm.extra.iter().cloned());
    }
    kw.extend(keywords.iter().cloned());

    let body: String = kw
        .iter()
        .map(|(k, v)| format!("{}{DELIM}{}{DELIM}", escape(k), escape(v)))
        .collect();
    let data_len = n * p * dtype.bytes();

    // TEXT = delimiter + $BEGINDATA + $ENDDATA + the rest. Iterate the offsets to a fixed point.
    let mut begin = 0usize;
    let text = loop {
        let end = if data_len == 0 {
            begin
        } else {
            begin + data_len - 1
        };
        let t = format!("{DELIM}$BEGINDATA{DELIM}{begin}{DELIM}$ENDDATA{DELIM}{end}{DELIM}{body}");
        let next = HEADER_LEN + t.len();
        if next == begin {
            break t;
        }
        begin = next;
    };
    let text_end = HEADER_LEN + text.len() - 1;
    let data_end = if data_len == 0 {
        begin
    } else {
        begin + data_len - 1
    };
    let field = |v: usize| -> String {
        if v > 99_999_999 {
            format!("{:>8}", 0)
        } else {
            format!("{v:>8}")
        }
    };
    let (hb, he) = if data_end > 99_999_999 {
        (0, 0)
    } else {
        (begin, data_end)
    };
    let header = format!(
        "FCS3.1    {}{}{}{}{}{}",
        field(HEADER_LEN),
        field(text_end),
        if hb == 0 { field(0) } else { field(hb) },
        if he == 0 { field(0) } else { field(he) },
        field(0),
        field(0)
    );
    debug_assert_eq!(header.len(), HEADER_LEN);

    let mut out = Vec::with_capacity(HEADER_LEN + text.len() + data_len);
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(text.as_bytes());
    match dtype {
        DataType::F => {
            for v in data {
                out.extend_from_slice(&(*v as f32).to_le_bytes());
            }
        }
        DataType::D => {
            for v in data {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    Ok(out)
}

/// What [`read`] returns: TEXT's keywords in order, and the data row-major as f64.
#[derive(Debug)]
pub struct Parsed {
    pub keywords: Vec<(String, String)>,
    pub n_events: usize,
    pub n_params: usize,
    pub data: Vec<f64>,
}

impl Parsed {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.keywords
            .iter()
            .find(|(kk, _)| kk.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.as_str())
    }
}

/// A minimal FCS reader (list mode, F or D, either byte order), for the tests.
pub fn read(b: &[u8]) -> Result<Parsed> {
    if b.len() < HEADER_LEN || !b.starts_with(b"FCS3") {
        bail!("not an FCS 3.x file");
    }
    let num = |s: &[u8]| -> Result<usize> {
        let t = std::str::from_utf8(s)?.trim();
        Ok(if t.is_empty() { 0 } else { t.parse()? })
    };
    let ts = num(&b[10..18])?;
    let te = num(&b[18..26])?;
    let text = std::str::from_utf8(&b[ts..=te])?;
    let delim = text.chars().next().unwrap();
    // Split on single delimiters, a doubled one being an escaped character.
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut it = text[1..].chars().peekable();
    while let Some(c) = it.next() {
        if c == delim {
            if it.peek() == Some(&delim) {
                cur.push(delim);
                it.next();
            } else {
                fields.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    let keywords: Vec<(String, String)> = fields
        .chunks_exact(2)
        .map(|kv| (kv[0].clone(), kv[1].clone()))
        .collect();
    let p = Parsed {
        keywords,
        n_events: 0,
        n_params: 0,
        data: vec![],
    };
    let n: usize = p.get("$TOT").unwrap_or("0").parse()?;
    let par: usize = p.get("$PAR").unwrap_or("0").parse()?;
    let begin: usize = p.get("$BEGINDATA").unwrap_or("0").trim().parse()?;
    let dtype = p.get("$DATATYPE").unwrap_or("F");
    let little = p.get("$BYTEORD").unwrap_or("1,2,3,4").starts_with('1');
    let width = if dtype == "D" { 8 } else { 4 };
    let raw = &b[begin..begin + n * par * width];
    let data: Vec<f64> = raw
        .chunks_exact(width)
        .map(|c| match (width, little) {
            (4, true) => f32::from_le_bytes(c.try_into().unwrap()) as f64,
            (4, false) => f32::from_be_bytes(c.try_into().unwrap()) as f64,
            (_, true) => f64::from_le_bytes(c.try_into().unwrap()),
            (_, false) => f64::from_be_bytes(c.try_into().unwrap()),
        })
        .collect();
    Ok(Parsed {
        n_events: n,
        n_params: par,
        data,
        ..p
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(names: &[&str]) -> Vec<Param> {
        names
            .iter()
            .map(|n| Param {
                name: n.to_string(),
                desc: None,
                extra: vec![],
            })
            .collect()
    }

    #[test]
    fn round_trip_values_and_keywords() {
        let mut ps = params(&["FSC-A", "CD3"]);
        ps[1].desc = Some("CD3 | BUV395".into()); // a delimiter inside a value
        let data = [1.5, -2.25, 3.0, 4.75, 1e6, 0.0];
        let b = encode(
            &ps,
            &data,
            DataType::F,
            "a.fcs",
            &[("TERCEN_X".into(), "y".into())],
        )
        .unwrap();
        let r = read(&b).unwrap();
        assert_eq!((r.n_events, r.n_params), (3, 2));
        assert_eq!(r.data, data.to_vec());
        assert_eq!(r.get("$P2S"), Some("CD3 | BUV395"));
        assert_eq!(r.get("$P1N"), Some("FSC-A"));
        assert_eq!(r.get("$FIL"), Some("a.fcs"));
        assert_eq!(r.get("TERCEN_X"), Some("y"));
        assert_eq!(r.get("$P2R"), Some("5"));
        assert_eq!(r.get("flowCore_$P2Rmin"), Some("-2.25"));
        // The header's data offsets agree with TEXT's.
        let h = std::str::from_utf8(&b[..58]).unwrap();
        assert!(h.starts_with("FCS3.1"));
        let begin: usize = h[26..34].trim().parse().unwrap();
        assert_eq!(begin.to_string(), r.get("$BEGINDATA").unwrap());
        assert_eq!(b.len(), begin + 3 * 2 * 4);
    }

    #[test]
    fn doubles_keep_large_integers() {
        let data = [16_777_217.0, 1.0];
        let r =
            read(&encode(&params(&["id", "x"]), &data, DataType::D, "f", &[]).unwrap()).unwrap();
        assert_eq!(r.data[0], 16_777_217.0);
        let r =
            read(&encode(&params(&["id", "x"]), &data, DataType::F, "f", &[]).unwrap()).unwrap();
        assert_eq!(
            r.data[0], 16_777_216.0,
            "float32 cannot hold it, which is why D exists"
        );
    }

    #[test]
    fn same_input_same_bytes() {
        let data: Vec<f64> = (0..300).map(|i| i as f64 * 0.37).collect();
        let a = encode(&params(&["a", "b", "c"]), &data, DataType::F, "x", &[]).unwrap();
        let b = encode(&params(&["a", "b", "c"]), &data, DataType::F, "x", &[]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn commas_in_names_are_refused() {
        assert!(encode(&params(&["a,b"]), &[1.0], DataType::F, "x", &[]).is_err());
    }
}
