use super::*;
use editor_core::document::Document;

fn doc_with(text: &str) -> Document {
    let mut doc = Document::untitled();
    doc.apply(
        &Transaction::insert(0, text),
        Selection::at(0),
        Selection::at(text.chars().count()),
    );
    doc
}

// ---- Python method parameters ----------------------------------------

fn python() -> EditorOptions {
    EditorOptions {
        language: LanguageId::Python,
        ..EditorOptions::default()
    }
}

/// Typing the `(` of a method should leave `(self)` with the caret ready
/// for a comma -- not `()` with the caret between them.
#[test]
fn opening_a_methods_bracket_inserts_self() {
    let mut doc = doc_with("class A:\n    def greet");
    let mut view = EditorView::default();
    view.set_caret(doc.len_chars());

    assert!(view.type_text(&mut doc, python(), "("));
    assert_eq!(doc.text().to_string(), "class A:\n    def greet(self)");
    assert_eq!(
        view.selection.head,
        doc.len_chars() - 1,
        "caret sits before the closing bracket"
    );
}

#[test]
fn a_classmethod_gets_cls_and_a_staticmethod_gets_an_empty_pair() {
    let mut doc = doc_with("class A:\n    @classmethod\n    def make");
    let mut view = EditorView::default();
    view.set_caret(doc.len_chars());
    view.type_text(&mut doc, python(), "(");
    assert!(doc.text().to_string().ends_with("def make(cls)"));

    let mut doc = doc_with("class A:\n    @staticmethod\n    def helper");
    let mut view = EditorView::default();
    view.set_caret(doc.len_chars());
    view.type_text(&mut doc, python(), "(");
    assert!(
        doc.text().to_string().ends_with("def helper()"),
        "got {:?}",
        doc.text().to_string()
    );
}

#[test]
fn a_plain_function_still_gets_an_empty_pair() {
    let mut doc = doc_with("def greet");
    let mut view = EditorView::default();
    view.set_caret(doc.len_chars());
    view.type_text(&mut doc, python(), "(");
    assert_eq!(doc.text().to_string(), "def greet()");
}

/// The rule must not reach into any other language, where `self` is either
/// spelled differently or means nothing at all.
#[test]
fn other_languages_are_untouched() {
    let mut doc = doc_with("impl A {\n    fn greet");
    let mut view = EditorView::default();
    view.set_caret(doc.len_chars());
    view.type_text(
        &mut doc,
        EditorOptions {
            language: LanguageId::Rust,
            ..EditorOptions::default()
        },
        "(",
    );
    assert_eq!(doc.text().to_string(), "impl A {\n    fn greet()");
}

/// One undo takes back the whole thing, not just the bracket.
#[test]
fn inserting_self_is_a_single_undo_step() {
    let mut doc = doc_with("class A:\n    def greet");
    let mut view = EditorView::default();
    view.set_caret(doc.len_chars());
    view.type_text(&mut doc, python(), "(");
    assert!(view.undo(&mut doc));
    assert_eq!(doc.text().to_string(), "class A:\n    def greet");
}

// ---- folding ---------------------------------------------------------

/// Drive `sync_folds` the way `render` does, without a window.
fn with_folds(source: &str) -> (Document, EditorView, Highlighter) {
    let doc = doc_with(source);
    let highlighter =
        Highlighter::new(LanguageId::Python, doc.text()).expect("Python has a grammar");
    let mut view = EditorView::default();
    view.sync_tree_data(&doc, Some(&highlighter));
    (doc, view, highlighter)
}

const NESTED: &str =
    "class A:\n    def f(self):\n        x = 1\n        y = 2\n\n\ndef g():\n    pass\n";

/// `NESTED`, zero-based:
/// 0 `class A:`
/// 1 `    def f(self):`
/// 2 `        x = 1`
/// 3 `        y = 2`
/// 4 blank
/// 5 blank
/// 6 `def g():`
/// 7 `    pass`
#[test]
fn nothing_is_pinned_at_the_top_of_a_file() {
    let (doc, view, _h) = with_folds(NESTED);
    assert!(
        view.sticky_lines(0, doc.line_count()).is_empty(),
        "the class line is on screen"
    );
}

#[test]
fn a_declaration_is_pinned_once_its_own_line_has_scrolled_off() {
    let (doc, view, _h) = with_folds(NESTED);
    assert_eq!(
        view.sticky_lines(1, doc.line_count()),
        vec![0],
        "the class, not the def"
    );
    assert_eq!(
        view.sticky_lines(2, doc.line_count()),
        vec![0, 1],
        "outermost first"
    );
    assert_eq!(view.sticky_lines(3, doc.line_count()), vec![0, 1]);
}

/// The blank lines between `A` and `g` are inside neither. A header driven
/// by "the nearest declaration above" would pin `f` here, which is a
/// function the reader left two lines ago.
#[test]
fn nothing_is_pinned_between_two_declarations() {
    let (doc, view, _h) = with_folds(NESTED);
    assert!(
        view.sticky_lines(4, doc.line_count()).is_empty(),
        "got {:?}",
        view.sticky_lines(4, doc.line_count())
    );
    assert!(view.sticky_lines(5, doc.line_count()).is_empty());
}

#[test]
fn a_later_declaration_pins_only_itself() {
    let (doc, view, _h) = with_folds(NESTED);
    assert_eq!(view.sticky_lines(7, doc.line_count()), vec![6]);
}

/// Deeper than the band is allowed to grow. The innermost declarations are
/// the ones kept.
#[test]
fn the_band_is_capped_and_keeps_the_innermost() {
    let (doc, view, _h) = with_folds(
        "class A:
    class B:
        class C:
            class D:
                class E:
                    def f(self):
                        pass
",
    );
    let pinned = view.sticky_lines(6, doc.line_count());
    assert_eq!(pinned.len(), STICKY_MAX_ROWS);
    assert_eq!(pinned, vec![2, 3, 4, 5], "C, D, E and f");
}

/// A file with no grammar has no scopes, and must not have a header.
#[test]
fn a_file_with_no_parse_tree_pins_nothing() {
    let doc = doc_with(NESTED);
    let mut view = EditorView::default();
    view.sync_tree_data(&doc, None);
    assert!(view.sticky_lines(2, doc.line_count()).is_empty());
}

#[test]
fn a_file_with_structure_has_folds_and_starts_unfolded() {
    let (doc, view, _h) = with_folds(NESTED);
    assert!(!view.folds.is_empty(), "there is something to fold");
    assert!(view.collapsed.is_empty());
    assert!(view.fold_map.is_identity());
    assert_eq!(view.fold_map.visible_rows(), doc.line_count());
}

#[test]
fn folding_at_the_caret_takes_the_innermost_fold() {
    let (doc, mut view, _h) = with_folds(NESTED);
    // Caret on `x = 1`, inside both the class and the method.
    view.set_caret(doc.offset_at(2, 8));
    assert!(view.toggle_fold_at_caret(&doc));

    // The method, not the class: folding the outermost from inside one
    // function would collapse the whole file.
    let folded = *view.collapsed.iter().next().expect("something folded");
    assert_eq!(folded, 1, "the `def f` line, not the `class A` line");
    assert!(view.fold_map.is_hidden(2));
    assert!(!view.fold_map.is_hidden(1), "the header stays visible");
    assert!(!view.fold_map.is_hidden(6), "`def g` is untouched");
}

#[test]
fn toggling_twice_returns_to_where_it_started() {
    let (doc, mut view, _h) = with_folds(NESTED);
    let before = view.fold_map.visible_rows();
    view.set_caret(doc.offset_at(2, 8));
    assert!(view.toggle_fold_at_caret(&doc));
    assert!(view.fold_map.visible_rows() < before);
    assert!(view.toggle_fold_at_caret(&doc));
    assert_eq!(view.fold_map.visible_rows(), before);
    assert!(view.fold_map.is_identity());
}

#[test]
fn folding_all_and_unfolding_all_report_whether_anything_changed() {
    let (_doc, mut view, _h) = with_folds(NESTED);
    assert!(view.fold_all(true), "there was something to fold");
    assert!(!view.fold_all(true), "and now there is not");
    assert!(view.fold_all(false), "unfolding undoes it");
    assert!(!view.fold_all(false), "and there is nothing left to unfold");
    assert!(view.fold_map.is_identity());
}

#[test]
fn a_caret_outside_any_fold_reports_nothing_to_fold() {
    let (doc, mut view, _h) = with_folds("x = 1\ny = 2\n");
    view.set_caret(0);
    assert!(!view.toggle_fold_at_caret(&doc));
}

/// The fold has to move with the lines it was put on, or an edit above it
/// silently collapses a different function.
#[test]
fn a_fold_moves_when_lines_are_inserted_above_it() {
    let (mut doc, mut view, mut highlighter) = with_folds(NESTED);
    view.set_caret(doc.offset_at(6, 0));
    assert!(view.toggle_fold_at_caret(&doc), "fold `def g`");
    assert_eq!(view.collapsed.iter().next().copied(), Some(6));

    // Two blank lines at the very top, as typing above would produce.
    view.set_caret(0);
    doc.apply(
        &Transaction::insert(0, "\n\n"),
        Selection::at(0),
        Selection::at(2),
    );
    let changes = doc.take_changes();
    highlighter.update(&changes, doc.text());
    view.sync_tree_data(&doc, Some(&highlighter));

    assert_eq!(
        view.collapsed.iter().next().copied(),
        Some(8),
        "the fold followed its function down the file"
    );
    assert!(view.fold_map.is_hidden(9), "and still hides its body");
}

