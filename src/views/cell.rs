pub struct CellString {
    pub real: String,
    pub short: Option<String>,
}
impl CellString {
    pub fn new(real: String, short: String) -> Self {
        CellString {
            real,
            short: Some(short),
        }
    }

    pub fn short_or_real(&self) -> &String {
        self.short.as_ref().unwrap_or(&self.real)
    }

    /// Would the displayed text be cut off in `room` pixels of cell interior?
    ///
    /// Deliberately a character estimate rather than real shaping: the tables call this once per
    /// cell per frame, and iced won't measure text outside a layout pass anyway. Note this asks
    /// about *width* only — `short != real` is not a usable signal, because `stats_format` renders
    /// `short` with quotes or a truncation marker, which often makes it the *longer* of the two.
    pub fn overflows(&self, room: f32) -> bool {
        estimate_width(self.short_or_real()) > room
    }
}

/// Approximate rendered width of `s` in the UI font at the tables' 13px text size.
///
/// Splitting on case matters more than it looks: a flat average badly mis-sizes the two things
/// these tables actually hold. Parquet encoding names are nearly all caps, and a timestamp is
/// nearly all digits and punctuation, so one number either cries overflow on every timestamp or
/// stays silent while `RLE_DICTIONARY, PLAIN, RLE` runs off the edge. Two classes track the real
/// Geist Regular advances (~8.1px uppercase, ~6.9px for everything else) to within a few pixels.
fn estimate_width(s: &str) -> f32 {
    const UPPER_PX: f32 = 8.1;
    const OTHER_PX: f32 = 6.9;

    s.chars()
        .map(|c| if c.is_uppercase() { UPPER_PX } else { OTHER_PX })
        .sum()
}
impl From<String> for CellString {
    fn from(value: String) -> Self {
        CellString {
            real: value,
            short: None,
        }
    }
}
impl<'a> From<&'a str> for CellString {
    fn from(value: &'a str) -> Self {
        Self::from(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Room figures are the interior of a 195px table column (width minus 20px of padding).
    const CELL_ROOM: f32 = 175.0;

    #[test]
    fn a_value_wider_than_the_cell_overflows() {
        let uuid: CellString = "550e8400-e29b-41d4-a716-446655440000".into();
        assert!(uuid.overflows(CELL_ROOM)); // ~271px rendered

        let int64_min: CellString = "-9223372036854775808".into();
        assert!(!int64_min.overflows(CELL_ROOM)); // ~154px
    }

    #[test]
    fn digits_are_not_charged_the_uppercase_rate() {
        // A flat per-character average reports these two as the same width. They are not: the
        // timestamp fits its cell and the encoding list does not.
        let timestamp: CellString = "2026-08-01T12:34:56.789Z".into(); // 24 chars, ~165px
        let encodings: CellString = "RLE_DICTIONARY, PLAIN, RLE".into(); // 26 chars, ~182px

        assert!(!timestamp.overflows(CELL_ROOM));
        assert!(encodings.overflows(CELL_ROOM));
    }

    #[test]
    fn a_nanosecond_timestamp_still_overflows() {
        // arrow-rs's default timestamp unit — too wide for the cell, so it must offer a tooltip.
        let nanos: CellString = "2026-08-01T12:34:56.789012345Z".into(); // ~211px
        assert!(nanos.overflows(CELL_ROOM));
    }

    #[test]
    fn overflow_measures_what_is_displayed_not_what_is_copied() {
        // `short` is what the cell shows; `real` is only what a click copies.
        let cell = CellString::new("x".repeat(200), "short".into());
        assert!(!cell.overflows(CELL_ROOM));
    }
}
