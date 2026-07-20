use iced::widget::container::Style as ContainerStyle;
use iced::widget::text::Wrapping;
use iced::widget::{Row, button, column, container, mouse_area, row, text};
use iced::{Background, Border, Element, Length, Theme};
use parquet::file::metadata::ColumnChunkMetaData;
use parquet::file::statistics::Statistics;

use crate::app::{FileMessage, Message};
use crate::format::{bytes_view, human_bytes};
use crate::parquet_io::FileSummary;
use crate::views::cell::CellString;
use crate::views::overview::format_sorting_columns;

pub fn view(file: &FileSummary, selected: Option<usize>) -> Element<'_, Message> {
    let mut col = column![summary_header()].spacing(0);

    for (i, rg) in file.metadata.row_groups().iter().enumerate() {
        let compressed: i64 = rg.columns().iter().map(|c| c.compressed_size()).sum();
        let toggle_label = if selected == Some(i) { "▾" } else { "▸" };
        let zebra = i % 2 == 1;

        let summary = row![
            container(
                button(text(toggle_label.to_string()))
                    .on_press(FileMessage::RowGroupToggled(i).into())
                    .style(button::secondary),
            )
            .width(Length::Fixed(40.0))
            .padding([2, 4]),
            body_cell(format!("Group {i}"), 110.into()),
            body_cell(format!("{}", rg.num_rows()), 110.into()),
            body_cell(human_bytes(rg.total_byte_size().max(0) as u64), 140.into()),
            body_cell(human_bytes(compressed.max(0) as u64), 140.into()),
            body_cell(format!("{}", rg.num_columns()), 90.into()),
        ]
        .spacing(0)
        .align_y(iced::Alignment::Center);

        let styled_summary =
            container(summary).style(move |theme: &Theme| body_row_style(theme, zebra));
        col = col.push(styled_summary);

        if selected == Some(i) {
            col = col.push(column_chunk_table(file, i));
        }
    }

    col.into()
}

fn summary_header() -> Element<'static, Message> {
    let r = row![
        container(text(" ")).width(Length::Fixed(40.0)),
        header_cell("Index", 110.into()),
        header_cell("Rows", 110.into()),
        header_cell("Raw", 140.into()),
        header_cell("Packed", 140.into()),
        header_cell("Columns", 90.into()),
    ]
    .spacing(0);
    container(r).style(header_row_style).into()
}