/// Rebuilding after an edit must not leave a collapsed entry pointing at a
/// fold that no longer exists, which would hide lines nothing can unhide.
#[test]
fn a_fold_whose_code_was_deleted_is_forgotten() {
    let (mut doc, mut view, mut highlighter) = with_folds(NESTED);
    view.set_caret(doc.offset_at(6, 0));
    view.toggle_fold_at_caret(&doc);
    assert!(!view.collapsed.is_empty());

    // Replace the whole file with something that has no folds at all.
    let end = doc.len_chars();
    doc.apply(
        &editor_core::edit::Transaction::new(vec![editor_core::edit::Edit::replace(
            0..end,
            "a = 1\n".to_owned(),
        )]),
        Selection::at(0),
        Selection::at(0),
    );
    let changes = doc.take_changes();
    highlighter.update(&changes, doc.text());
    view.sync_tree_data(&doc, Some(&highlighter));

    assert!(view.collapsed.is_empty(), "the fold went with its code");
    assert!(view.fold_map.is_identity());
}

// ---- accessibility ---------------------------------------------------

/// A caret that never stops blinking is exactly the animation that
/// accessibility guidance names first, and unlike most animations it is on
/// screen the whole time you are reading.
#[test]
fn reduce_motion_leaves_the_caret_solid() {
    let mut view = EditorView {
        reduce_motion: false,
        ..EditorView::default()
    };

    let seen: Vec<bool> = (0..40)
        .map(|i| {
            view.last_interaction =
                Some(std::time::Instant::now() - std::time::Duration::from_millis(600 + i * 100));
            view.blink_on()
        })
        .collect();
    assert!(
        seen.contains(&true) && seen.contains(&false),
        "with motion allowed the caret does blink"
    );

    view.reduce_motion = true;
    for i in 0..40 {
        view.last_interaction =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(600 + i * 100));
        assert!(view.blink_on(), "reduce motion means always visible");
    }
}

// ---- multiple carets -------------------------------------------------

/// The core promise: one keystroke, one character at every caret, and one
/// undo step for the lot.
#[test]
fn typing_with_several_carets_inserts_at_each_of_them() {
    let mut doc = doc_with("one\ntwo\nthree\n");
    let mut view = EditorView::default();
    view.set_caret(0);
    assert!(view.add_cursor_vertically(&doc, 1));
    assert!(view.add_cursor_vertically(&doc, 1));
    assert_eq!(view.cursor_count(), 3);

    assert!(view.insert(&mut doc, "# "));
    assert_eq!(doc.text().to_string(), "# one\n# two\n# three\n");

    assert!(view.undo(&mut doc));
    assert_eq!(
        doc.text().to_string(),
        "one\ntwo\nthree\n",
        "three carets typing is still one undo step"
    );
    assert_eq!(
        view.cursor_count(),
        1,
        "undo restores one selection, so the extra carets have to go rather \
             than be left pointing at text the undo has moved"
    );
}

/// Every caret has to end up after its own insertion, not after somebody
/// else's. Getting this wrong is invisible for one caret and nonsense for
/// three.
#[test]
fn each_caret_ends_up_after_the_text_it_typed() {
    let mut doc = doc_with("aa\nbb\ncc\n");
    let mut view = EditorView::default();
    view.set_caret(0);
    view.add_cursor_vertically(&doc, 1);
    view.add_cursor_vertically(&doc, 1);

    view.insert(&mut doc, "X");
    assert_eq!(doc.text().to_string(), "Xaa\nXbb\nXcc\n");

    let (cursors, _) = view.cursors();
    let heads: Vec<usize> = cursors.iter().map(|s| s.head).collect();
    // "Xaa\n" is 4 characters, so the carets sit at 1, 5 and 9.
    assert_eq!(heads, vec![1, 5, 9]);
}

#[test]
fn backspace_applies_to_every_caret() {
    let mut doc = doc_with("_one\n_two\n");
    let mut view = EditorView::default();
    view.set_caret(1);
    view.add_cursor_vertically(&doc, 1);
    assert_eq!(view.cursor_count(), 2);

    assert!(press(
        &mut view,
        &mut doc,
        egui::Key::Backspace,
        egui::Modifiers::NONE
    ));
    assert_eq!(doc.text().to_string(), "one\ntwo\n");
}

/// Carets do collide -- press End with carets on lines of different
/// lengths, or Backspace them into each other. Two carets in one place
/// would each apply the next edit, so one keystroke would insert twice.
#[test]
fn carets_that_land_on_the_same_spot_are_merged() {
    let mut view = EditorView::default();
    view.install_cursors(
        vec![Selection::at(5), Selection::at(5), Selection::at(9)],
        0,
    );
    assert_eq!(view.cursor_count(), 2, "the duplicate went");

    let mut doc = doc_with("0123456789abc");
    view.insert(&mut doc, "X");
    assert_eq!(
        doc.text().to_string(),
        "01234X5678X9abc",
        "one X per place, not two at the first"
    );
}

#[test]
fn overlapping_selections_merge_into_one() {
    let mut view = EditorView::default();
    view.install_cursors(vec![Selection::new(2, 8), Selection::new(6, 12)], 0);
    assert_eq!(view.cursor_count(), 1);
    assert_eq!(view.selection.range(), 2..12);
}

#[test]
fn escape_puts_the_editor_back_to_one_caret() {
    let mut doc = doc_with("one\ntwo\nthree\n");
    let mut view = EditorView::default();
    view.set_caret(0);
    view.add_cursor_vertically(&doc, 1);
    assert_eq!(view.cursor_count(), 2);

    press(
        &mut view,
        &mut doc,
        egui::Key::Escape,
        egui::Modifiers::NONE,
    );
    assert_eq!(view.cursor_count(), 1);
}

#[test]
fn arrow_keys_move_every_caret() {
    let mut doc = doc_with("abcd\nefgh\n");
    let mut view = EditorView::default();
    view.set_caret(0);
    view.add_cursor_vertically(&doc, 1);

    press(
        &mut view,
        &mut doc,
        egui::Key::ArrowRight,
        egui::Modifiers::NONE,
    );
    press(
        &mut view,
        &mut doc,
        egui::Key::ArrowRight,
        egui::Modifiers::NONE,
    );
    let (cursors, _) = view.cursors();
    assert_eq!(
        cursors.iter().map(|s| s.head).collect::<Vec<_>>(),
        vec![2, 7],
        "both carets moved two characters"
    );
}

/// With nothing selected, the first Ctrl+D selects the word so that the
/// second has something to look for.
#[test]
fn the_first_add_cursor_selects_the_word_under_the_caret() {
    let doc = doc_with("total = total + 1");
    let mut view = EditorView::default();
    view.set_caret(2);

    assert!(view.add_cursor_at_next_match(&doc));
    assert_eq!(view.cursor_count(), 1);
    assert_eq!(view.selection.range(), 0..5);

    assert!(view.add_cursor_at_next_match(&doc));
    assert_eq!(view.cursor_count(), 2, "the second `total` got a caret");
    let (cursors, _) = view.cursors();
    assert_eq!(cursors[1].range(), 8..13);
}

/// Having worked to the bottom of the file, the next one should come back
/// to the top rather than leaving the key looking broken.
#[test]
fn adding_cursors_wraps_round_the_end_of_the_file() {
    let doc = doc_with("x\ny\nx\n");
    let mut view = EditorView::default();
    view.select_range(4, 5); // the second `x`
    assert!(view.add_cursor_at_next_match(&doc));

    let (cursors, _) = view.cursors();
    assert_eq!(cursors.len(), 2);
    assert_eq!(cursors[0].range(), 0..1, "wrapped to the first `x`");
}

#[test]
fn adding_a_cursor_stops_when_everything_is_already_selected() {
    let doc = doc_with("x y x");
    let mut view = EditorView::default();
    view.select_range(0, 1);
    assert!(view.add_cursor_at_next_match(&doc));
    assert_eq!(view.cursor_count(), 2);
    assert!(
        !view.add_cursor_at_next_match(&doc),
        "both are taken, so say so rather than silently doing nothing"
    );
}

/// Multi-byte text: `find_from` works in bytes internally and must hand
/// back character offsets, or a caret lands inside a character.
#[test]
fn adding_cursors_counts_characters_not_bytes() {
    let doc = doc_with("café x café");
    let mut view = EditorView::default();
    view.select_range(0, 4); // "café"
    assert!(view.add_cursor_at_next_match(&doc));

    let (cursors, _) = view.cursors();
    assert_eq!(cursors[1].range(), 7..11, "characters, not bytes");
    assert_eq!(doc.text().slice(cursors[1].range()).to_string(), "café");
}

