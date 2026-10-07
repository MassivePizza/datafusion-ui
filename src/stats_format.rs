//! Rendering of parquet column-chunk statistics (min/max bounds).
//!
//! Statistics are stored using a column's *physical* type, so a `DECIMAL(18,2)` bound arrives as a
//! big-endian byte array and a `TIMESTAMP(MICROS)` bound as a bare `i64`. This module decodes each
//! bound using the column's logical type (falling back to the legacy converted type) so the Row
//! Groups view shows the value as it actually appears in the column.
//!
//! Timestamps are rendered from the parquet metadata alone: `isAdjustedToUTC` columns get a
//! trailing `Z`, others render as a naive datetime. No Arrow-schema lookup is involved, so nested
//! leaf columns work the same as top-level ones.

use arrow::datatypes::{Decimal128Type, Decimal256Type, DecimalType, i256};
use arrow::temporal_conversions::{
    date32_to_datetime, time32ms_to_time, time64ns_to_time, time64us_to_time,
    timestamp_ms_to_datetime, timestamp_ns_to_datetime, timestamp_us_to_datetime,
};
use parquet::basic::{ConvertedType, LogicalType, TimeUnit};
use parquet::data_type::Int96;
use parquet::file::statistics::{Statistics, ValueStatistics};
use parquet::schema::types::ColumnDescriptor;

use crate::format::{bytes_hex, bytes_view};
use crate::views::cell::CellString;

/// ISO-8601 with a fractional part only when there is one.
const DATETIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.f";

/// Shortest BYTE_ARRAY bound that gets an "inexact" marker.
///
/// Parquet reads a missing `is_min_value_exact` / `is_max_value_exact` flag as `false`, and plenty
/// of writers omit it, so the flag alone would mark nearly every string bound as truncated. Writers
/// only truncate values longer than their configured truncate length (64 bytes by default in
/// arrow-rs), so a short bound is never a prefix regardless of what the flag says.
const INEXACT_MARK_MIN_LEN: usize = 16;

/// Which bound of a statistics pair to render.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bound {
    Min,
    Max,
}

impl Bound {
    fn pick<T>(self, stats: &ValueStatistics<T>) -> Option<&T> {
        match self {
            Bound::Min => stats.min_opt(),
            Bound::Max => stats.max_opt(),
        }
    }

    /// Whether the writer told us this bound is the exact value rather than a prefix of it.
    fn is_exact(self, stats: &Statistics) -> bool {
        match self {
            Bound::Min => stats.min_is_exact(),
            Bound::Max => stats.max_is_exact(),
        }
    }
}

/// Format one bound of a column chunk's statistics using the column's logical type.
///
/// Returns `None` when the bound is not present in the metadata.
pub fn stat_value(
    stats: &Statistics,
    descr: &ColumnDescriptor,
    bound: Bound,
) -> Option<CellString> {
    let kind = StatKind::of(descr);
    let exact = bound.is_exact(stats);

    match stats {
        Statistics::Boolean(s) => bound.pick(s).map(|v| v.to_string().into()),
        Statistics::Int32(s) => bound.pick(s).map(|v| int32(*v, kind)),
        Statistics::Int64(s) => bound.pick(s).map(|v| int64(*v, kind)),
        Statistics::Int96(s) => bound.pick(s).map(int96),
        Statistics::Float(s) => bound.pick(s).map(|v| v.to_string().into()),
        Statistics::Double(s) => bound.pick(s).map(|v| v.to_string().into()),
        Statistics::ByteArray(s) => bound.pick(s).map(|v| byte_array(v.data(), kind, exact)),
        Statistics::FixedLenByteArray(s) => {
            bound.pick(s).map(|v| fixed_len_byte_array(v.data(), kind))
        }
    }
}

/// Sub-second resolution of a `Time` or `Timestamp` logical type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Unit {
    Millis,
    Micros,
    Nanos,
}

impl From<&TimeUnit> for Unit {
    fn from(unit: &TimeUnit) -> Self {
        match unit {
            TimeUnit::MILLIS => Unit::Millis,
            TimeUnit::MICROS => Unit::Micros,
            TimeUnit::NANOS => Unit::Nanos,
        }
    }
}

