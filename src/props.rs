//! Operator properties.
use anyhow::{Result, bail};
use tercen_rs::PropertyReader;
use tercen_rs::context::ContextBase;

use crate::fcs::DataType;

#[derive(Debug, Clone)]
pub struct Settings {
    /// `file_factor`: the column factor that splits cells into files. Empty: the column factor
    /// named `filename` (or ending in `.filename`) if there is one, else one file.
    pub file_factor: String,
    /// `datatype`: F (32-bit float, FlowJo's usual) or D (64-bit).
    pub dtype: DataType,
    /// `zip_name`: the archive's name.
    pub zip_name: String,
}

pub fn read(ctx: &ContextBase) -> Result<Settings> {
    let pr = PropertyReader::from_operator_settings(ctx.operator_settings());
    parse(|name| pr.get_string(name, ""))
}

pub fn parse(get: impl Fn(&str) -> String) -> Result<Settings> {
    let dtype = match get("datatype").trim().to_ascii_uppercase().as_str() {
        "" | "F" => DataType::F,
        "D" => DataType::D,
        other => bail!("datatype should be F or D, got '{other}'"),
    };
    let zip_name = match get("zip_name").trim() {
        "" => "Tercen_FCS_Export.zip".to_string(),
        z if z.to_ascii_lowercase().ends_with(".zip") => z.to_string(),
        z => format!("{z}.zip"),
    };
    Ok(Settings {
        file_factor: get("file_factor").trim().to_string(),
        dtype,
        zip_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_checks() {
        let s = parse(|_| String::new()).unwrap();
        assert_eq!(
            (s.dtype, s.zip_name.as_str(), s.file_factor.as_str()),
            (DataType::F, "Tercen_FCS_Export.zip", "")
        );
        let s = parse(|n| match n {
            "datatype" => "d".into(),
            "zip_name" => "lyme".into(),
            _ => String::new(),
        })
        .unwrap();
        assert_eq!((s.dtype, s.zip_name.as_str()), (DataType::D, "lyme.zip"));
        assert!(
            parse(|n| if n == "datatype" {
                "I".into()
            } else {
                String::new()
            })
            .is_err()
        );
    }
}