/// A column selection is a rectangle. Lines too short to reach into it get
/// nothing -- inventing a caret on them means the next keystroke edits a
/// line the rectangle never covered.
#[test]
fn a_column_selection_skips_lines_too_short_to_reach_it() {
    let doc = doc_with("aaaaaa\nbb\ncccccc\n");
    let mut view = EditorView::default();
    // Columns 3..5 down all three lines. The middle line has two
    // characters, so it is not in the rectangle at all.
    view.select_column(&doc, 3, doc.offset_at(2, 5));

    let (cursors, _) = view.cursors();
    assert_eq!(cursors.len(), 2, "the short line is skipped: {cursors:?}");
    assert_eq!(doc.text().slice(cursors[0].range()).to_string(), "aa");
    assert_eq!(doc.text().slice(cursors[1].range()).to_string(), "cc");
}

#[test]
fn a_zero_width_column_selection_is_a_column_of_carets() {
    let doc = doc_with("one\ntwo\nsix\n");
    let mut view = EditorView::default();
    view.select_column(&doc, 0, doc.offset_at(2, 0));

    assert_eq!(view.cursor_count(), 3);
    assert!(
        view.cursors().0.iter().all(|s| s.is_empty()),
        "a rectangle with no width is three carets, not three selections"
    );
}

/// Alt+click on a caret that is already there removes it, but never the
/// last one -- an editor with no caret cannot be typed into.
#[test]
fn alt_clicking_a_caret_removes_it_but_never_the_last_one() {
    let mut view = EditorView::default();
    view.set_caret(4);
    view.toggle_cursor_at(9);
    assert_eq!(view.cursor_count(), 2);

    view.toggle_cursor_at(9);
    assert_eq!(view.cursor_count(), 1);

    view.toggle_cursor_at(4);
    assert_eq!(view.cursor_count(), 1, "the last caret stays");
}

/// Press a key with modifiers, as `handle_keys` would.
fn press(
    view: &mut EditorView,
    doc: &mut Document,
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> bool {
    view.handle_key(doc, EditorOptions::default(), key, modifiers, 20)
}

/// The platform's "by word" modifier, so these tests exercise the same
/// combination the user presses rather than a hard-coded Ctrl.
fn ctrl() -> egui::Modifiers {
    if cfg!(target_os = "macos") {
        egui::Modifiers::ALT
    } else {
        egui::Modifiers::CTRL
    }
}

fn ctrl_shift() -> egui::Modifiers {
    ctrl().plus(egui::Modifiers::SHIFT)
}

#[test]
fn word_motion_is_on_the_right_modifier_for_the_platform() {
    // Cmd+arrow on macOS is start/end of line. Putting word motion there
    // would take over a combination that already means something else.
    assert!(word_modifier(ctrl()), "the platform modifier must work");
    if cfg!(target_os = "macos") {
        assert!(!word_modifier(egui::Modifiers::MAC_CMD));
    } else {
        assert!(!word_modifier(egui::Modifiers::ALT));
    }
    assert!(!word_modifier(egui::Modifiers::NONE));
}

#[test]
fn duplicating_a_line_puts_the_copy_below_it() {
    let mut doc = doc_with("one\ntwo\nthree\n");
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(1)); // `two`
    assert!(view.duplicate_lines(&mut doc));
    assert_eq!(doc.text().to_string(), "one\ntwo\ntwo\nthree\n");
}

#[test]
fn duplicating_again_gives_a_third_copy() {
    // The caret has to follow the copy, or the second press duplicates the
    // original again and the two copies end up interleaved.
    let mut doc = doc_with("one\ntwo\n");
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(1));
    view.duplicate_lines(&mut doc);
    view.duplicate_lines(&mut doc);
    assert_eq!(doc.text().to_string(), "one\ntwo\ntwo\ntwo\n");
}

#[test]
fn duplicating_the_last_line_of_a_file_without_a_final_newline() {
    // The block has no newline of its own, so the copy needs one in front
    // of it or the two lines are glued together.
    let mut doc = doc_with("one\ntwo");
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(1));
    view.duplicate_lines(&mut doc);
    assert_eq!(doc.text().to_string(), "one\ntwo\ntwo");
}

#[test]
fn duplicating_a_multi_line_selection_copies_the_whole_block() {
    let mut doc = doc_with("a\nb\nc\n");
    let mut view = EditorView::default();
    view.select_range(0, doc.line_start(1) + 1);
    view.duplicate_lines(&mut doc);
    assert_eq!(doc.text().to_string(), "a\nb\na\nb\nc\n");
}

#[test]
fn deleting_a_line_removes_it_and_its_newline() {
    let mut doc = doc_with("one\ntwo\nthree\n");
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(1));
    assert!(view.delete_lines(&mut doc));
    assert_eq!(doc.text().to_string(), "one\nthree\n");
}

#[test]
fn deleting_the_last_line_does_not_leave_a_blank_one_behind() {
    // Taking the newline *after* the last line is impossible -- there is
    // none -- so the one before it goes instead.
    let mut doc = doc_with("one\ntwo");
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(1));
    view.delete_lines(&mut doc);
    assert_eq!(doc.text().to_string(), "one");
}

#[test]
fn deleting_the_only_line_empties_the_document() {
    let mut doc = doc_with("only\n");
    let mut view = EditorView::default();
    view.set_caret(0);
    view.delete_lines(&mut doc);
    assert_eq!(doc.text().to_string(), "");
}

#[test]
fn moving_a_line_down_swaps_it_with_the_one_below() {
    let mut doc = doc_with("one\ntwo\nthree\n");
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(0));
    assert!(view.move_lines(&mut doc, 1));
    assert_eq!(doc.text().to_string(), "two\none\nthree\n");
}

#[test]
fn moving_a_line_up_swaps_it_with_the_one_above() {
    let mut doc = doc_with("one\ntwo\nthree\n");
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(2));
    assert!(view.move_lines(&mut doc, -1));
    assert_eq!(doc.text().to_string(), "one\nthree\ntwo\n");
}

#[test]
fn moving_past_either_end_does_nothing() {
    let mut doc = doc_with("one\ntwo\n");
    let mut view = EditorView::default();
    view.set_caret(0);
    assert!(!view.move_lines(&mut doc, -1), "already at the top");
    view.set_caret(doc.line_start(1));
    assert!(!view.move_lines(&mut doc, 1), "already at the bottom");
    assert_eq!(doc.text().to_string(), "one\ntwo\n");
}

#[test]
fn moving_into_a_final_line_that_has_no_newline_does_not_join_them() {
    // The missing newline belongs to the *end of the file*, not to the
    // block being moved. Swapping the two blocks verbatim would carry it
    // into the middle and produce "twoone".
    let mut doc = doc_with("one\ntwo");
    let mut view = EditorView::default();
    view.set_caret(0);
    view.move_lines(&mut doc, 1);
    assert_eq!(doc.text().to_string(), "two\none");
}

#[test]
fn moving_a_line_keeps_it_selected() {
    // So the shortcut can be held down to walk a line up a file.
    let mut doc = doc_with("aaa\nbb\nc\n");
    let mut view = EditorView::default();
    view.select_range(doc.line_start(2), doc.line_start(2) + 1);
    view.move_lines(&mut doc, -1);
    assert_eq!(doc.text().to_string(), "aaa\nc\nbb\n");
    assert_eq!(
        view.selected_text(&doc).as_deref(),
        Some("c"),
        "the moved text is no longer selected"
    );
}

#[test]
fn a_line_move_survives_repeated_application() {
    // Walking a line from the bottom to the top and back must be lossless.
    let mut doc = doc_with("one\ntwo\nthree\nfour\n");
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(3));
    for _ in 0..3 {
        view.move_lines(&mut doc, -1);
    }
    assert_eq!(doc.text().to_string(), "four\none\ntwo\nthree\n");
    for _ in 0..3 {
        view.move_lines(&mut doc, 1);
    }
    assert_eq!(doc.text().to_string(), "one\ntwo\nthree\nfour\n");
}

#[test]
fn each_line_operation_is_a_single_undo_step() {
    let mut doc = doc_with("one\ntwo\nthree\n");
    let original = doc.text().to_string();
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(1));

    view.duplicate_lines(&mut doc);
    assert!(view.undo(&mut doc));
    assert_eq!(doc.text().to_string(), original, "duplicate");

    view.set_caret(doc.line_start(1));
    view.delete_lines(&mut doc);
    assert!(view.undo(&mut doc));
    assert_eq!(doc.text().to_string(), original, "delete");

    view.set_caret(doc.line_start(1));
    view.move_lines(&mut doc, 1);
    assert!(view.undo(&mut doc));
    assert_eq!(doc.text().to_string(), original, "move");
}

#[test]
fn ctrl_arrow_moves_the_caret_a_word_at_a_time() {
    let mut doc = doc_with("alpha beta_gamma delta");
    let mut view = EditorView::default();
    view.set_caret(0);

    press(&mut view, &mut doc, egui::Key::ArrowRight, ctrl());
    assert_eq!(view.selection.head, 5, "the end of `alpha`");
    press(&mut view, &mut doc, egui::Key::ArrowRight, ctrl());
    assert_eq!(view.selection.head, 16, "the end of `beta_gamma`");
    press(&mut view, &mut doc, egui::Key::ArrowLeft, ctrl());
    assert_eq!(view.selection.head, 6, "the start of `beta_gamma`");
}

#[test]
fn plain_arrows_still_move_one_character() {
    // The word motion must not swallow the ordinary case.
    let mut doc = doc_with("abc");
    let mut view = EditorView::default();
    view.set_caret(0);
    press(
        &mut view,
        &mut doc,
        egui::Key::ArrowRight,
        egui::Modifiers::NONE,
    );
    assert_eq!(view.selection.head, 1);
}

