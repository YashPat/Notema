//! Model setup dialog. Same frame as settings: a list, the highlighted row's
//! description, and a hint bar. Field rows paint their input over the list.

use super::*;

use unicode_width::UnicodeWidthStr;

use crate::tui::entry_rows::wrap_text;
use crate::tui::features::model_setup::{ASKING_OLLAMA, ModelSetupState};
use crate::tui::ui::interaction::TextFieldId;

const MODEL_SETUP_WIDTH: u16 = 52;
const MODEL_SETUP_MAX_VISIBLE_ROWS: u16 = 12;
const MODEL_SETUP_DESC_SPACER: u16 = 1;
const MODEL_SETUP_HINTS_SPACER: u16 = 1;
const MODEL_SETUP_DESC_MAX_ROWS: usize = 4;

const HINT_SETTINGS: [Hint; 1] = [Hint::new(
    "return to settings",
    "esc",
    HintId::ModelSetupBack,
)];
const HINT_BACK: [Hint; 1] = [Hint::new("back", "esc", HintId::ModelSetupBack)];

pub(crate) fn model_setup_hints(state: &ModelSetupState) -> &'static [Hint] {
    if state.stack.len() <= 1 {
        &HINT_SETTINGS
    } else {
        &HINT_BACK
    }
}

fn hint_rows(theme: &Theme, frame_area: Rect, state: &ModelSetupState) -> u16 {
    hint_height(
        model_setup_hints(state),
        dialog_hint_width(theme, frame_area, MODEL_SETUP_WIDTH),
    )
}

fn description_rows(theme: &Theme, frame_area: Rect, state: &ModelSetupState) -> u16 {
    let probe = centered_rect_fixed_size(MODEL_SETUP_WIDTH, 1, frame_area);
    let width = dialog_content_full(theme, probe).width.max(1) as usize;
    let mut tallest = (0..state.item_count())
        .map(|index| {
            wrap_text(
                &state.row_description(index),
                width,
                MODEL_SETUP_DESC_MAX_ROWS,
            )
            .len()
        })
        .max()
        .unwrap_or(1);
    if state.on_local() {
        tallest = tallest.max(wrap_text(ASKING_OLLAMA, width, MODEL_SETUP_DESC_MAX_ROWS).len());
    }
    tallest.clamp(1, MODEL_SETUP_DESC_MAX_ROWS) as u16
}

#[derive(Clone, Copy)]
pub(crate) struct ModelSetupLayout {
    pub(crate) area: Rect,
    pub(crate) list: Rect,
    pub(crate) description: Rect,
    pub(crate) hints: Rect,
}

pub(crate) fn model_setup_layout(
    theme: &Theme,
    frame_area: Rect,
    state: &ModelSetupState,
) -> ModelSetupLayout {
    let hint_height = hint_rows(theme, frame_area, state);
    let desc_rows = description_rows(theme, frame_area, state);
    let visible = (state.item_count() as u16).clamp(1, MODEL_SETUP_MAX_VISIBLE_ROWS);
    let height = (dialog_frame_rows(theme)
        + visible
        + MODEL_SETUP_DESC_SPACER
        + desc_rows
        + MODEL_SETUP_HINTS_SPACER
        + hint_height)
        .min(frame_area.height.saturating_sub(2));
    let area = centered_rect_fixed_size(MODEL_SETUP_WIDTH, height, frame_area);
    let inner = dialog_content_full(theme, area);
    let list_height = inner.height.saturating_sub(
        MODEL_SETUP_DESC_SPACER + desc_rows + MODEL_SETUP_HINTS_SPACER + hint_height,
    );
    let list = Rect {
        x: inner.x,
        y: inner.y,
        width: dialog_list_width(theme, inner.width, state.item_count(), list_height),
        height: list_height,
    };
    let description = Rect {
        height: desc_rows,
        ..dialog_row(inner, list_height + MODEL_SETUP_DESC_SPACER)
    };
    ModelSetupLayout {
        area,
        list,
        description,
        hints: dialog_hints_rect(inner, hint_height),
    }
}

pub(crate) struct ModelFieldHit {
    pub(crate) id: TextFieldId,
    pub(crate) rect: Rect,
}

pub(crate) fn model_setup_fields(
    layout: &ModelSetupLayout,
    state: &ModelSetupState,
) -> Vec<ModelFieldHit> {
    let offset = state.offset();
    let mut hits = Vec::new();
    for row in 0..layout.list.height as usize {
        let index = offset + row;
        let Some(id) = state.field_id_at(index) else {
            continue;
        };
        let rect = field_rect(layout.list, row as u16, &state.row_label(index));
        if rect.width > 0 {
            hits.push(ModelFieldHit { id, rect });
        }
    }
    hits
}

fn field_prefix(label: &str) -> String {
    format!("  {label} ")
}

fn field_rect(list: Rect, row: u16, label: &str) -> Rect {
    let used = UnicodeWidthStr::width(field_prefix(label).as_str()) as u16;
    Rect {
        x: list.x.saturating_add(used),
        y: list.y.saturating_add(row),
        width: list.width.saturating_sub(used),
        height: 1,
    }
}

pub(in crate::tui::render) fn draw_model_setup(
    theme: &Theme,
    frame: &mut Frame<'_>,
    state: &mut ModelSetupState,
    hover: HoverTarget,
) {
    let layout = model_setup_layout(theme, frame.area(), state);
    let rows_len = state.item_count();
    let scroll = state.clamp_offset(layout.list.height);
    let selected = state.selected_index();
    let hovered_row = hovered_dialog_row(hover);

    draw_dialog_frame_wide(theme, frame, layout.area, state.title(), true);

    let items: Vec<ListItem<'_>> = (0..rows_len)
        .map(|index| {
            let label = state.row_label(index);
            let text = if state.field_id_at(index).is_some() {
                field_prefix(&label)
            } else {
                format!("  {label}")
            };
            let item = ListItem::new(Line::from(text));
            lift_hovered_row(item, theme, index, hovered_row, selected)
        })
        .collect();
    let list = List::new(items).highlight_style(theme.selection());
    let mut render_state = list_state_for_render(selected, scroll, layout.list.height, true);
    frame.render_stateful_widget(list, layout.list, &mut render_state);

    let fields: Vec<(usize, Rect)> = (0..layout.list.height as usize)
        .filter_map(|row| {
            let index = scroll + row;
            state.field_id_at(index)?;
            Some((
                index,
                field_rect(layout.list, row as u16, &state.row_label(index)),
            ))
        })
        .collect();
    for (index, rect) in fields {
        let focused = selected == Some(index);
        let hovered = super::hovered_field(hover, rect);
        if let Some(input) = state.field_input_mut(index) {
            input.render_in(theme, frame, rect, focused, hovered);
        }
    }

    let description = state
        .selected_index()
        .map(|index| state.row_description(index))
        .unwrap_or_default();
    let desc_lines: Vec<Line<'_>> = wrap_text(
        &description,
        layout.description.width as usize,
        layout.description.height as usize,
    )
    .into_iter()
    .map(|line| Line::from(Span::styled(line, theme.muted())))
    .collect();
    frame.render_widget(Paragraph::new(desc_lines), layout.description);

    render_hint_line(theme, frame, model_setup_hints(state), layout.hints, hover);
    render_dialog_list_scrollbar(theme, frame, layout.list, rows_len, scroll, true);
}