fn column_chunk_table(file: &FileSummary, rg_idx: usize) -> Element<'_, Message> {
    let rg = file.metadata.row_group(rg_idx);
    let sort_order: Element<'_, Message> = match rg.sorting_columns() {
        Some(cols) if !cols.is_empty() => Row::with_children(
            format_sorting_columns(file, cols)
                .into_iter()
                .map(|text| iced::widget::Text::from(text).size(13).into()),
        )
        .into(),
        Some(_) => "(empty)".into(),
        None => "(not specified)".into(),
    };
    let sort_row = row![text("Sort order:").size(13), sort_order]
        .spacing(8)
        .padding([0, 0]);

    let mut columns = [
        CcColumn::new("Column", 240.into(), |cc| cc.column_path().string()),
        CcColumn::new("Values", 90.into(), |cc| format!("{}", cc.num_values())),
        CcColumn::new("Nulls", 90.into(), |cc| {
            cc.statistics()
                .and_then(|s| s.null_count_opt())
                .map_or_else(|| "—".into(), |val| val.to_string())
        }),
        CcColumn::new("Raw", 100.into(), |cc| {
            human_bytes(cc.uncompressed_size().max(0) as u64)
        }),
        CcColumn::new("Packed", 100.into(), |cc| {
            human_bytes(cc.compressed_size().max(0) as u64)
        }),
        CcColumn::new("Comp", 50.into(), |cc| format!("{:?}", cc.compression())),
        CcColumn::new("Coding", 100.into(), |cc| {
            cc.encodings()
                .map(|e| format!("{e:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        }),
        CcColumn::new("Min", 240.into(), |cc| {
            let min_str = format_min(cc.statistics());
            min_str.unwrap_or_else(|| "—".into())
        }),
        CcColumn::new("Max", 240.into(), |cc| {
            let max_str = format_max(cc.statistics());
            max_str.unwrap_or_else(|| "—".into())
        }),
    ];

    let mut header_row = row![];
    for column in columns.iter_mut() {
        header_row = header_row.push(column.element.take());
    }
    let mut table = column![sort_row, header_row].spacing(0);

    for (idx, cc) in rg.columns().iter().enumerate() {
        let mut row = row![].spacing(0);

        for column in columns.iter() {
            row = row.push(body_cell((column.view)(cc), column.width));
        }

        let zebra = idx % 2 == 1;
        let styled = container(row).style(move |theme| body_row_style(theme, zebra));
        table = table.push(styled);
    }

    container(table).padding([4, 40]).into()
}

pub struct CcColumn<'a, 'b> {
    element: Option<Element<'a, Message>>,
    width: Length,
    view: Box<dyn Fn(&'a ColumnChunkMetaData) -> CellString + 'b>,
}
impl<'a, 'b> CcColumn<'a, 'b> {
    pub fn new<S: Into<CellString>>(
        header: &str,
        width: Length,
        view: impl Fn(&'a ColumnChunkMetaData) -> S + 'b,
    ) -> Self {
        Self {
            element: Some(header_cell(header, width)),
            width,
            view: Box::new(move |cc| view(cc).into()),
        }
    }
}

fn header_cell<'a>(label: &str, length: Length) -> Element<'a, Message> {
    let label = text(label.to_string()).size(13).wrapping(Wrapping::None);
    container(label)
        .width(length)
        .padding([6, 10])
        .clip(true)
        .into()
}

fn body_cell<'a>(value: impl Into<CellString>, width: Length) -> Element<'a, Message> {
    let value = value.into();
    let label = text(value.short_or_real().clone())
        .size(13)
        .wrapping(Wrapping::Word);
    let inner = container(label).width(width).padding([4, 10]).clip(true);

    mouse_area(inner)
        .on_press(FileMessage::CopyCell(value.real).into())
        .into()
}

fn header_row_style(theme: &Theme) -> ContainerStyle {
    let p = theme.extended_palette();
    ContainerStyle {
        background: Some(Background::Color(p.background.strong.color)),
        text_color: Some(p.background.strong.text),
        border: Border::default(),
        ..ContainerStyle::default()
    }
}

fn body_row_style(theme: &Theme, zebra: bool) -> ContainerStyle {
    let p = theme.extended_palette();
    let bg = if zebra {
        p.background.weak.color
    } else {
        p.background.base.color
    };
    ContainerStyle {
        background: Some(Background::Color(bg)),
        text_color: Some(p.background.base.text),
        border: Border::default(),
        ..ContainerStyle::default()
    }
}

fn format_min(stats: Option<&Statistics>) -> Option<CellString> {
    match stats? {
        Statistics::Boolean(s) => opt_dbg(s.min_opt()),
        Statistics::Int32(s) => opt_dbg(s.min_opt()),
        Statistics::Int64(s) => opt_dbg(s.min_opt()),
        Statistics::Int96(s) => opt_dbg(s.min_opt()),
        Statistics::Float(s) => opt_dbg(s.min_opt()),
        Statistics::Double(s) => opt_dbg(s.min_opt()),
        Statistics::ByteArray(s) => s.min_opt().map(|b| bytes_view(b.data())),
        Statistics::FixedLenByteArray(s) => s.min_opt().map(|b| bytes_view(b.data())),
    }
}

fn format_max(stats: Option<&Statistics>) -> Option<CellString> {
    match stats? {
        Statistics::Boolean(s) => opt_dbg(s.max_opt()),
        Statistics::Int32(s) => opt_dbg(s.max_opt()),
        Statistics::Int64(s) => opt_dbg(s.max_opt()),
        Statistics::Int96(s) => opt_dbg(s.max_opt()),
        Statistics::Float(s) => opt_dbg(s.max_opt()),
        Statistics::Double(s) => opt_dbg(s.max_opt()),
        Statistics::ByteArray(s) => s.max_opt().map(|b| bytes_view(b.data())),
        Statistics::FixedLenByteArray(s) => s.max_opt().map(|b| bytes_view(b.data())),
    }
}

fn opt_dbg<T: std::fmt::Debug>(v: Option<&T>) -> Option<CellString> {
    v.map(|v| format!("{v:?}").into())
}