/// How a column's statistics should be interpreted, resolved from its schema entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StatKind {
    Decimal {
        precision: i32,
        scale: i32,
    },
    Date,
    Time(Unit),
    Timestamp {
        unit: Unit,
        utc: bool,
    },
    /// Stored in a signed physical type but logically unsigned.
    Unsigned,
    Float16,
    Uuid,
    Interval,
    /// Text-like: UTF-8 encoded.
    Text,
    /// Nothing to decode — render the physical value as-is.
    Raw,
}

impl StatKind {
    fn of(descr: &ColumnDescriptor) -> Self {
        match descr.logical_type_ref() {
            Some(logical) => Self::from_logical(logical, descr),
            // Files written before parquet-format 2.4.0 carry only the converted type.
            None => Self::from_converted(descr.converted_type(), descr),
        }
    }

    fn from_logical(logical: &LogicalType, descr: &ColumnDescriptor) -> Self {
        match logical {
            LogicalType::Decimal(decimal) => StatKind::Decimal {
                precision: decimal.precision,
                scale: decimal.scale,
            },
            LogicalType::Date => StatKind::Date,
            LogicalType::Time(time) => StatKind::Time((&time.unit).into()),
            LogicalType::Timestamp(timestamps) => StatKind::Timestamp {
                unit: (&timestamps.unit).into(),
                utc: timestamps.is_adjusted_to_u_t_c,
            },
            LogicalType::Integer(int) if !int.is_signed => StatKind::Unsigned,
            LogicalType::Float16 => StatKind::Float16,
            LogicalType::Uuid => StatKind::Uuid,
            LogicalType::String | LogicalType::Enum | LogicalType::Json => StatKind::Text,
            // `INTERVAL` never got a logical-type equivalent, so a column can declare another
            // logical type while still carrying the converted type.
            _ if descr.converted_type() == ConvertedType::INTERVAL => StatKind::Interval,
            _ => StatKind::Raw,
        }
    }

    fn from_converted(converted: ConvertedType, descr: &ColumnDescriptor) -> Self {
        match converted {
            ConvertedType::DECIMAL => StatKind::Decimal {
                precision: descr.type_precision(),
                scale: descr.type_scale(),
            },
            ConvertedType::DATE => StatKind::Date,
            ConvertedType::TIME_MILLIS => StatKind::Time(Unit::Millis),
            ConvertedType::TIME_MICROS => StatKind::Time(Unit::Micros),
            // The legacy timestamp converted types are defined as UTC-normalised.
            ConvertedType::TIMESTAMP_MILLIS => StatKind::Timestamp {
                unit: Unit::Millis,
                utc: true,
            },
            ConvertedType::TIMESTAMP_MICROS => StatKind::Timestamp {
                unit: Unit::Micros,
                utc: true,
            },
            ConvertedType::UINT_8
            | ConvertedType::UINT_16
            | ConvertedType::UINT_32
            | ConvertedType::UINT_64 => StatKind::Unsigned,
            ConvertedType::UTF8 | ConvertedType::ENUM | ConvertedType::JSON => StatKind::Text,
            ConvertedType::INTERVAL => StatKind::Interval,
            _ => StatKind::Raw,
        }
    }
}

// ----------------------------------------------------------------------
// Per-physical-type rendering

fn int32(v: i32, kind: StatKind) -> CellString {
    let decoded = match kind {
        StatKind::Decimal { precision, scale } => decimal_i128(v.into(), precision, scale),
        StatKind::Date => date32_to_datetime(v).map(|dt| dt.date().to_string()),
        StatKind::Time(unit) => time(v.into(), unit),
        StatKind::Unsigned => Some((v as u32).to_string()),
        _ => None,
    };
    decoded.unwrap_or_else(|| v.to_string()).into()
}

fn int64(v: i64, kind: StatKind) -> CellString {
    let decoded = match kind {
        StatKind::Decimal { precision, scale } => decimal_i128(v.into(), precision, scale),
        StatKind::Time(unit) => time(v, unit),
        StatKind::Timestamp { unit, utc } => timestamp(v, unit, utc),
        StatKind::Unsigned => Some((v as u64).to_string()),
        _ => None,
    };
    decoded.unwrap_or_else(|| v.to_string()).into()
}

