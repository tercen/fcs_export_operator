//! The result: one table, one row — the ZIP as a file (`filename`, `mimetype`, `.content`),
//! joined to the crosstab on nothing, as a file or image a step returns. The same shape the R
//! operator wrote (`as_join_operator(list(), list())`) and `read_fcs` writes for its summary.
//!
//! `.content` is the ZIP in base64, a third larger than the ZIP and a few hundred MB on a
//! cohort, so it is streamed from the ZIP on disk into the result file, never held.
use std::io::{Read, Write};

use anyhow::{Result, anyhow};
use base64::Engine;

use crate::tson::TsonWriter;

pub const TABLE: &str = "fcs_export";
pub const MIMETYPE: &str = "application/zip";

fn column_header<W: Write>(w: &mut TsonWriter<W>, name: &str) -> Result<()> {
    w.map(6)?;
    w.key("kind")?;
    w.str("Column")?;
    w.key("name")?;
    w.str(name)?;
    w.key("type")?;
    w.str("string")?;
    w.key("nRows")?;
    w.i32(1)?;
    w.key("size")?;
    w.i32(1)?;
    w.key("values")?;
    Ok(())
}

/// Write the whole `OperatorResult`. `zip` is read once, `zip_len` bytes.
pub fn write_result<W: Write, R: Read>(
    w: &mut TsonWriter<W>,
    namespace: &str,
    zip_name: &str,
    mut zip: R,
    zip_len: u64,
) -> Result<()> {
    w.map(3)?;
    w.key("kind")?;
    w.str("OperatorResult")?;
    w.key("tables")?;
    w.list(1)?;

    w.map(4)?;
    w.key("kind")?;
    w.str("Table")?;
    w.key("nRows")?;
    w.i32(1)?;
    w.key("properties")?;
    w.map(4)?;
    w.key("kind")?;
    w.str("TableProperties")?;
    w.key("name")?;
    w.str(TABLE)?;
    w.key("sortOrder")?;
    w.list(0)?;
    w.key("ascending")?;
    w.bool(false)?;
    w.key("columns")?;
    w.list(3)?;
    column_header(w, &format!("{namespace}.filename"))?;
    w.str_list(&[zip_name])?;
    column_header(w, &format!("{namespace}.mimetype"))?;
    w.str_list(&[MIMETYPE])?;
    column_header(w, ".content")?;
    let b64_len =
        usize::try_from(zip_len.div_ceil(3) * 4).map_err(|_| anyhow!("export too large"))?;
    w.str_list_one_begin(b64_len)?;
    // Whole 3-byte groups per piece, so the pieces' base64 concatenates to the file's.
    let mut buf = vec![0u8; 3 << 20];
    let mut written = 0usize;
    loop {
        let mut got = 0;
        while got < buf.len() {
            let k = zip.read(&mut buf[got..])?;
            if k == 0 {
                break;
            }
            got += k;
        }
        if got == 0 {
            break;
        }
        let s = base64::engine::general_purpose::STANDARD.encode(&buf[..got]);
        written += s.len();
        w.raw(s.as_bytes())?;
        if got < buf.len() {
            break;
        }
    }
    if written != b64_len {
        return Err(anyhow!(
            "wrote {written} bytes of base64 where {b64_len} were declared"
        ));
    }
    w.str_list_one_end()?;

    // Joined on nothing: a file beside the crosstab, not a value per cell.
    w.key("joinOperators")?;
    w.list(1)?;
    w.map(4)?;
    w.key("kind")?;
    w.str("JoinOperator")?;
    w.key("joinType")?;
    w.str("")?;
    w.key("leftPair")?;
    w.map(3)?;
    w.key("kind")?;
    w.str("ColumnPair")?;
    w.key("lColumns")?;
    w.list(0)?;
    w.key("rColumns")?;
    w.list(0)?;
    w.key("rightRelation")?;
    w.map(3)?;
    w.key("kind")?;
    w.str("SimpleRelation")?;
    w.key("id")?;
    w.str(TABLE)?;
    w.key("index")?;
    w.i32(0)?;
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The streamed result decodes with rustson to the right table, and the base64 decodes back
    /// to the bytes — for lengths on and off the 3-byte boundary and across pieces.
    #[test]
    fn result_round_trips() {
        for len in [0usize, 1, 2, 3, 4, (3 << 20) + 1, 7_000_000] {
            let zip: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
            let mut out = Vec::new();
            {
                let mut w = TsonWriter::new(&mut out).unwrap();
                write_result(&mut w, "ds9", "x.zip", &zip[..], zip.len() as u64).unwrap();
            }
            let v = crate::input::documents(&out).unwrap().remove(0);
            let rustson::Value::MAP(m) = v else { panic!() };
            let rustson::Value::LST(tables) = &m["tables"] else {
                panic!()
            };
            let rustson::Value::MAP(t) = &tables[0] else {
                panic!()
            };
            let rustson::Value::LST(cols) = &t["columns"] else {
                panic!()
            };
            let rustson::Value::MAP(c) = &cols[2] else {
                panic!()
            };
            let rustson::Value::LSTSTR(s) = &c["values"] else {
                panic!()
            };
            let s = s.try_to_vec().unwrap();
            assert_eq!(s.len(), 1);
            let back = base64::engine::general_purpose::STANDARD
                .decode(&s[0])
                .unwrap();
            assert_eq!(back, zip, "len {len}");
            let rustson::Value::MAP(c0) = &cols[0] else {
                panic!()
            };
            assert_eq!(c0["name"], rustson::Value::STR("ds9.filename".into()));
        }
    }
}
