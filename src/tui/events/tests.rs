use super::*;
use crate::{
    config::Config,
    tui::{
        app::{AppModel, Focus, ScrollbarDrag},
        features::{
            feelings::FeelingRow,
            insights::{InsightsTab, InsightsTimeframe},
            location::LocationPreset,
            metadata::EditMetadataFocus,
        },
        render, scroll,
        state::{HoverTarget, ListNav},
        test_support::{app_reading, app_with_entries, app_with_entry, app_with_journals, new_app},
    },
};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use notema_storage::JournalStore;
use ratatui::layout::Rect;
use std::fs;
use tempfile::tempdir;

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    }
}

fn down() -> MouseEventKind {
    MouseEventKind::Down(MouseButton::Left)
}

fn drag() -> MouseEventKind {
    MouseEventKind::Drag(MouseButton::Left)
}

fn up() -> MouseEventKind {
    MouseEventKind::Up(MouseButton::Left)
}

/// Render a frame into a fresh `ViewState`, returning it with the terminal so
/// callers can drive the production translation/dispatch paths against the
/// regions that render registered.
fn render_view(
    app: &mut AppModel,
    w: u16,
    h: u16,
) -> (
    ratatui::Terminal<ratatui::backend::TestBackend>,
    crate::tui::ui::ViewState,
) {
    let backend = ratatui::backend::TestBackend::new(w, h);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    let mut view = crate::tui::ui::ViewState::default();
    let theme = app.appearance.theme.clone();
    terminal
        .draw(|frame| {
            let mut context = crate::tui::ui::RenderContext::new(&theme, &mut view);
            crate::tui::render::draw(frame, app, &mut context);
        })
        .unwrap();
    (terminal, view)
}

/// Render a frame and dispatch the `ViewRendered` the run loop would, so tests
/// exercise the real render-to-model handshake that link-hint labels ride on.
fn render_and_sync(
    app: &mut AppModel,
    w: u16,
    h: u16,
) -> ratatui::Terminal<ratatui::backend::TestBackend> {
    let (mut terminal, mut view) = render_view(app, w, h);
    dispatch_action(
        &mut terminal,
        app,
        Action::ViewRendered {
            reader_scroll: (view.reader.line_count > 0).then_some(view.reader.scroll),
            insights_scroll: (view.insights.total > 0).then_some(view.insights.scroll),
            journal_offset: view.journals.as_ref().map(|journals| journals.offset),
            entry_offset: view.entry_offset,
            reader_hints: std::mem::take(&mut view.reader.hints),
            reader_openable: view.reader.openable,
        },
    )
    .unwrap();
    terminal
}

fn mouse_in_area(app: &mut AppModel, event: MouseEvent, w: u16, h: u16) {
    let (mut terminal, view) = render_view(app, w, h);
    if let Some(action) = mouse::mouse_to_action(app, event, Rect::new(0, 0, w, h), &view, false) {
        dispatch_action(&mut terminal, app, action).unwrap();
    }
}

/// The first cell whose topmost registered interaction region satisfies
/// `predicate`, scanning the frame row-major.
fn find_interaction(
    view: &crate::tui::ui::ViewState,
    w: u16,
    h: u16,
    predicate: impl Fn(&crate::tui::ui::InteractionKind) -> bool,
) -> Option<(u16, u16)> {
    (0..h)
        .flat_map(|row| (0..w).map(move |col| (col, row)))
        .find(|(col, row)| view.interactions.hit(*col, *row).is_some_and(&predicate))
}

/// Render a frame, then drive the production hover path at `(col, row)`.
/// Returns whether the hover target changed — the run loop's repaint signal.
fn apply_hover(app: &mut AppModel, col: u16, row: u16, area: Rect) -> bool {
    let (mut terminal, view) = render_view(app, area.width, area.height);
    mouse::update_hover(&mut terminal, app, col, row, area, &view).unwrap()
}

