//! Format-aware export of a query result stream. Unified across engines: it
//! consumes a `SendableRecordBatchStream` (local `execute_stream` or a Flight
//! re-fetch) and writes Parquet / CSV / JSON with the chosen settings.
//!
//! Parquet carries full compression control; CSV/JSON expose header / delimiter
//! / ndjson. (File-level gzip for CSV/JSON is a deliberate follow-up — it would
//! pull in a new compression dependency.)

use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;

use datafusion::arrow::csv::WriterBuilder as CsvWriterBuilder;
use datafusion::arrow::json::{ArrayWriter, LineDelimitedWriter};
use datafusion::physical_plan::SendableRecordBatchStream;
use futures::StreamExt;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use parquet::schema::types::ColumnPath;

use crate::error::ExportError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Parquet,
    Csv,
    Json,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 3] =
        [ExportFormat::Parquet, ExportFormat::Csv, ExportFormat::Json];

    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Parquet => "Parquet",
            ExportFormat::Csv => "CSV",
            ExportFormat::Json => "JSON",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Parquet => "parquet",
            ExportFormat::Csv => "csv",
            ExportFormat::Json => "json",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParquetCompression {
    None,
    Snappy,
    Gzip,
    Zstd,
    Lz4,
}
impl ParquetCompression {
    pub const ALL: [ParquetCompression; 5] = [
        ParquetCompression::None,
        ParquetCompression::Snappy,
        ParquetCompression::Gzip,
        ParquetCompression::Zstd,
        ParquetCompression::Lz4,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ParquetCompression::None => "None",
            ParquetCompression::Snappy => "Snappy",
            ParquetCompression::Gzip => "Gzip",
            ParquetCompression::Zstd => "Zstd",
            ParquetCompression::Lz4 => "LZ4",
        }
    }

    fn to_parquet(&self) -> Compression {
        match self {
            ParquetCompression::None => Compression::UNCOMPRESSED,
            ParquetCompression::Snappy => Compression::SNAPPY,
            ParquetCompression::Gzip => Compression::GZIP(Default::default()),
            ParquetCompression::Zstd => Compression::ZSTD(Default::default()),
            ParquetCompression::Lz4 => Compression::LZ4_RAW,
        }
    }
}

pub trait ExportOptions {
    fn format() -> ExportFormat;

    /// Drain `stream` into `path` in the chosen format. Writers are synchronous and
    /// driven incrementally as batches arrive, so memory stays bounded for the
    /// local engine (Flight pre-buffers, see `run_sql_stream`).
    async fn write_stream(
        &self,
        stream: SendableRecordBatchStream,
        path: PathBuf,
    ) -> Result<PathBuf, ExportError>;
}

#[derive(Debug, Clone)]
pub struct ParquetOptions {
    pub compression: ParquetCompression,
    pub encoding: Option<parquet::basic::Encoding>,
    pub dictionary_enabled: bool,
    pub per_column_options: HashMap<String, ParquetColumnOptions>,
}
impl Default for ParquetOptions {
    fn default() -> Self {
        Self {
            compression: ParquetCompression::Zstd,
            encoding: None,
            dictionary_enabled: true,
            per_column_options: HashMap::new(),
        }
    }
}
impl ExportOptions for ParquetOptions {
    fn format() -> ExportFormat {
        ExportFormat::Parquet
    }

    async fn write_stream(
        &self,
        mut stream: SendableRecordBatchStream,
        path: PathBuf,
    ) -> Result<PathBuf, ExportError> {
        let schema = stream.schema();
        let file = File::create(&path).map_err(|e| ExportError::CreateFile(e.to_string()))?;
        tracing::info!(dest = %path.display(), format = ?Self::format(), "exporting query result");

        let write = |op: &'static str, e: &dyn std::fmt::Display| ExportError::Write {
            op,
            msg: e.to_string(),
        };

        let mut props = WriterProperties::builder()
            .set_compression(self.compression.to_parquet())
            .set_dictionary_enabled(self.dictionary_enabled);
        if let Some(encoding) = self.encoding {
            props = props.set_encoding(encoding);
        }