/// INT96 is only ever a nanosecond timestamp, and always UTC by convention.
fn int96(v: &Int96) -> CellString {
    timestamp(v.to_nanos(), Unit::Nanos, true)
        .unwrap_or_else(|| bytes_hex(&v.to_nanos().to_be_bytes()))
        .into()
}

fn byte_array(data: &[u8], kind: StatKind, exact: bool) -> CellString {
    // Variable-length bounds are the only ones writers truncate.
    let inexact = !exact && data.len() >= INEXACT_MARK_MIN_LEN;

    match kind {
        StatKind::Text => text(data, inexact),
        // A truncated two's-complement prefix decodes to a different number, so fall through to the
        // raw bytes rather than showing a confidently wrong value.
        StatKind::Decimal { precision, scale } if !inexact => {
            hex_or(data, decimal_be(data, precision, scale), inexact)
        }
        StatKind::Raw => mark_inexact(bytes_view(data), inexact),
        _ => mark_inexact(bytes_hex(data).into(), inexact),
    }
}

fn fixed_len_byte_array(data: &[u8], kind: StatKind) -> CellString {
    // FIXED_LEN_BYTE_ARRAY bounds are never truncated, so every length check below is a
    // well-formedness check rather than a truncation check.
    let decoded = match kind {
        StatKind::Decimal { precision, scale } => decimal_be(data, precision, scale),
        StatKind::Uuid => uuid(data),
        StatKind::Float16 => float16(data),
        StatKind::Interval => interval(data),
        StatKind::Text => return text(data, false),
        // With no logical type to go on, keep the existing "quote it if it reads as ASCII"
        // heuristic; for anything else the bytes are opaque and hex is the honest rendering.
        StatKind::Raw => return bytes_view(data),
        _ => None,
    };
    hex_or(data, decoded, false)
}

// ----------------------------------------------------------------------
// Decoders

fn decimal_i128(v: i128, precision: i32, scale: i32) -> Option<String> {
    let (precision, scale) = decimal_params(precision, scale)?;
    Some(Decimal128Type::format_decimal(v, precision, scale))
}

/// Decode a two's-complement big-endian unscaled value, as stored by BYTE_ARRAY and
/// FIXED_LEN_BYTE_ARRAY decimal columns.
fn decimal_be(data: &[u8], precision: i32, scale: i32) -> Option<String> {
    let (precision, scale) = decimal_params(precision, scale)?;
    if data.len() <= 16 {
        Some(Decimal128Type::format_decimal(
            i128::from_be_bytes(sign_extend(data)?),
            precision,
            scale,
        ))
    } else {
        Some(Decimal256Type::format_decimal(
            i256::from_be_bytes(sign_extend(data)?),
            precision,
            scale,
        ))
    }
}

/// Sign-extend a big-endian two's-complement value into a fixed-width buffer.
fn sign_extend<const N: usize>(data: &[u8]) -> Option<[u8; N]> {
    if data.is_empty() || data.len() > N {
        return None;
    }
    let fill = if data[0] & 0x80 == 0 { 0x00 } else { 0xFF };
    let mut buf = [fill; N];
    buf[N - data.len()..].copy_from_slice(data);
    Some(buf)
}

/// `format_decimal` bounds its output by `precision`, so a bogus precision would silently blank the
/// value. Reject anything outside the range parquet allows.
fn decimal_params(precision: i32, scale: i32) -> Option<(u8, i8)> {
    let precision = u8::try_from(precision)
        .ok()
        .filter(|p| (1..=76).contains(p))?;
    let scale = i8::try_from(scale).ok()?;
    Some((precision, scale))
}

fn time(v: i64, unit: Unit) -> Option<String> {
    let time = match unit {
        Unit::Millis => time32ms_to_time(i32::try_from(v).ok()?),
        Unit::Micros => time64us_to_time(v),
        Unit::Nanos => time64ns_to_time(v),
    }?;
    Some(time.to_string())
}

fn timestamp(v: i64, unit: Unit, utc: bool) -> Option<String> {
    let datetime = match unit {
        Unit::Millis => timestamp_ms_to_datetime(v),
        Unit::Micros => timestamp_us_to_datetime(v),
        Unit::Nanos => timestamp_ns_to_datetime(v),
    }?;
    let formatted = datetime.format(DATETIME_FORMAT);
    Some(if utc {
        format!("{formatted}Z")
    } else {
        formatted.to_string()
    })
}

