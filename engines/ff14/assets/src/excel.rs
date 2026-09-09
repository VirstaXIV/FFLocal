//! Excel (EXH/EXD) sheet access with a name → column map derived from EXDSchema.
//!
//! Physis exposes columns positionally. EXDSchema lists fields in row-offset order, so
//! sorting the EXH column definitions by `(offset, data_type)` and zipping them with the
//! schema's expanded field list yields a stable name → index map. Column drift between
//! game patches shows up as a count mismatch, which is logged loudly and can be inspected
//! with `ffl-cli excel <Sheet> <row> --schema`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use physis::Language;
use physis::excel::{Field, Row, Sheet};
use physis::exh::EXH;

use crate::schema_data;
use crate::source::SqPackSource;

/// Name → column index map for one sheet.
#[derive(Debug, Clone)]
pub struct SchemaMap {
    /// Schema field name per Physis column index (empty string when unknown).
    pub names: Vec<String>,
    by_name: HashMap<String, usize>,
    /// Non-fatal problems found while building the map.
    pub warnings: Vec<String>,
}

impl SchemaMap {
    pub fn build(exh: &EXH, schema: &[&str]) -> Self {
        // Physis column index → (offset, type) ; sort a permutation by (offset, type).
        let mut order: Vec<usize> = (0..exh.column_definitions.len()).collect();
        order.sort_by_key(|&i| {
            let c = exh.column_definitions[i];
            (c.offset, c.data_type as u16)
        });

        let mut names = vec![String::new(); exh.column_definitions.len()];
        let mut by_name = HashMap::new();
        let mut warnings = Vec::new();
        if schema.len() != order.len() {
            warnings.push(format!(
                "schema has {} fields but the sheet has {} columns; names after the shorter list are unmapped",
                schema.len(),
                order.len()
            ));
        }
        for (rank, &col) in order.iter().enumerate() {
            if let Some(name) = schema.get(rank) {
                names[col] = (*name).to_string();
                by_name.insert((*name).to_string(), col);
            }
        }
        Self {
            names,
            by_name,
            warnings,
        }
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }
}

/// A sheet loaded for one language, with its schema map when one is embedded.
pub struct LoadedSheet {
    pub name: String,
    pub language: Language,
    pub sheet: Sheet,
    pub schema: Option<SchemaMap>,
}

impl LoadedSheet {
    pub fn row(&self, row_id: u32) -> Option<&Row> {
        self.sheet.row(row_id)
    }

    /// Column index for a schema field name.
    pub fn column(&self, name: &str) -> Result<usize> {
        self.schema
            .as_ref()
            .ok_or_else(|| anyhow!("no embedded schema for sheet {}", self.name))?
            .index(name)
            .ok_or_else(|| anyhow!("sheet {} has no field named {name}", self.name))
    }

    /// Field by row id and schema name.
    pub fn field(&self, row_id: u32, name: &str) -> Result<&Field> {
        let col = self.column(name)?;
        let row = self
            .row(row_id)
            .ok_or_else(|| anyhow!("sheet {} has no row {row_id}", self.name))?;
        row.columns
            .get(col)
            .ok_or_else(|| anyhow!("row {row_id} of {} has no column {col}", self.name))
    }

    pub fn string(&self, row_id: u32, name: &str) -> Result<String> {
        let f = self.field(row_id, name)?;
        f.into_string()
            .cloned()
            .ok_or_else(|| anyhow!("{}.{name} is not a string: {f:?}", self.name))
    }

    /// Any integer-like field widened to u64 (bools become 0/1).
    pub fn integer(&self, row_id: u32, name: &str) -> Result<u64> {
        let f = self.field(row_id, name)?;
        field_as_u64(f).ok_or_else(|| anyhow!("{}.{name} is not an integer: {f:?}", self.name))
    }

    pub fn signed(&self, row_id: u32, name: &str) -> Result<i64> {
        let f = self.field(row_id, name)?;
        field_as_i64(f).ok_or_else(|| anyhow!("{}.{name} is not an integer: {f:?}", self.name))
    }