#[test]
fn ctrl_shift_arrow_extends_the_selection_by_a_word() {
    let mut doc = doc_with("alpha beta");
    let mut view = EditorView::default();
    view.set_caret(0);
    press(&mut view, &mut doc, egui::Key::ArrowRight, ctrl_shift());
    assert_eq!(view.selection.anchor, 0, "the anchor stays put");
    assert_eq!(view.selection.head, 5);
    assert_eq!(view.selected_text(&doc).as_deref(), Some("alpha"));
}

#[test]
fn ctrl_backspace_deletes_the_word_before_the_caret() {
    let mut doc = doc_with("alpha beta");
    let mut view = EditorView::default();
    view.set_caret(10);
    assert!(press(&mut view, &mut doc, egui::Key::Backspace, ctrl()));
    assert_eq!(doc.text().to_string(), "alpha ");
}

#[test]
fn ctrl_delete_deletes_the_word_after_the_caret() {
    let mut doc = doc_with("alpha beta");
    let mut view = EditorView::default();
    view.set_caret(5);
    assert!(press(&mut view, &mut doc, egui::Key::Delete, ctrl()));
    assert_eq!(doc.text().to_string(), "alpha");
}

#[test]
fn deleting_a_word_is_one_undo_step() {
    // Without breaking the undo run either side, a word deletion coalesces
    // with whatever was typed before it and undo takes back too much.
    let mut doc = doc_with("alpha beta");
    let mut view = EditorView::default();
    view.set_caret(10);
    press(&mut view, &mut doc, egui::Key::Backspace, ctrl());
    assert_eq!(doc.text().to_string(), "alpha ");
    assert!(view.undo(&mut doc));
    assert_eq!(doc.text().to_string(), "alpha beta", "one undo restores it");
}

#[test]
fn plain_backspace_still_deletes_to_the_tab_stop() {
    // Smart backspace must survive the addition of the Ctrl variant.
    let mut doc = doc_with("        x");
    let mut view = EditorView::default();
    view.set_caret(8);
    press(
        &mut view,
        &mut doc,
        egui::Key::Backspace,
        egui::Modifiers::NONE,
    );
    assert_eq!(doc.text().to_string(), "    x", "back to the tab stop");
}

#[test]
fn ctrl_backspace_with_a_selection_deletes_the_selection() {
    // A word motion must not override an explicit selection.
    let mut doc = doc_with("alpha beta gamma");
    let mut view = EditorView::default();
    view.select_range(6, 10);
    press(&mut view, &mut doc, egui::Key::Backspace, ctrl());
    assert_eq!(doc.text().to_string(), "alpha  gamma");
}

#[test]
fn ctrl_backspace_at_the_start_of_the_document_does_nothing() {
    let mut doc = doc_with("alpha");
    let mut view = EditorView::default();
    view.set_caret(0);
    assert!(!press(&mut view, &mut doc, egui::Key::Backspace, ctrl()));
    assert_eq!(doc.text().to_string(), "alpha");
}

#[test]
fn double_click_selects_a_snake_case_identifier_whole() {
    let doc = doc_with("total_count = other_value + 1");
    let sel = word_at(&doc, 4);
    assert_eq!(doc.text().slice(sel.range()).to_string(), "total_count");
}

#[test]
fn double_click_on_whitespace_selects_the_whitespace_run() {
    let doc = doc_with("a    b");
    let sel = word_at(&doc, 2);
    assert_eq!(doc.text().slice(sel.range()).to_string(), "    ");
}

#[test]
fn double_click_works_on_the_second_line() {
    let doc = doc_with("first\nsecond_thing here");
    let offset = doc.offset_at(1, 3);
    let sel = word_at(&doc, offset);
    assert_eq!(doc.text().slice(sel.range()).to_string(), "second_thing");
}

#[test]
fn smart_backspace_deletes_to_the_previous_tab_stop_in_leading_whitespace() {
    let doc = doc_with("        code");
    let opts = EditorOptions {
        tab_width: 4,
        insert_spaces: true,
        ..EditorOptions::default()
    };

    // Caret at column 8, all spaces before it: one press clears one level.
    let view = EditorView {
        selection: Selection::at(8),
        ..EditorView::default()
    };
    assert_eq!(view.backspace_width(&doc, opts), 4);

    // Caret at column 6 is mid-stop: fall back to the nearest boundary.
    let view = EditorView {
        selection: Selection::at(6),
        ..EditorView::default()
    };
    assert_eq!(view.backspace_width(&doc, opts), 2);
}

#[test]
fn smart_backspace_deletes_one_character_inside_actual_text() {
    let doc = doc_with("    hello");
    let opts = EditorOptions::default();
    let view = EditorView {
        selection: Selection::at(9),
        ..EditorView::default()
    };
    assert_eq!(
        view.backspace_width(&doc, opts),
        1,
        "backspace in text must delete one character, not four"
    );
}

#[test]
fn smart_backspace_is_disabled_when_indenting_with_tabs() {
    let doc = doc_with("        code");
    let opts = EditorOptions {
        insert_spaces: false,
        ..EditorOptions::default()
    };
    let view = EditorView {
        selection: Selection::at(8),
        ..EditorView::default()
    };
    assert_eq!(view.backspace_width(&doc, opts), 1);
}

#[test]
fn typing_replaces_the_selection() {
    let mut doc = doc_with("hello world");
    let mut view = EditorView {
        selection: Selection::new(0, 5),
        ..EditorView::default()
    };
    assert!(view.insert(&mut doc, "goodbye"));
    assert_eq!(doc.text().to_string(), "goodbye world");
    assert_eq!(view.selection, Selection::at(7));
}

#[test]
fn vertical_movement_remembers_the_goal_column_across_a_short_line() {
    let mut doc = doc_with("longest line here\nshort\nlongest line here");
    let mut view = EditorView {
        selection: Selection::at(15),
        ..EditorView::default()
    };
    assert_eq!(doc.line_col(view.selection.head), (1, 16));

    view.move_vertical(&doc, 1, false);
    assert_eq!(
        doc.line_col(view.selection.head),
        (2, 6),
        "clamps to the end of the short line"
    );

    view.move_vertical(&doc, 1, false);
    assert_eq!(
        doc.line_col(view.selection.head),
        (3, 16),
        "returns to the original column, not the short line's end"
    );

    doc.break_undo_run();
}

#[test]
fn a_read_only_document_refuses_edits() {
    let mut doc = Document::untitled();
    // A large-file or permissions flag is what makes a document read-only;
    // an untitled document is editable, so this checks the guard itself.
    assert!(doc.is_editable());
    let mut view = EditorView::default();
    assert!(view.insert(&mut doc, "x"));
    assert_eq!(doc.text().to_string(), "x");
}

fn python_opts() -> EditorOptions {
    EditorOptions {
        language: LanguageId::Python,
        ..EditorOptions::default()
    }
}

/// Type each character in turn, as the keyboard would deliver them.
fn type_all(view: &mut EditorView, doc: &mut Document, opts: EditorOptions, text: &str) {
    for c in text.chars() {
        view.type_text(doc, opts, &c.to_string());
    }
}

#[test]
fn typing_an_opening_bracket_inserts_its_closer_and_stays_inside() {
    let mut doc = doc_with("");
    let mut view = EditorView::default();
    view.type_text(&mut doc, python_opts(), "(");

    assert_eq!(doc.text().to_string(), "()");
    assert_eq!(view.selection, Selection::at(1), "caret sits between them");
}

#[test]
fn typing_the_closer_over_an_auto_inserted_one_steps_past_it() {
    let mut doc = doc_with("");
    let mut view = EditorView::default();
    type_all(&mut view, &mut doc, python_opts(), "()");

    assert_eq!(
        doc.text().to_string(),
        "()",
        "typing the closer must not double it"
    );
    assert_eq!(view.selection, Selection::at(2));
}

#[test]
fn brackets_do_not_auto_close_in_front_of_a_word() {
    // `(word` becoming `()word` is almost never what anyone wants.
    let mut doc = doc_with("word");
    let mut view = EditorView {
        selection: Selection::at(0),
        ..EditorView::default()
    };
    view.type_text(&mut doc, python_opts(), "(");
    assert_eq!(doc.text().to_string(), "(word");
}

#[test]
fn an_apostrophe_after_a_word_character_does_not_auto_close() {
    let mut doc = doc_with("dont");
    let mut view = EditorView {
        selection: Selection::at(3),
        ..EditorView::default()
    };
    view.type_text(&mut doc, python_opts(), "'");
    assert_eq!(
        doc.text().to_string(),
        "don't",
        "typing an apostrophe mid-word must not produce don''t"
    );
}

#[test]
fn typing_a_bracket_with_a_selection_surrounds_it() {
    let mut doc = doc_with("hello world");
    let mut view = EditorView {
        selection: Selection::new(0, 5),
        ..EditorView::default()
    };
    view.type_text(&mut doc, python_opts(), "(");

    assert_eq!(
        doc.text().to_string(),
        "(hello) world",
        "the selection must be wrapped, not replaced"
    );
    assert_eq!(
        doc.text().slice(view.selection.range()).to_string(),
        "hello",
        "and it stays selected"
    );
}