        for (col, co) in self.per_column_options.iter() {
            let col = ColumnPath::from(col.clone());
            if let Some(value) = co.compression {
                props = props.set_column_compression(col.clone(), value.to_parquet());
            }
            if let Some(value) = co.dictionary_enabled {
                props = props.set_column_dictionary_enabled(col.clone(), value);
            }
            if let Some(value) = co.encoding {
                props = props.set_column_encoding(col.clone(), value);
            }
        }

        let mut writer = ArrowWriter::try_new(file, schema, Some(props.build()))
            .map_err(|e| write("open parquet writer", &e))?;

        while let Some(batch) = stream.next().await {
            let batch = batch.map_err(|e| write("read batch", &e))?;
            writer
                .write(&batch)
                .map_err(|e| write("write parquet", &e))?;
        }
        writer.close().map_err(|e| write("finish parquet", &e))?;

        tracing::info!(dest = %path.display(), "export complete");
        Ok(path)
    }
}

#[derive(Debug, Clone)]
pub struct ParquetColumnOptions {
    pub compression: Option<ParquetCompression>,
    pub encoding: Option<parquet::basic::Encoding>,
    pub dictionary_enabled: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct CsvOptions {
    pub header: bool,
    pub delimiter: u8,
}
impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            header: true,
            delimiter: b',',
        }
    }
}
impl ExportOptions for CsvOptions {
    fn format() -> ExportFormat {
        ExportFormat::Csv
    }

    async fn write_stream(
        &self,
        mut stream: SendableRecordBatchStream,
        path: PathBuf,
    ) -> Result<PathBuf, ExportError> {
        let file = File::create(&path).map_err(|e| ExportError::CreateFile(e.to_string()))?;
        tracing::info!(dest = %path.display(), format = ?Self::format(), "exporting query result");

        let write = |op: &'static str, e: &dyn std::fmt::Display| ExportError::Write {
            op,
            msg: e.to_string(),
        };

        let mut writer = CsvWriterBuilder::new()
            .with_header(self.header)
            .with_delimiter(self.delimiter)
            .build(file);

        while let Some(batch) = stream.next().await {
            let batch = batch.map_err(|e| write("read batch", &e))?;
            writer.write(&batch).map_err(|e| write("write csv", &e))?;
        }

        tracing::info!(dest = %path.display(), "export complete");
        Ok(path)
    }
}

#[derive(Debug, Clone)]
pub struct JsonOptions {
    /// JSON: newline-delimited (one object per line) vs a single JSON array.
    pub ndjson: bool,
}
impl Default for JsonOptions {
    fn default() -> Self {
        Self { ndjson: true }
    }
}
impl ExportOptions for JsonOptions {
    fn format() -> ExportFormat {
        ExportFormat::Json
    }

    async fn write_stream(
        &self,
        mut stream: SendableRecordBatchStream,
        path: PathBuf,
    ) -> Result<PathBuf, ExportError> {
        let file = File::create(&path).map_err(|e| ExportError::CreateFile(e.to_string()))?;
        tracing::info!(dest = %path.display(), format = ?Self::format(), "exporting query result");

        let write = |op: &'static str, e: &dyn std::fmt::Display| ExportError::Write {
            op,
            msg: e.to_string(),
        };

        if self.ndjson {
            let mut writer = LineDelimitedWriter::new(file);
            while let Some(batch) = stream.next().await {
                let batch = batch.map_err(|e| write("read batch", &e))?;
                writer.write(&batch).map_err(|e| write("write json", &e))?;
            }
            writer.finish().map_err(|e| write("finish json", &e))?;
        } else {
            let mut writer = ArrayWriter::new(file);
            while let Some(batch) = stream.next().await {
                let batch = batch.map_err(|e| write("read batch", &e))?;
                writer.write(&batch).map_err(|e| write("write json", &e))?;
            }
            writer.finish().map_err(|e| write("finish json", &e))?;
        }

        tracing::info!(dest = %path.display(), "export complete");
        Ok(path)
    }
}
