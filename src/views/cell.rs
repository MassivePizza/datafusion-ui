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