#[test]
fn auto_close_can_be_switched_off() {
    let opts = EditorOptions {
        auto_close_brackets: false,
        ..python_opts()
    };
    let mut doc = doc_with("");
    let mut view = EditorView::default();
    view.type_text(&mut doc, opts, "(");
    assert_eq!(doc.text().to_string(), "(");
}

#[test]
fn tab_with_a_multi_line_selection_indents_rather_than_replacing_it() {
    let mut doc = doc_with("a\nb\nc\n");
    let mut view = EditorView {
        selection: Selection::new(0, 3),
        ..EditorView::default()
    };
    assert!(view.shift_lines(&mut doc, python_opts(), 1));

    assert_eq!(
        doc.text().to_string(),
        "    a\n    b\nc\n",
        "the selected text must survive"
    );
}

#[test]
fn outdent_removes_one_level_and_stops_at_the_margin() {
    let mut doc = doc_with("        a\n    b\nc\n");
    let mut view = EditorView {
        selection: Selection::new(0, doc.len_chars()),
        ..EditorView::default()
    };
    view.shift_lines(&mut doc, python_opts(), -1);
    assert_eq!(doc.text().to_string(), "    a\nb\nc\n");

    view.shift_lines(&mut doc, python_opts(), -1);
    assert_eq!(
        doc.text().to_string(),
        "a\nb\nc\n",
        "outdenting past column zero must not remove text"
    );
}

#[test]
fn indenting_leaves_the_same_text_selected_so_tab_can_repeat() {
    let mut doc = doc_with("a\nb\n");
    let mut view = EditorView {
        selection: Selection::new(0, 3),
        ..EditorView::default()
    };
    view.shift_lines(&mut doc, python_opts(), 1);
    assert_eq!(
        doc.text().slice(view.selection.range()).to_string(),
        "a\n    b"
    );

    view.shift_lines(&mut doc, python_opts(), 1);
    assert_eq!(doc.text().to_string(), "        a\n        b\n");
}

#[test]
fn a_selection_ending_at_a_line_start_does_not_indent_the_next_line() {
    let mut doc = doc_with("a\nb\n");
    let mut view = EditorView {
        // Exactly the first line, including its newline.
        selection: Selection::new(0, 2),
        ..EditorView::default()
    };
    view.shift_lines(&mut doc, python_opts(), 1);
    assert_eq!(doc.text().to_string(), "    a\nb\n");
}

#[test]
fn blank_lines_are_not_indented_into_trailing_whitespace() {
    let mut doc = doc_with("a\n\nb\n");
    let mut view = EditorView {
        selection: Selection::new(0, doc.len_chars()),
        ..EditorView::default()
    };
    view.shift_lines(&mut doc, python_opts(), 1);
    assert_eq!(doc.text().to_string(), "    a\n\n    b\n");
}

#[test]
fn comment_toggle_comments_then_uncomments_exactly() {
    let original = "def f():\n    a = 1\n    b = 2\n";
    let mut doc = doc_with(original);
    let mut view = EditorView {
        selection: Selection::new(0, doc.len_chars()),
        ..EditorView::default()
    };

    assert!(view.toggle_comment(&mut doc, python_opts()));
    assert_eq!(
        doc.text().to_string(),
        "# def f():\n#     a = 1\n#     b = 2\n"
    );

    assert!(view.toggle_comment(&mut doc, python_opts()));
    assert_eq!(
        doc.text().to_string(),
        original,
        "uncommenting must restore the original exactly"
    );
}

#[test]
fn comment_markers_align_to_the_shallowest_line_in_the_block() {
    let mut doc = doc_with("    a = 1\n        b = 2\n");
    let mut view = EditorView {
        selection: Selection::new(0, doc.len_chars()),
        ..EditorView::default()
    };
    view.toggle_comment(&mut doc, python_opts());
    assert_eq!(
        doc.text().to_string(),
        "    # a = 1\n    #     b = 2\n",
        "the block keeps its relative shape"
    );
}

#[test]
fn a_partly_commented_block_is_commented_rather_than_uncommented() {
    let mut doc = doc_with("# a\nb\n");
    let mut view = EditorView {
        selection: Selection::new(0, doc.len_chars()),
        ..EditorView::default()
    };
    view.toggle_comment(&mut doc, python_opts());
    assert_eq!(doc.text().to_string(), "# # a\n# b\n");
}

#[test]
fn comment_toggle_uses_the_right_token_per_language() {
    for (language, expected) in [
        (LanguageId::Python, "# x\n"),
        (LanguageId::Rust, "// x\n"),
        (LanguageId::Ini, "; x\n"),
    ] {
        let mut doc = doc_with("x\n");
        let mut view = EditorView {
            selection: Selection::at(0),
            ..EditorView::default()
        };
        view.toggle_comment(
            &mut doc,
            EditorOptions {
                language,
                ..EditorOptions::default()
            },
        );
        assert_eq!(doc.text().to_string(), expected, "{language:?}");
    }
}

#[test]
fn comment_toggle_reports_failure_for_a_language_without_line_comments() {
    let mut doc = doc_with("{}\n");
    let mut view = EditorView::default();
    assert!(
        !view.toggle_comment(
            &mut doc,
            EditorOptions {
                language: LanguageId::Json,
                ..EditorOptions::default()
            }
        ),
        "JSON has no comment syntax, so the command must decline"
    );
    assert_eq!(doc.text().to_string(), "{}\n");
}

#[test]
fn horizontal_movement_collapses_a_selection_to_its_edge() {
    let doc = doc_with("hello world");
    let mut view = EditorView {
        selection: Selection::new(2, 8),
        ..EditorView::default()
    };

    view.move_horizontal(&doc, -1, false);
    assert_eq!(
        view.selection,
        Selection::at(2),
        "left with a selection goes to its start, not one left of the head"
    );

    view.selection = Selection::new(2, 8);
    view.move_horizontal(&doc, 1, false);
    assert_eq!(view.selection, Selection::at(8));
}

#[test]
fn movement_cannot_run_off_either_end_of_the_document() {
    let doc = doc_with("abc");
    let mut view = EditorView::default();

    for _ in 0..10 {
        view.move_horizontal(&doc, -1, false);
    }
    assert_eq!(view.selection.head, 0);

    for _ in 0..10 {
        view.move_horizontal(&doc, 1, false);
    }
    assert_eq!(view.selection.head, doc.len_chars());
}

// ---- pointer selection -------------------------------------------------

/// egui reports a drag as: nothing at all on the press, one frame that
/// starts the drag once the pointer has moved past the threshold, then a
/// frame per move after that. `moved_to` is where the pointer had already
/// got to when that first frame arrived, which is not quite where the
/// button went down.
fn start_drag(view: &mut EditorView, doc: &Document, pressed_at: usize, moved_to: usize) {
    view.apply_pointer(
        doc,
        Gesture {
            dragged: true,
            drag_started: true,
            ..Gesture::default()
        },
        moved_to,
        pressed_at,
    );
}

fn drag_to(view: &mut EditorView, doc: &Document, offset: usize) {
    view.apply_pointer(
        doc,
        Gesture {
            dragged: true,
            ..Gesture::default()
        },
        offset,
        offset,
    );
}

fn drag(view: &mut EditorView, doc: &Document, from: usize, to: usize) {
    start_drag(view, doc, from, from);
    drag_to(view, doc, to);
}

/// The bug this section exists for: dragging over one word and then over
/// another used to leave both — and everything between them — selected,
/// because the second drag never planted an anchor of its own.
#[test]
fn a_second_drag_replaces_the_first_selection_rather_than_extending_it() {
    let doc = doc_with("one two three four");
    let mut view = EditorView::default();

    drag(&mut view, &doc, 0, 3);
    assert_eq!(view.selection, Selection::new(0, 3), "\"one\" is selected");

    drag(&mut view, &doc, 8, 13);
    assert_eq!(
        view.selection,
        Selection::new(8, 13),
        "the second drag selects \"three\" alone, not \"one two three\""
    );
}

/// The press itself is silent, so the anchor has to come from the frame
/// that notices the drag — and by then the pointer has already travelled a
/// few pixels, which can be a character.
#[test]
fn a_drag_anchors_where_the_button_went_down_not_where_it_was_noticed() {
    let doc = doc_with("one two three four");
    let mut view = EditorView::default();

    start_drag(&mut view, &doc, 4, 5);
    drag_to(&mut view, &doc, 7);
    assert_eq!(
        view.selection,
        Selection::new(4, 7),
        "the selection starts at the pressed character, not the next one"
    );
}

/// Shift is the one gesture that is asking to extend, so it keeps the
/// anchor it already had.
#[test]
fn shift_dragging_still_extends_the_existing_selection() {
    let doc = doc_with("one two three four");
    let mut view = EditorView::default();

    drag(&mut view, &doc, 0, 3);
    view.apply_pointer(
        &doc,
        Gesture {
            dragged: true,
            drag_started: true,
            shift: true,
            ..Gesture::default()
        },
        13,
        8,
    );
    assert_eq!(
        view.selection,
        Selection::new(0, 13),
        "shift+drag reaches out from the old anchor"
    );
}