    pub fn row_count(&self) -> u32 {
        self.sheet.exh.header.row_count
    }
}

pub fn field_as_u64(f: &Field) -> Option<u64> {
    Some(match f {
        Field::Bool(b) => *b as u64,
        Field::Int8(v) => *v as u64,
        Field::UInt8(v) => *v as u64,
        Field::Int16(v) => *v as u64,
        Field::UInt16(v) => *v as u64,
        Field::Int32(v) => *v as u64,
        Field::UInt32(v) => *v as u64,
        Field::Int64(v) => *v as u64,
        Field::UInt64(v) => *v,
        Field::String(_) | Field::Float32(_) => return None,
    })
}

pub fn field_as_i64(f: &Field) -> Option<i64> {
    Some(match f {
        Field::Bool(b) => *b as i64,
        Field::Int8(v) => *v as i64,
        Field::UInt8(v) => *v as i64,
        Field::Int16(v) => *v as i64,
        Field::UInt16(v) => *v as i64,
        Field::Int32(v) => *v as i64,
        Field::UInt32(v) => *v as i64,
        Field::Int64(v) => *v,
        Field::UInt64(v) => *v as i64,
        Field::String(_) | Field::Float32(_) => return None,
    })
}

/// Caches parsed sheets per (name, language).
pub struct ExcelCache {
    source: Arc<SqPackSource>,
    sheets: Mutex<HashMap<(String, u8), Arc<LoadedSheet>>>,
    preferred: Language,
}

impl ExcelCache {
    pub fn new(source: Arc<SqPackSource>) -> Self {
        Self {
            source,
            sheets: Mutex::new(HashMap::new()),
            preferred: Language::English,
        }
    }

    pub fn header(&self, name: &str) -> Result<EXH> {
        self.source
            .with_resource(|r| r.read_excel_sheet_header(name))
            .map_err(|e| anyhow!("reading {name}.exh: {e:?}"))
    }

    /// Pick the language to read: `None` for language-neutral sheets, else the preferred one.
    fn choose_language(&self, exh: &EXH) -> Language {
        // Physis reports padding bytes in the language list as `None`, so `None` only counts
        // when no real language is listed at all.
        if exh.languages.contains(&self.preferred) {
            self.preferred
        } else if let Some(l) = exh.languages.iter().find(|l| **l != Language::None) {
            *l
        } else {
            Language::None
        }
    }

    /// Load (or fetch from cache) a sheet in the best available language.
    pub fn sheet(&self, name: &str) -> Result<Arc<LoadedSheet>> {
        let exh = self.header(name)?;
        let language = self.choose_language(&exh);
        self.sheet_in(name, language, Some(exh))
    }

    pub fn sheet_in(
        &self,
        name: &str,
        language: Language,
        exh: Option<EXH>,
    ) -> Result<Arc<LoadedSheet>> {
        let key = (name.to_string(), language as u8);
        if let Some(s) = self.sheets.lock().expect("excel cache poisoned").get(&key) {
            return Ok(s.clone());
        }
        let exh = match exh {
            Some(e) => e,
            None => self.header(name)?,
        };
        let sheet = self
            .source
            .with_resource(|r| r.read_excel_sheet(&exh, name, language))
            .map_err(|e| anyhow!("reading sheet {name} ({language:?}): {e:?}"))?;
        let schema = schema_data::schema_for(name).map(|s| {
            let map = SchemaMap::build(&exh, s);
            for w in &map.warnings {
                tracing::warn!("schema {name}: {w}");
            }
            map
        });
        let loaded = Arc::new(LoadedSheet {
            name: name.to_string(),
            language,
            sheet,
            schema,
        });
        self.sheets
            .lock()
            .expect("excel cache poisoned")
            .insert(key, loaded.clone());
        Ok(loaded)
    }

    /// Convenience: one string field.
    pub fn string(&self, sheet: &str, row: u32, field: &str) -> Result<String> {
        self.sheet(sheet)?
            .string(row, field)
            .with_context(|| format!("{sheet}[{row}].{field}"))
    }
}
