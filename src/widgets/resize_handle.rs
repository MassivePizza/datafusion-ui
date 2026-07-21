//! A thin draggable resize handle for the data grid. Horizontal handles sit at
//! the right edge of a header cell (column width); vertical handles sit at the
//! bottom edge of a row-number gutter cell (uniform row height). Built directly
//! on Iced's `advanced` widget API because Iced has no built-in grid resizer.
//!
//! Behaviour (pane_grid-resizer pattern):
//! - press + drag → emits `on_resize(new_size)` continuously (tracking the
//!   cursor even when it leaves the handle's narrow bounds, since `update`
//!   receives every `CursorMoved`);
//! - release after a real drag → emits `on_release` (the caller persists);
//! - double-click → emits `on_double_click` (autofit for columns, reset for
//!   rows).

use std::time::Instant;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse, renderer};
use iced::{Element, Event, Length, Rectangle, Size};

use crate::theme::palette;

/// Smallest width a column may be dragged to. Shared with the resize handlers
/// so the widget and the app agree on the floor.
pub const MIN_COL_WIDTH: f32 = 48.0;
const MAX_COL_WIDTH: f32 = 2000.0;
/// Smallest height a data row may be dragged to. Shared with the resize
/// handlers so the widget and the app agree on the floor.
pub const MIN_ROW_HEIGHT: f32 = 20.0;
const MAX_ROW_HEIGHT: f32 = 400.0;
/// Thickness of the (invisible) hit area; the visible divider is 1px centered.
const HANDLE_HIT_WIDTH: f32 = 8.0;
/// Two presses within this window count as a double-click.
const DOUBLE_CLICK_MS: u128 = 400;

/// Which dimension the handle resizes.
enum Axis {
    /// Right edge of a column: drags left/right, adjusts width.
    Horizontal,
    /// Bottom edge of a row: drags up/down, adjusts height.
    Vertical,
}

pub struct ResizeHandle<'a, Message> {
    axis: Axis,
    /// The current committed size along `axis` (drag origin).
    current_size: f32,
    min: f32,
    max: f32,
    on_resize: Box<dyn Fn(f32) -> Message + 'a>,
    on_release: Message,
    on_double_click: Message,
}

/// Build a column-resize handle for a column of `current_width`.
/// Double-click emits `on_autofit`.
pub fn resize_handle<'a, Message: 'a>(
    current_width: f32,
    on_resize: impl Fn(f32) -> Message + 'a,
    on_release: Message,
    on_autofit: Message,
) -> ResizeHandle<'a, Message> {
    ResizeHandle {
        axis: Axis::Horizontal,
        current_size: current_width,
        min: MIN_COL_WIDTH,
        max: MAX_COL_WIDTH,
        on_resize: Box::new(on_resize),
        on_release,
        on_double_click: on_autofit,
    }
}

/// Build a row-resize handle for rows of `current_height`.
/// Double-click emits `on_reset`.
pub fn resize_handle_vertical<'a, Message: 'a>(
    current_height: f32,
    on_resize: impl Fn(f32) -> Message + 'a,
    on_release: Message,
    on_reset: Message,
) -> ResizeHandle<'a, Message> {
    ResizeHandle {
        axis: Axis::Vertical,
        current_size: current_height,
        min: MIN_ROW_HEIGHT,
        max: MAX_ROW_HEIGHT,
        on_resize: Box::new(on_resize),
        on_release,
        on_double_click: on_reset,
    }
}

#[derive(Default)]
struct State {
    drag: Option<Drag>,
    last_click: Option<Instant>,
}

struct Drag {
    press_coord: f32,
    press_size: f32,
    moved: bool,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for ResizeHandle<'_, Message>
where
    Message: Clone,
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        match self.axis {
            Axis::Horizontal => Size {
                width: Length::Fixed(HANDLE_HIT_WIDTH),
                height: Length::Fill,
            },
            Axis::Vertical => Size {
                width: Length::Fill,
                height: Length::Fixed(HANDLE_HIT_WIDTH),
            },
        }
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let Size { width, height } = <Self as Widget<Message, Theme, Renderer>>::size(self);
        layout::atomic(limits, width, height)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(pos) = cursor.position() else { return };
                if !bounds.contains(pos) {
                    return;
                }
                let now = Instant::now();
                let is_double = state
                    .last_click
                    .map(|prev| now.duration_since(prev).as_millis() <= DOUBLE_CLICK_MS)
                    .unwrap_or(false);
                if is_double {
                    state.last_click = None;
                    state.drag = None;
                    shell.publish(self.on_double_click.clone());
                } else {
                    state.last_click = Some(now);
                    let press_coord = match self.axis {
                        Axis::Horizontal => pos.x,
                        Axis::Vertical => pos.y,
                    };
                    state.drag = Some(Drag {
                        press_coord,
                        press_size: self.current_size,
                        moved: false,
                    });
                    shell.request_redraw();
                }
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let Some(drag) = state.drag.as_mut() {
                    drag.moved = true;
                    let coord = match self.axis {
                        Axis::Horizontal => position.x,
                        Axis::Vertical => position.y,
                    };
                    let new_size =
                        (drag.press_size + (coord - drag.press_coord)).clamp(self.min, self.max);
                    shell.publish((self.on_resize)(new_size));
                    shell.request_redraw();
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let Some(drag) = state.drag.take() {
                    if drag.moved {
                        shell.publish(self.on_release.clone());
                    }
                    shell.request_redraw();
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        if state.drag.is_some() || cursor.is_over(layout.bounds()) {
            match self.axis {
                Axis::Horizontal => mouse::Interaction::ResizingHorizontally,
                Axis::Vertical => mouse::Interaction::ResizingVertically,
            }
        } else {
            mouse::Interaction::None
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let active = state.drag.is_some() || cursor.is_over(bounds);
        let line = match self.axis {
            Axis::Horizontal => {
                // 1px (2px when active) vertical divider, centered in the hit
                // area.
                let w = if active { 2.0 } else { 1.0 };
                Rectangle {
                    x: bounds.x + (bounds.width - w) / 2.0,
                    y: bounds.y,
                    width: w,
                    height: bounds.height,
                }
            }
            Axis::Vertical => {
                // The grid already draws a 1px divider after each row, so the
                // idle handle stays invisible; only hover/drag shows a line.
                if !active {
                    return;
                }
                Rectangle {
                    x: bounds.x,
                    y: bounds.y + (bounds.height - 2.0) / 2.0,
                    width: bounds.width,
                    height: 2.0,
                }
            }
        };
        let color = if active {
            palette::accent_warm()
        } else {
            palette::border_subtle()
        };
        renderer.fill_quad(
            renderer::Quad {
                bounds: line,
                ..renderer::Quad::default()
            },
            color,
        );
    }
}

impl<'a, Message, Theme, Renderer> From<ResizeHandle<'a, Message>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(handle: ResizeHandle<'a, Message>) -> Self {
        Element::new(handle)
    }
}