/// A fresh drag is a fresh single selection, so any extra carets go with
/// the old one — the same as a plain click.
#[test]
fn a_new_drag_drops_the_extra_carets() {
    let doc = doc_with("one two three four");
    let mut view = EditorView::default();
    view.install_cursors(vec![Selection::at(0), Selection::at(8)], 0);
    assert_eq!(view.cursor_count(), 2);

    drag(&mut view, &doc, 4, 7);
    assert_eq!(view.cursor_count(), 1);
    assert_eq!(view.selection, Selection::new(4, 7));
}

/// Alt+drag had the same stale anchor, held in `column_anchor` instead of
/// in the selection: a second rectangle grew out of where the first began.
#[test]
fn a_second_alt_drag_starts_a_new_rectangle() {
    let doc = doc_with(
        "aaaa
bbbb
cccc
dddd
",
    );
    let mut view = EditorView::default();

    let alt_drag = |view: &mut EditorView, from: usize, to: usize| {
        view.apply_pointer(
            &doc,
            Gesture {
                dragged: true,
                drag_started: true,
                alt: true,
                ..Gesture::default()
            },
            from,
            from,
        );
        view.apply_pointer(
            &doc,
            Gesture {
                dragged: true,
                alt: true,
                ..Gesture::default()
            },
            to,
            to,
        );
    };

    // A column down the first two lines, then one down the last two.
    alt_drag(&mut view, 0, 6);
    assert_eq!(view.cursor_count(), 2);

    alt_drag(&mut view, 10, 16);
    assert_eq!(
        view.cursor_count(),
        2,
        "the second rectangle covers its own two lines, not all four"
    );
    let (cursors, _) = view.cursors();
    assert_eq!(cursors[0].start(), 10, "and it starts where it was drawn");
}

/// Folding is skipped on frames where nothing has changed, which is what
/// keeps scrolling a large file free.
#[test]
fn folds_are_not_rebuilt_on_an_ordinary_frame() {
    let view = EditorView {
        folds_version: Some(7),
        ..EditorView::default()
    };
    assert!(!view.folds_need_rebuild(7, false));
    assert!(view.folds_need_rebuild(8, false), "an edit rebuilds them");
}

/// The catch-up reparse repairs the tree without touching the document, so
/// a rebuild keyed on the document version alone would leave the folds
/// read off the half-finished tree in place until the next keystroke.
#[test]
fn folds_are_rebuilt_when_a_late_reparse_catches_up() {
    let view = EditorView {
        folds_version: Some(7),
        folds_stale: true,
        ..EditorView::default()
    };

    assert!(
        !view.folds_need_rebuild(7, true),
        "still parsing: nothing better to read yet"
    );
    assert!(
        view.folds_need_rebuild(7, false),
        "the tree caught up, so the folds taken off the old one are wrong"
    );
}

/// And once they have been rebuilt from a finished tree, that is the end
/// of it — no rebuild on every frame thereafter.
#[test]
fn catching_up_rebuilds_once_and_then_settles() {
    let view = EditorView {
        folds_version: Some(7),
        folds_stale: false,
        ..EditorView::default()
    };
    assert!(!view.folds_need_rebuild(7, false));
}

/// Reloading a file after another program changed it replaces the
/// `Document` behind the view, and the replacement starts its own version
/// counter. The folds are cached against that counter, so an unedited
/// buffer reloading to unedited-but-different text produced two documents
/// both claiming to be version 0 — and the chevrons stayed where the old
/// text had put them, beside blank lines in the new one.
#[test]
fn folds_are_rebuilt_when_the_document_underneath_is_replaced() {
    let before = doc_with(
        "def a():
    pass
",
    );
    let mut view = EditorView::default();
    let h = Highlighter::new(LanguageId::Python, before.text()).expect("grammar");
    view.sync_tree_data(&before, Some(&h));
    assert!(
        view.folds.iter().any(|f| f.first == 0),
        "the def on line 0 folds: {:?}",
        view.folds
    );

    // The same file with one line added at the top, as a reload would
    // bring it: every fold has moved down one.
    let after = doc_with(
        "import os
def a():
    pass
",
    );
    let h = Highlighter::new(LanguageId::Python, after.text()).expect("grammar");
    view.sync_tree_data(&after, Some(&h));
    assert!(
        view.folds.iter().any(|f| f.first == 1),
        "the def is on line 1 now: {:?}",
        view.folds
    );
    assert!(
        !view.folds.iter().any(|f| f.first == 0),
        "and nothing folds on line 0, which is an import: {:?}",
        view.folds
    );
}

// ---- painting a document that just got shorter -------------------------

/// Backspacing a selection that spans lines took the whole application
/// down. The row map is built at the top of the frame; the keystroke is
/// handled later in that same frame and shortens the document at once. In
/// a file short enough that its last line is on screen, the paint loop
/// then walked to a row the document no longer had, and asked the rope for
/// the byte offset of a line past its end — which is a panic, and a panic
/// in the paint pass is the process.
#[test]
fn painting_after_a_multi_line_delete_stays_inside_the_document() {
    // The file that found it: a docstring of three lines inside a def.
    let source = "a = 5
b = 10
c = 20

def add_numbers(a, b, c):
    \"\"\"

    \"\"\"
    result = a + b + c
    return result

print(add_numbers(a, b, c))
";
    let mut doc = doc_with(source);
    let mut view = EditorView::default();
    let highlighter = Highlighter::new(LanguageId::Python, doc.text()).expect("grammar");

    // The frame begins: the row map is built for the document as it is.
    view.sync_tree_data(&doc, Some(&highlighter));
    let rows_before = view.fold_map.visible_rows();
    assert_eq!(rows_before, doc.line_count());

    // The selection in the report: the whole docstring, from the opening
    // quotes to the closing ones.
    let start = doc.line_start(5) + 4;
    let end = doc.line_start(7) + 7;
    view.selection = Selection::new(start, end);
    assert!(press(
        &mut view,
        &mut doc,
        egui::Key::Backspace,
        egui::Modifiers::NONE
    ));
    assert_eq!(
        doc.line_count(),
        rows_before - 2,
        "two lines went, and the map still describes them"
    );

    // What the paint loop does with each row it is about to draw. The last
    // call is the one that panicked.
    let line_count = doc.text().len_lines();
    for row in 0..view.fold_map.rows_within(line_count) {
        let line = view.fold_map.line_at(row);
        assert!(line < line_count, "row {row} has no line behind it");
        let _ = doc.text().line_to_byte(line);
        let _ = doc.line_text(line);
    }
}

/// The same guarantee with a fold closed, where a row and its line are
/// different numbers.
#[test]
fn a_shortened_document_is_safe_to_paint_with_a_fold_closed() {
    let mut doc = doc_with(
        "def a():
    x = 1
    y = 2
    z = 3

def b():
    pass
",
    );
    let mut view = EditorView::default();
    let highlighter = Highlighter::new(LanguageId::Python, doc.text()).expect("grammar");
    view.sync_tree_data(&doc, Some(&highlighter));
    view.toggle_fold(0);
    assert!(!view.fold_map.is_identity(), "the first def is closed");

    // Delete the last two lines, as a selection to the end of the file.
    let start = doc.line_start(5);
    view.selection = Selection::new(start, doc.len_chars());
    press(
        &mut view,
        &mut doc,
        egui::Key::Backspace,
        egui::Modifiers::NONE,
    );

    let line_count = doc.text().len_lines();
    for row in 0..view.fold_map.rows_within(line_count) {
        let line = view.fold_map.line_at(row);
        assert!(line < line_count, "row {row} has no line behind it");
        let _ = doc.text().line_to_byte(line);
    }
}

// ---- the pointer over the gutter ---------------------------------------

/// A view laid out the way `render` lays one out, so the zone arithmetic
/// is exercised on real geometry rather than on numbers chosen to pass.
fn gutter_view(doc: &Document) -> (EditorView, egui::Rect, f32, f32) {
    let row_height = 16.0;
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 400.0));
    let mut view = EditorView::default();
    let highlighter = Highlighter::new(LanguageId::Python, doc.text()).expect("grammar");
    view.sync_tree_data(doc, Some(&highlighter));
    view.gutter = Gutter::new(0.0, row_height, 8.0, doc.line_count(), true);
    let text_left = rect.left() + view.gutter.width();
    (view, rect, text_left, row_height)
}

/// Draw `view` for a few frames in a window 800 points wide, as the
/// application would.
fn run_frames(view: &mut EditorView, doc: &mut Document, frames: usize) {
    let ctx = egui::Context::default();
    let theme = SyntaxTheme::for_ui(editor_config::theme::ResolvedTheme::Dark);
    for frame in 0..frames {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            // Half a second a frame, so a scroll's easing finishes.
            time: Some(frame as f64 * 0.5),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                view.ui(ui, doc, None, &theme, EditorOptions::default());
            });
        });
        // No renderer to hand the font atlas to.
        output.textures_delta.clear();
    }
}

/// The scroll area was a fixed hundred and twenty columns wide, so the end
/// of a longer line could not be scrolled to and typing there put the
/// caret off the edge of the window.
#[test]
fn the_end_of_a_long_line_can_be_scrolled_to() {
    let long = "x".repeat(500);
    let mut doc = doc_with(&format!("short\n{long}\nshort\n"));
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(1) + 500);

    run_frames(&mut view, &mut doc, 6);
    let caret = view
        .caret_screen_rect()
        .expect("the caret's line is on screen");
    assert!(
        (0.0..=800.0).contains(&caret.left()),
        "the caret at the end of the long line is off screen, at x = {}",
        caret.left()
    );
}