fn uuid(data: &[u8]) -> Option<String> {
    let hex = hex::encode(<[u8; 16]>::try_from(data).ok()?);
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// Parquet stores FLOAT16 as two little-endian bytes of IEEE-754 binary16.
fn float16(data: &[u8]) -> Option<String> {
    let bits = u16::from_le_bytes(<[u8; 2]>::try_from(data).ok()?);
    Some(f16_to_f32(bits).to_string())
}

fn f16_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15);
    let exponent = u32::from((bits >> 10) & 0x1F);
    let fraction = u32::from(bits & 0x03FF);

    let magnitude = match exponent {
        // Subnormal (and zero): the value is `fraction * 2^-24`, exact in f32.
        0 => return apply_sign(f32::from(fraction as u16) / 16_777_216.0, sign),
        // Infinity / NaN.
        0x1F => f32::from_bits((0xFF << 23) | (fraction << 13)),
        // Normal: rebias the exponent from 15 to 127 and widen the mantissa.
        _ => f32::from_bits(((exponent + 112) << 23) | (fraction << 13)),
    };
    apply_sign(magnitude, sign)
}

fn apply_sign(magnitude: f32, sign: u32) -> f32 {
    if sign == 1 { -magnitude } else { magnitude }
}

/// The deprecated `INTERVAL` converted type: three little-endian `u32`s.
fn interval(data: &[u8]) -> Option<String> {
    let bytes = <[u8; 12]>::try_from(data).ok()?;
    let months = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let days = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let millis = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    Some(format!(
        "{months}mo {days}d {}.{:03}s",
        millis / 1000,
        millis % 1000
    ))
}

// ----------------------------------------------------------------------
// Cell construction

/// Render a text-like bound, accepting any valid UTF-8 rather than only ASCII.
fn text(data: &[u8], inexact: bool) -> CellString {
    let Some(text) = utf8_prefix(data) else {
        return mark_inexact(bytes_view(data), inexact);
    };

    // A truncated bound may end mid-code-point, in which case the decoded prefix is shorter than
    // the raw bytes and the value is definitely not exact.
    let inexact = inexact || text.len() < data.len();

    let mut short = String::with_capacity(text.len() + 6);
    short.push('"');
    let mut words = text.split_whitespace();
    if let Some(word) = words.next() {
        short.push_str(word);
    }
    for word in words {
        short.push(' ');
        short.push_str(word);
    }
    if inexact {
        short.push('…');
    }
    short.push('"');

    CellString::new(text.to_owned(), short)
}

/// The longest valid UTF-8 prefix of `data`, or `None` if the bytes are not text at all.
///
/// A complete value decodes whole; a bound truncated mid-code-point leaves an incomplete trailing
/// sequence (`error_len() == None`), which we drop. Genuinely invalid bytes are rejected.
fn utf8_prefix(data: &[u8]) -> Option<&str> {
    match str::from_utf8(data) {
        Ok(text) => Some(text),
        Err(err) if err.error_len().is_none() && err.valid_up_to() > 0 => {
            str::from_utf8(&data[..err.valid_up_to()]).ok()
        }
        Err(_) => None,
    }
}

/// Use `decoded` if the bytes made sense for the column's type, otherwise show them as hex.
///
/// Falling back to hex rather than [`bytes_view`] matters here: the column's type says these bytes
/// are a number or an identifier, so quoting them as text just because they happen to be printable
/// ASCII would be misleading.
fn hex_or(data: &[u8], decoded: Option<String>, inexact: bool) -> CellString {
    let cell = decoded.unwrap_or_else(|| bytes_hex(data));
    mark_inexact(cell.into(), inexact)
}