/// A search box offering `count` tag values, typed into as the event loop would.
/// Past the popup's eight rows the list scrolls, which is what the wheel, the
/// thumb and the arrow keys are all measured against.
fn app_offering_suggestions(count: usize) -> AppModel {
    let mut app = crate::tui::test_support::app_in_temp(|root| {
        let dir = root.join("work").join("2026-07-01");
        fs::create_dir_all(&dir).unwrap();
        for index in 0..count {
            fs::write(
                dir.join(format!("{index}.md")),
                format!(
                    "+++\nschema_version = 1\n\n[entry]\npeople = [\"tag-{index:02}\"]\n\n[time]\ncreated_at = \"2026-07-01T10:{index:02}:00+02:00\"\n+++\n\nbody\n"
                ),
            )
            .unwrap();
        }
    });
    app.begin_search();
    for ch in "people:tag".chars() {
        app.search_input_key(key(KeyCode::Char(ch)));
    }
    assert_eq!(app.search.suggestions.len(), count);
    app
}

fn set_tag_dialog_items(app: &mut AppModel, count: usize) {
    let state = app.edit_metadata_state_mut().unwrap();
    state.all_values = (0..count)
        .map(|index| (format!("tag-{index:02}"), index + 1))
        .collect();
    state.filtered = (0..count).collect();
    state.normalize_list_state();
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

/// A short fullscreen reader folds metadata into the body. The click lands on
/// the drawn Autofill row, including after that row scrolls, and misses the
/// pinned footer rect and a row that has scrolled away.
#[test]
fn scrolling_autofill_follows_the_drawn_row() {
    let mut app = app_reading(&"line\n".repeat(80));
    app.nav.reader_fullscreen = true;
    app.appearance.theme = crate::tui::theme::test_flat_theme();
    let area = Rect::new(0, 0, 100, 18);

    let (_terminal, view) = render_view(&mut app, area.width, area.height);
    let line = view
        .reader
        .autofill_line
        .expect("label scrolls with the body");
    let rect = view.reader.content_rect;
    assert!(
        line >= view.reader.scroll as usize + rect.height as usize,
        "the label starts below the viewport"
    );

    let metadata = app.selected_entry_metadata_values();
    let values = metadata.values();
    let (show_button, _) = app.reader_autofill_state(values);
    let pinned = crate::tui::surface::entry_metadata_layout(
        &app.appearance.theme,
        view.layout.unwrap().reader.unwrap().area,
        values,
        show_button,
    )
    .autofill
    .expect("pinned layout still reserves a button");
    let pinned_click =
        mouse::mouse_to_action(&app, mouse(down(), pinned.x, pinned.y), area, &view, false);
    assert!(
        !matches!(
            pinned_click,
            Some(Action::Mouse(super::action::MouseAction::Autofill))
        ),
        "the empty bottom row is not the scrolling label"
    );
    assert!(!view.reader_autofill_at(rect.x, rect.y + rect.height - 1));

    app.nav.scroll.reader = line as u16;
    let (terminal, view) = render_view(&mut app, area.width, area.height);
    let shown = view.reader.scroll;
    let row = view
        .reader
        .content_rect
        .y
        .saturating_add((line as u16).saturating_sub(shown));
    let col = view.reader.content_rect.x;
    assert!(view.reader_autofill_at(col, row));
    assert_eq!(
        mouse::mouse_to_action(&app, mouse(down(), col, row), area, &view, false),
        Some(Action::Mouse(super::action::MouseAction::Autofill))
    );

    app.hover = HoverTarget::Autofill;
    let (hovered, _) = render_view(&mut app, area.width, area.height);
    let plain = terminal
        .backend()
        .buffer()
        .cell((col, row))
        .unwrap()
        .style();
    let lifted = hovered.backend().buffer().cell((col, row)).unwrap().style();
    assert_ne!(plain.bg, lifted.bg);
    assert_eq!(lifted.bg, app.appearance.theme.hover().bg);
}

mod environment_cases;
mod keyboard_cases;
mod mouse_cases;
mod overlay_cases;
mod paste_cases;