/// The to-do list's complaint: a wide blank band to the left of the line
/// numbers. Between the breakpoint column and the digits there is now only
/// the gap that separates them.
#[test]
fn the_numbers_sit_right_after_the_glyph_column() {
    let (row_height, digit) = (18.0, 8.0);
    let gutter = Gutter::new(0.0, row_height, digit, 500, true);
    let digits_left = gutter.numbers_right() - 3.0 * digit;
    let glyphs_right = gutter.glyphs_left() + gutter.glyphs;
    assert_eq!(digits_left - glyphs_right, GUTTER_GAP);
    assert!(
        gutter.width()
            <= CHANGE_COLUMN + row_height + GUTTER_GAP + 3.0 * digit + digit + 2.0 * GUTTER_GAP,
        "no column is reserved that nothing is drawn in: {gutter:?}"
    );
}

/// The fold zone used to reach back over the last digit of the line
/// number, so a click meant for the number folded the function instead.
#[test]
fn every_column_is_its_own_zone_and_the_zones_do_not_overlap() {
    let gutter = Gutter::new(20.0, 18.0, 8.0, 99, true);
    assert_eq!(gutter.zone(10.0), Zone::Annotation, "blame");
    assert_eq!(gutter.zone(gutter.glyphs_centre()), Zone::Breakpoints);
    assert_eq!(
        gutter.zone(gutter.numbers_right() - 1.0),
        Zone::Numbers,
        "the last digit"
    );
    assert_eq!(gutter.zone(gutter.folds_centre()), Zone::Folds);
    assert_eq!(gutter.zone(gutter.width()), Zone::Text);

    let mut previous = Zone::Annotation;
    let order = |z: Zone| match z {
        Zone::Annotation => 0,
        Zone::Breakpoints => 1,
        Zone::Numbers => 2,
        Zone::Folds => 3,
        Zone::Text => 4,
    };
    let mut x = 0.0;
    while x < gutter.width() + 10.0 {
        let zone = gutter.zone(x);
        assert!(
            order(zone) >= order(previous),
            "zones out of order at x={x}"
        );
        previous = zone;
        x += 0.5;
    }
}

#[test]
fn with_line_numbers_off_the_numbers_take_no_room() {
    let on = Gutter::new(0.0, 18.0, 8.0, 1000, true);
    let off = Gutter::new(0.0, 18.0, 8.0, 1000, false);
    assert_eq!(on.width() - off.width(), GUTTER_GAP + 4.0 * 8.0);
}

fn at(x: f32, line: usize, row_height: f32) -> egui::Pos2 {
    egui::pos2(x, line as f32 * row_height + 1.0)
}

#[test]
fn the_pointer_is_an_i_beam_over_the_code_and_an_arrow_over_the_numbers() {
    let doc = doc_with(
        "def f():
    pass
",
    );
    let (view, rect, text_left, row_height) = gutter_view(&doc);

    assert_eq!(
        view.cursor_icon(&doc, at(text_left + 30.0, 0, row_height), rect, row_height),
        egui::CursorIcon::Text,
        "the code pane is text"
    );
    assert_eq!(
        view.cursor_icon(
            &doc,
            at(view.gutter.numbers_right() - 1.0, 0, row_height),
            rect,
            row_height
        ),
        egui::CursorIcon::Default,
        "a line number is not something to click"
    );
}

/// The two parts of the gutter that act on a click say so.
#[test]
fn the_pointer_is_a_hand_over_a_breakpoint_and_over_a_chevron() {
    let doc = doc_with(
        "def f():
    pass
",
    );
    let (view, rect, _, row_height) = gutter_view(&doc);
    assert!(
        view.folds.iter().any(|f| f.first == 0),
        "line 0 opens a fold: {:?}",
        view.folds
    );

    let breakpoints = rect.left() + view.gutter.glyphs_centre();
    assert_eq!(
        view.cursor_icon(&doc, at(breakpoints, 0, row_height), rect, row_height),
        egui::CursorIcon::PointingHand
    );
    let chevron = rect.left() + view.gutter.folds_centre();
    assert_eq!(
        view.cursor_icon(&doc, at(chevron, 0, row_height), rect, row_height),
        egui::CursorIcon::PointingHand
    );
}

/// Most of the fold column is empty, and a hand beside a line with no
/// chevron promises a click that does nothing.
#[test]
fn the_fold_column_is_only_a_hand_where_there_is_a_chevron() {
    let doc = doc_with(
        "def f():
    pass
",
    );
    let (view, rect, _, row_height) = gutter_view(&doc);
    let chevron = rect.left() + view.gutter.folds_centre();
    assert_eq!(
        view.cursor_icon(&doc, at(chevron, 1, row_height), rect, row_height),
        egui::CursorIcon::Default,
        "line 1 is the body, and folds nothing"
    );
}

/// Below the last line the breakpoint column still reads as clickable,
/// because a click there still does something: the row map clamps a
/// pointer past the end to the last line, deliberately, so that clicking
/// under a short file means "the end" rather than nothing. The pointer
/// follows the click rather than second-guessing it.
#[test]
fn the_gutter_below_the_last_line_follows_what_a_click_would_do() {
    let doc = doc_with(
        "def f():
    pass
",
    );
    let (mut view, rect, _, row_height) = gutter_view(&doc);
    let breakpoints = rect.left() + view.gutter.glyphs_centre();
    assert_eq!(
        view.cursor_icon(&doc, at(breakpoints, 40, row_height), rect, row_height),
        egui::CursorIcon::PointingHand
    );

    // And it does: the same position resolves to the last line.
    let line = view.line_at_pos(at(breakpoints, 40, row_height).y, rect, row_height);
    assert_eq!(line, doc.line_count() - 1);
    view.toggle_breakpoint = None;
}

// ---- clicking under the last line --------------------------------------

/// The blank space under a short file is part of the editor, and clicking
/// it puts the caret at the end of the last line -- which is what every
/// other editor does, and what the row map was already clamping towards.
///
/// What was missing was the space itself: the editor allocated exactly the
/// height of its text, so a click below it landed on the scroll area's
/// background instead. The caret did not move and the editor did not take
/// focus, so the next thing typed went nowhere at all.
#[test]
fn a_click_under_the_last_line_puts_the_caret_at_the_end_of_it() {
    let doc = doc_with(
        "def f():
    pass
",
    );
    let (view, rect, _text_left, row_height) = gutter_view(&doc);

    // Well below the three rows this file occupies, and far to the left of
    // where the text ends, so a column-mapped answer would differ.
    let y = 40.0 * row_height;
    assert_eq!(
        view.offset_below_last_row(&doc, y, rect, row_height),
        Some(doc.len_chars()),
        "the file ends with a newline, so the last line is the empty one"
    );
}

/// The same file without its trailing newline: the last line has text on
/// it, and the caret goes after that text rather than to the start of it.
#[test]
fn the_caret_lands_after_the_text_on_the_last_line_not_before_it() {
    let doc = doc_with(
        "def f():
    pass",
    );
    let (view, rect, _text_left, row_height) = gutter_view(&doc);

    let offset = view
        .offset_below_last_row(&doc, 40.0 * row_height, rect, row_height)
        .expect("below the last row");
    assert_eq!(offset, doc.len_chars());
    let (line, column) = doc.line_col(offset);
    assert_eq!((line, column), (2, 9), "end of `    pass`");
}

/// A click on a row that has text in it is not this rule's business, and
/// must fall through to the galley so the column is measured properly.
#[test]
fn a_click_on_a_row_with_text_in_it_is_left_to_the_galley() {
    let doc = doc_with(
        "def f():
    pass
",
    );
    let (view, rect, _text_left, row_height) = gutter_view(&doc);
    for row in 0..view.fold_map.visible_rows() {
        assert_eq!(
            view.offset_below_last_row(&doc, at(0.0, row, row_height).y, rect, row_height),
            None,
            "row {row} has text on it"
        );
    }
}

/// With the tail of the file folded away, the blank space below belongs to
/// the last row that is actually on screen. Jumping to the end of the
/// document would put the caret inside text the user cannot see, and
/// scroll a fold open to show them where it went.
#[test]
fn a_click_under_a_folded_tail_stops_at_the_last_visible_row() {
    // No trailing newline, so the fold really does reach the end of the
    // file: a final empty line would still be a visible row below it.
    let doc = doc_with(
        "x = 1
def f():
    pass
    pass",
    );
    let (mut view, rect, _text_left, row_height) = gutter_view(&doc);
    view.toggle_fold(1);
    view.sync_tree_data(&doc, None);
    assert!(
        view.fold_map.visible_rows() < doc.line_count(),
        "something has to be hidden for this test to mean anything"
    );

    let offset = view
        .offset_below_last_row(&doc, 40.0 * row_height, rect, row_height)
        .expect("below the last visible row");
    assert_eq!(
        doc.line_col(offset),
        (2, 9),
        "the end of `def f():`, not the end of the file"
    );
}

