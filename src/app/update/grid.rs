use iced::Task;

use super::super::*;

impl App {
    pub(crate) fn update_grid(&mut self, m: GridMessage) -> Task<Message> {
        match m {
            GridMessage::ColumnResize { id, col, width } => {
                let w = width.clamp(MIN_COL_WIDTH, 2000.0);
                if let Some(widths) = self.col_widths_mut(id)
                    && let Some(slot) = widths.get_mut(col)
                {
                    *slot = w;
                }
                Task::none()
            }
            GridMessage::ColumnResizeEnd { id } => {
                self.persist_col_widths(id);
                Task::none()
            }
            GridMessage::ColumnAutofit { id, col } => {
                let Some(new_w) = self.grid_batch(id).map(|b| autofit_width(b, col)) else {
                    return Task::none();
                };
                if let Some(widths) = self.col_widths_mut(id)
                    && let Some(slot) = widths.get_mut(col)
                {
                    *slot = new_w;
                }
                self.persist_col_widths(id);
                Task::none()
            }
            GridMessage::RowResize { id, height } => {
                let h = height.clamp(MIN_ROW_HEIGHT, 400.0);
                if let Some(t) = self.sql.editors.iter_mut().find(|t| t.id == id) {
                    t.row_height = h;
                }
                Task::none()
            }
            GridMessage::RowResizeEnd { id } => {
                self.persist_row_height(id);
                Task::none()
            }
            GridMessage::RowHeightReset { id } => {
                if let Some(t) = self.sql.editors.iter_mut().find(|t| t.id == id) {
                    t.row_height = crate::views::data::DEFAULT_ROW_HEIGHT;
                }
                self.persist_row_height(id);
                Task::none()
            }
        }
    }
}