/// Append an ellipsis to the *displayed* value only, so copying the cell still yields the raw bound.
fn mark_inexact(cell: CellString, inexact: bool) -> CellString {
    if !inexact {
        return cell;
    }
    let short = format!("{}…", cell.short_or_real());
    CellString::new(cell.real, short)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parquet::basic::{DecimalType, Type as PhysicalType};
    use parquet::data_type::{ByteArray, FixedLenByteArray};
    use parquet::schema::types::{ColumnPath, Type as SchemaType};

    use super::*;

    fn cell(decoded: Option<String>) -> String {
        decoded.expect("value should decode")
    }

    /// Build a leaf-column descriptor the way a real parquet schema would.
    fn descriptor(
        physical: PhysicalType,
        logical: Option<LogicalType>,
        converted: ConvertedType,
        length: i32,
        precision: i32,
        scale: i32,
    ) -> ColumnDescriptor {
        let primitive = SchemaType::primitive_type_builder("c", physical)
            .with_logical_type(logical)
            .with_converted_type(converted)
            .with_length(length)
            .with_precision(precision)
            .with_scale(scale)
            .build()
            .expect("valid primitive type");
        ColumnDescriptor::new(Arc::new(primitive), 0, 0, ColumnPath::new(vec!["c".into()]))
    }

    fn logical(physical: PhysicalType, logical: LogicalType) -> ColumnDescriptor {
        descriptor(physical, Some(logical), ConvertedType::NONE, 0, 0, 0)
    }

    /// Render both bounds of a statistics pair through the public entry point.
    fn bounds(stats: &Statistics, descr: &ColumnDescriptor) -> (String, String) {
        let render = |bound| {
            stat_value(stats, descr, bound)
                .expect("bound should be present")
                .short_or_real()
                .clone()
        };
        (render(Bound::Min), render(Bound::Max))
    }

    fn flba_stats(min: Vec<u8>, max: Vec<u8>) -> Statistics {
        Statistics::FixedLenByteArray(ValueStatistics::new(
            Some(FixedLenByteArray::from(ByteArray::from(min))),
            Some(FixedLenByteArray::from(ByteArray::from(max))),
            None,
            None,
            false,
        ))
    }

    #[test]
    fn decimal_column_end_to_end() {
        // FIXED_LEN_BYTE_ARRAY(8), DECIMAL(18, 2) — the encoding the user hit.
        let descr = descriptor(
            PhysicalType::FIXED_LEN_BYTE_ARRAY,
            Some(LogicalType::Decimal(DecimalType {
                precision: 18,
                scale: 2,
            })),
            ConvertedType::NONE,
            8,
            18,
            2,
        );
        let stats = flba_stats(
            (-1234_i64).to_be_bytes().to_vec(),
            567_890_i64.to_be_bytes().to_vec(),
        );
        assert_eq!(bounds(&stats, &descr), ("-12.34".into(), "5678.90".into()));
    }

    #[test]
    fn decimal_column_from_int32_physical_type() {
        let descr = descriptor(
            PhysicalType::INT32,
            Some(LogicalType::Decimal(DecimalType {
                precision: 9,
                scale: 2,
            })),
            ConvertedType::NONE,
            0,
            9,
            2,
        );
        let stats = Statistics::int32(Some(-5), Some(1234), None, None, false);
        assert_eq!(bounds(&stats, &descr), ("-0.05".into(), "12.34".into()));
    }

    #[test]
    fn legacy_converted_type_columns_still_decode() {
        // A pre-2.4.0 file: no logical type, DECIMAL carried by the converted type.
        let descr = descriptor(PhysicalType::INT64, None, ConvertedType::DECIMAL, 0, 18, 2);
        let stats = Statistics::int64(Some(1234), Some(5678), None, None, false);
        assert_eq!(bounds(&stats, &descr), ("12.34".into(), "56.78".into()));

        let descr = descriptor(PhysicalType::INT32, None, ConvertedType::DATE, 0, 0, 0);
        let stats = Statistics::int32(Some(0), Some(20659), None, None, false);
        assert_eq!(
            bounds(&stats, &descr),
            ("1970-01-01".into(), "2026-07-25".into())
        );

        let descr = descriptor(
            PhysicalType::INT64,
            None,
            ConvertedType::TIMESTAMP_MILLIS,
            0,
            0,
            0,
        );
        let stats = Statistics::int64(Some(0), Some(1_784_988_191_250), None, None, false);
        assert_eq!(
            bounds(&stats, &descr),
            (
                "1970-01-01T00:00:00Z".into(),
                "2026-07-25T14:03:11.250Z".into()
            )
        );

        let descr = descriptor(PhysicalType::INT64, None, ConvertedType::UINT_64, 0, 0, 0);
        let stats = Statistics::int64(Some(0), Some(-1), None, None, false);
        assert_eq!(
            bounds(&stats, &descr),
            ("0".into(), "18446744073709551615".into())
        );
    }

    #[test]
    fn timestamp_column_end_to_end() {
        let descr = logical(
            PhysicalType::INT64,
            LogicalType::Timestamp(parquet::basic::TimestampType {
                is_adjusted_to_u_t_c: true,
                unit: TimeUnit::MICROS,
            }),
        );
        let stats = Statistics::int64(Some(0), Some(1_784_988_191_250_000), None, None, false);
        assert_eq!(
            bounds(&stats, &descr),
            (
                "1970-01-01T00:00:00Z".into(),
                "2026-07-25T14:03:11.250Z".into()
            )
        );

        let descr = logical(
            PhysicalType::INT64,
            LogicalType::Timestamp(parquet::basic::TimestampType {
                is_adjusted_to_u_t_c: false,
                unit: TimeUnit::MICROS,
            }),
        );
        assert_eq!(
            bounds(&stats, &descr).1,
            "2026-07-25T14:03:11.250".to_string()
        );
    }

    #[test]
    fn uuid_column_end_to_end() {
        let descr = descriptor(
            PhysicalType::FIXED_LEN_BYTE_ARRAY,
            Some(LogicalType::Uuid),
            ConvertedType::NONE,
            16,
            0,
            0,
        );
        let value = hex::decode("550e8400e29b41d4a716446655440000").unwrap();
        let stats = flba_stats(vec![0u8; 16], value);
        assert_eq!(
            bounds(&stats, &descr),
            (
                "00000000-0000-0000-0000-000000000000".into(),
                "550e8400-e29b-41d4-a716-446655440000".into()
            )
        );
    }

    #[test]
    fn string_column_end_to_end() {
        let descr = logical(PhysicalType::BYTE_ARRAY, LogicalType::String);
        let stats = Statistics::ByteArray(ValueStatistics::new(
            Some(ByteArray::from("café".as_bytes().to_vec())),
            Some(ByteArray::from("zürich".as_bytes().to_vec())),
            None,
            None,
            false,
        ));
        assert_eq!(
            bounds(&stats, &descr),
            ("\"café\"".into(), "\"zürich\"".into())
        );
    }

    #[test]
    fn missing_bounds_return_none() {
        let descr = logical(PhysicalType::INT32, LogicalType::Date);
        let stats = Statistics::int32(None, None, None, Some(3), false);
        assert!(stat_value(&stats, &descr, Bound::Min).is_none());
        assert!(stat_value(&stats, &descr, Bound::Max).is_none());
    }

    #[test]
    fn a_column_with_no_type_information_renders_as_before() {
        let descr = descriptor(PhysicalType::BYTE_ARRAY, None, ConvertedType::NONE, 0, 0, 0);
        let stats = Statistics::ByteArray(ValueStatistics::new(
            Some(ByteArray::from(vec![0xDE, 0xAD])),
            Some(ByteArray::from(b"plain".to_vec())),
            None,
            None,
            false,
        ));
        assert_eq!(
            bounds(&stats, &descr),
            ("0xDEAD".into(), "\"plain\"".into())
        );
    }

    #[test]
    fn decimal_from_int_physical_types() {
        assert_eq!(cell(decimal_i128(1234, 9, 2)), "12.34");
        assert_eq!(cell(decimal_i128(-1234, 9, 2)), "-12.34");
        assert_eq!(cell(decimal_i128(5, 9, 2)), "0.05");
        assert_eq!(cell(decimal_i128(1234, 9, 0)), "1234");
        // A negative scale multiplies by a power of ten.
        assert_eq!(cell(decimal_i128(12, 9, -2)), "1200");
    }

    #[test]
    fn decimal_from_big_endian_bytes() {
        // 1234 at scale 2, in each of the widths a writer might pick.
        assert_eq!(cell(decimal_be(&[0x04, 0xD2], 9, 2)), "12.34");
        assert_eq!(cell(decimal_be(&[0x00, 0x00, 0x04, 0xD2], 9, 2)), "12.34");
        assert_eq!(cell(decimal_be(&1234_i64.to_be_bytes(), 18, 2)), "12.34");
        assert_eq!(cell(decimal_be(&1234_i128.to_be_bytes(), 38, 2)), "12.34");
    }

    #[test]
    fn decimal_sign_extends_negative_bytes() {
        assert_eq!(cell(decimal_be(&[0xFB, 0x2E], 9, 2)), "-12.34");
        assert_eq!(
            cell(decimal_be(&(-1234_i64).to_be_bytes(), 18, 2)),
            "-12.34"
        );
        assert_eq!(cell(decimal_be(&[0xFF], 4, 2)), "-0.01");
    }

    #[test]
    fn decimal_uses_i256_beyond_sixteen_bytes() {
        let mut bytes = [0u8; 20];
        bytes[4..].copy_from_slice(&1234_i128.to_be_bytes());
        assert_eq!(cell(decimal_be(&bytes, 40, 2)), "12.34");

        let mut negative = [0xFFu8; 20];
        negative[4..].copy_from_slice(&(-1234_i128).to_be_bytes());
        assert_eq!(cell(decimal_be(&negative, 40, 2)), "-12.34");
    }

    #[test]
    fn decimal_rejects_bogus_precision_and_oversized_values() {
        assert_eq!(decimal_be(&[0x04, 0xD2], 0, 2), None);
        assert_eq!(decimal_be(&[0x04, 0xD2], 77, 2), None);
        assert_eq!(decimal_be(&[], 9, 2), None);
        assert_eq!(decimal_be(&[0u8; 33], 38, 2), None);
    }

    #[test]
    fn unsigned_columns_are_reinterpreted() {
        let kind = StatKind::Unsigned;
        assert_eq!(int64(-1, kind).real, "18446744073709551615");
        assert_eq!(int32(-1, kind).real, "4294967295");
        assert_eq!(int32(7, kind).real, "7");
        // Signed columns are untouched.
        assert_eq!(int64(-1, StatKind::Raw).real, "-1");
    }

    #[test]
    fn dates_and_times() {
        // 2026-07-25 is 20659 days after the epoch.
        assert_eq!(int32(20659, StatKind::Date).real, "2026-07-25");
        assert_eq!(cell(time(50_591_250, Unit::Millis)), "14:03:11.250");
        assert_eq!(cell(time(50_591_250_000, Unit::Micros)), "14:03:11.250");
        assert_eq!(cell(time(50_591_250_000_000, Unit::Nanos)), "14:03:11.250");
    }

    #[test]
    fn timestamps_mark_utc_only_when_adjusted() {
        let micros = 1_784_988_191_250_000;
        assert_eq!(
            cell(timestamp(micros, Unit::Micros, true)),
            "2026-07-25T14:03:11.250Z"
        );
        assert_eq!(
            cell(timestamp(micros, Unit::Micros, false)),
            "2026-07-25T14:03:11.250"
        );
        assert_eq!(
            cell(timestamp(micros / 1000, Unit::Millis, true)),
            "2026-07-25T14:03:11.250Z"
        );
        assert_eq!(
            cell(timestamp(micros * 1000, Unit::Nanos, true)),
            "2026-07-25T14:03:11.250Z"
        );
        // Whole seconds have no fractional part.
        assert_eq!(
            cell(timestamp(1_784_988_191, Unit::Millis, true)),
            "1970-01-21T15:49:48.191Z"
        );
    }

    #[test]
    fn int96_renders_as_a_utc_timestamp() {
        let mut value = Int96::new();
        // Nanoseconds since midnight, then the Julian day for 2026-07-25.
        let nanos = 50_591_250_000_000_u64;
        value.set_data(nanos as u32, (nanos >> 32) as u32, 20659 + 2_440_588);
        assert_eq!(int96(&value).real, "2026-07-25T14:03:11.250Z");
    }

    #[test]
    fn uuid_is_hyphenated() {
        let bytes = hex::decode("550e8400e29b41d4a716446655440000").unwrap();
        assert_eq!(cell(uuid(&bytes)), "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(uuid(&bytes[..15]), None);
    }

    #[test]
    fn float16_decodes_binary16() {
        assert_eq!(cell(float16(&0x3C00_u16.to_le_bytes())), "1");
        assert_eq!(cell(float16(&0xBC00_u16.to_le_bytes())), "-1");
        assert_eq!(cell(float16(&0x0000_u16.to_le_bytes())), "0");
        assert_eq!(cell(float16(&0x8000_u16.to_le_bytes())), "-0");
        assert_eq!(cell(float16(&0x3555_u16.to_le_bytes())), "0.33325195");
        assert_eq!(cell(float16(&0x7C00_u16.to_le_bytes())), "inf");
        assert_eq!(cell(float16(&0xFC00_u16.to_le_bytes())), "-inf");
        assert_eq!(cell(float16(&0x7E00_u16.to_le_bytes())), "NaN");
        // Smallest subnormal: 2^-24.
        assert_eq!(
            cell(float16(&0x0001_u16.to_le_bytes())),
            "0.000000059604645"
        );
        assert_eq!(float16(&[0x00]), None);
    }

    #[test]
    fn interval_splits_into_months_days_millis() {
        let mut bytes = [0u8; 12];
        bytes[0..4].copy_from_slice(&1_u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&2_u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&3500_u32.to_le_bytes());
        assert_eq!(cell(interval(&bytes)), "1mo 2d 3.500s");
    }

    #[test]
    fn text_accepts_non_ascii_utf8() {
        let value = text("café".as_bytes(), false);
        assert_eq!(value.real, "café");
        assert_eq!(value.short.as_deref(), Some("\"café\""));
    }

    #[test]
    fn text_collapses_whitespace_for_display_but_copies_the_original() {
        let value = text(b"a  b\n c", false);
        assert_eq!(value.real, "a  b\n c");
        assert_eq!(value.short.as_deref(), Some("\"a b c\""));
    }

    #[test]
    fn truncated_text_gets_an_ellipsis() {
        let value = byte_array(b"Alexandria and then some more", StatKind::Text, false);
        assert_eq!(value.real, "Alexandria and then some more");
        assert_eq!(
            value.short.as_deref(),
            Some("\"Alexandria and then some more…\"")
        );

        // Short bounds are left alone even though the flag says inexact, because writers do not
        // truncate values that short.
        let short = byte_array(b"Alex", StatKind::Text, false);
        assert_eq!(short.short.as_deref(), Some("\"Alex\""));
    }

    #[test]
    fn text_truncated_mid_code_point_drops_the_partial_byte() {
        // "café" cut one byte into the two-byte 'é'.
        let mut bytes = "café".as_bytes().to_vec();
        bytes.pop();
        let value = text(&bytes, false);
        assert_eq!(value.real, "caf");
        assert_eq!(value.short.as_deref(), Some("\"caf…\""));
    }

    #[test]
    fn truncated_byte_array_decimal_falls_back_to_hex() {
        let long = [0x0Au8; 20];
        let value = byte_array(
            &long,
            StatKind::Decimal {
                precision: 38,
                scale: 2,
            },
            false,
        );
        assert_eq!(value.real, bytes_hex(&long));
        assert_eq!(
            value.short.as_deref(),
            Some(&format!("{}…", bytes_hex(&long))[..])
        );

        // The same bytes decode once the writer vouches for them.
        let exact = byte_array(
            &long,
            StatKind::Decimal {
                precision: 38,
                scale: 2,
            },
            true,
        );
        assert!(
            exact.real.contains('.'),
            "expected a decimal, got {}",
            exact.real
        );
    }

    #[test]
    fn unknown_types_keep_the_previous_byte_rendering() {
        let value = fixed_len_byte_array(&[0xDE, 0xAD, 0xBE, 0xEF], StatKind::Raw);
        assert_eq!(value.real, "0xDEADBEEF");
    }

    #[test]
    fn malformed_values_fall_back_instead_of_panicking() {
        // A UUID column whose bound is the wrong width.
        assert_eq!(
            fixed_len_byte_array(&[0x01, 0x02], StatKind::Uuid).real,
            "0x0102"
        );
        // A date column whose statistics are somehow out of range still renders the raw integer.
        assert_eq!(int32(i32::MAX, StatKind::Date).real, i32::MAX.to_string());
    }
}