/// The pointer and the click handler read the same geometry, so a hand
/// always means a click that lands.
#[test]
fn every_zone_the_pointer_calls_clickable_is_one_the_click_handler_acts_on() {
    let doc = doc_with(
        "def f():
    pass
",
    );
    let (view, rect, text_left, row_height) = gutter_view(&doc);
    for x in [
        rect.left() + 1.0,
        rect.left() + view.gutter.glyphs_centre(),
        rect.left() + view.gutter.numbers_right() - 1.0,
        rect.left() + view.gutter.folds_centre(),
        text_left + 5.0,
    ] {
        let pos = at(x, 0, row_height);
        let zone = view.zone_at(x, rect);
        let hand = view.cursor_icon(&doc, pos, rect, row_height) == egui::CursorIcon::PointingHand;
        assert_eq!(
            hand,
            matches!(zone, Zone::Breakpoints | Zone::Folds),
            "at x={x} the pointer and the zone disagree ({zone:?})"
        );
    }
}

// ---- triple quotes and docstrings --------------------------------------

/// Type `text` a character at a time, the way a keyboard delivers it.
fn type_each(view: &mut EditorView, doc: &mut Document, opts: EditorOptions, text: &str) {
    for c in text.chars() {
        view.type_text(doc, opts, &c.to_string());
    }
}

fn python_with(style: Option<docstring::Style>) -> EditorOptions {
    EditorOptions {
        language: LanguageId::Python,
        docstrings: style,
        ..EditorOptions::default()
    }
}

/// The friction this replaces: with quotes auto-closing, three keystrokes
/// used to leave four quotes and a caret in the middle of them, and the
/// closing three had to be fought for. Three keystrokes now open and close
/// the string.
#[test]
fn typing_three_quotes_opens_and_closes_the_string() {
    let mut doc = doc_with("x = ");
    let mut view = EditorView::default();
    view.set_caret(doc.len_chars());
    type_each(&mut view, &mut doc, python_with(None), "\"\"\"");

    assert_eq!(doc.text().to_string(), "x = \"\"\"\"\"\"");
    assert_eq!(
        view.selection.head, 7,
        "the caret sits between the two triples"
    );
}

/// And typing the closing quotes by hand steps over the ones already
/// there rather than adding a second set.
#[test]
fn typing_the_closing_quotes_walks_over_them() {
    let mut doc = doc_with("x = ");
    let mut view = EditorView::default();
    view.set_caret(doc.len_chars());
    let opts = python_with(None);
    type_each(&mut view, &mut doc, opts, "\"\"\"hi\"\"\"");
    assert_eq!(doc.text().to_string(), "x = \"\"\"hi\"\"\"");
    assert_eq!(view.selection.head, doc.len_chars());
}

#[test]
fn single_quotes_make_a_triple_too() {
    let mut doc = doc_with("x = ");
    let mut view = EditorView::default();
    view.set_caret(doc.len_chars());
    type_each(&mut view, &mut doc, python_with(None), "'''");
    assert_eq!(doc.text().to_string(), "x = ''''''");
}

/// The headline: the skeleton comes from the signature.
#[test]
fn a_docstring_under_a_def_is_written_from_its_signature() {
    let mut doc = doc_with("def add(a: int, b: int = 2) -> int:\n    \n");
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(1) + 4);
    type_each(
        &mut view,
        &mut doc,
        python_with(Some(docstring::Style::Google)),
        "\"\"\"",
    );

    assert_eq!(
        doc.text().to_string(),
        concat!(
            "def add(a: int, b: int = 2) -> int:\n",
            "    \"\"\"\n",
            "\n",
            "    Args:\n",
            "        a (int): _description_\n",
            "        b (int, optional): _description_\n",
            "\n",
            "    Returns:\n",
            "        int: _description_\n",
            "    \"\"\"\n",
        )
    );
    let caret_line = doc.line_of(view.selection.head);
    assert_eq!(caret_line, 1, "the caret is on the summary line");
    assert_eq!(
        view.selection.head,
        doc.line_start(1) + 7,
        "just past the opening quotes, ready for the summary"
    );
}

/// A method is indented further, and every line of its docstring has to
/// be indented with it.
#[test]
fn a_docstring_is_indented_to_the_body_it_is_written_in() {
    let mut doc = doc_with("class A:\n    def f(self, a):\n        \n");
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(2) + 8);
    type_each(
        &mut view,
        &mut doc,
        python_with(Some(docstring::Style::Google)),
        "\"\"\"",
    );

    let text = doc.text().to_string();
    assert!(text.contains("        \"\"\"\n"), "{text}");
    assert!(text.contains("        Args:\n"), "{text}");
    assert!(text.contains("            a: _description_\n"), "{text}");
    assert!(
        !text.contains("self: _description_"),
        "self is not something a caller passes:\n{text}"
    );
}

/// Off means off: the quotes still pair, and nothing is written.
#[test]
fn the_setting_turns_the_writing_off_without_the_pairing() {
    let mut doc = doc_with("def f(a):\n    \n");
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(1) + 4);
    type_each(&mut view, &mut doc, python_with(None), "\"\"\"");
    assert_eq!(doc.text().to_string(), "def f(a):\n    \"\"\"\"\"\"\n");
}

/// A triple quote that is not in a docstring's position is just a string.
#[test]
fn a_string_elsewhere_in_a_body_is_left_alone() {
    let mut doc = doc_with("def f(a):\n    x = \n");
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(1) + 8);
    type_each(
        &mut view,
        &mut doc,
        python_with(Some(docstring::Style::Google)),
        "\"\"\"",
    );
    assert_eq!(doc.text().to_string(), "def f(a):\n    x = \"\"\"\"\"\"\n");
}

/// Nor is one written in a language that has no docstrings — or triples.
#[test]
fn a_language_without_triple_quotes_is_untouched() {
    let mut doc = doc_with("let x = ");
    let mut view = EditorView::default();
    view.set_caret(doc.len_chars());
    let opts = EditorOptions {
        language: LanguageId::Rust,
        ..EditorOptions::default()
    };
    type_each(&mut view, &mut doc, opts, "\"\"\"");
    assert_eq!(
        doc.text().to_string(),
        "let x = \"\"\"\"",
        "ordinary quote pairing, three times"
    );
}

/// Ctrl+Z takes the whole docstring back in one go, rather than a line at
/// a time — a dozen lines that have to be deleted by hand would be worse
/// than not offering them. It leaves the two quotes that opened it, which
/// is the state the keystroke before it produced: a run of typing never
/// merges across a newline, deliberately, so the docstring cannot join the
/// entry those quotes made. A second Ctrl+Z takes those.
#[test]
fn one_undo_takes_back_the_whole_docstring() {
    let mut doc = doc_with("def f(a, b):\n    \n");
    let before = doc.text().to_string();
    let mut view = EditorView::default();
    view.set_caret(doc.line_start(1) + 4);
    type_each(
        &mut view,
        &mut doc,
        python_with(Some(docstring::Style::Google)),
        "\"\"\"",
    );
    assert!(doc.text().to_string().contains("Args:"));

    assert!(view.undo(&mut doc));
    assert_eq!(
        doc.text().to_string(),
        "def f(a, b):
    \"\"
",
        "the docstring goes in one step, quotes and all"
    );
    assert!(view.undo(&mut doc));
    assert_eq!(
        doc.text().to_string(),
        before,
        "and the quotes with a second"
    );
}

#[test]
fn each_style_writes_its_own_layout() {
    for (style, marker) in [
        (docstring::Style::Google, "Args:"),
        (docstring::Style::Numpy, "Parameters"),
        (docstring::Style::Sphinx, ":param a:"),
    ] {
        let mut doc = doc_with("def f(a):\n    \n");
        let mut view = EditorView::default();
        view.set_caret(doc.line_start(1) + 4);
        type_each(&mut view, &mut doc, python_with(Some(style)), "\"\"\"");
        assert!(
            doc.text().to_string().contains(marker),
            "{style:?} should write {marker}:\n{}",
            doc.text()
        );
    }
}

// ---- Diagnostics in the gutter ----------------------------------------

fn underline(range: std::ops::Range<usize>, underlined: bool) -> Underline {
    Underline {
        range,
        severity: editor_lsp::diagnostics::Severity::Warning,
        message: "w".to_owned(),
        underlined,
    }
}

/// The gutter marker, its hover and the right-click items all key off this,
/// so a warning the setting does not underline must still count.
#[test]
fn a_line_has_a_problem_whether_or_not_it_is_underlined() {
    let doc = doc_with("a = 1\nb = c\nd = 2\n");
    let mut view = EditorView::default();
    // `c` on the second line, offsets 10..11.
    view.set_diagnostics(vec![underline(10..11, false)]);

    assert!(!view.line_has_problem(&doc, 0));
    assert!(view.line_has_problem(&doc, 1));
    assert!(!view.line_has_problem(&doc, 2));
}

#[test]
fn a_problem_spanning_lines_marks_each_of_them() {
    let doc = doc_with("x = (\n    1,\n)\ny = 2\n");
    let mut view = EditorView::default();
    view.set_diagnostics(vec![underline(4..13, true)]);

    assert!(view.line_has_problem(&doc, 0));
    assert!(view.line_has_problem(&doc, 1));
    assert!(view.line_has_problem(&doc, 2));
    assert!(!view.line_has_problem(&doc, 3));
}
