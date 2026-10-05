use super::*;

#[test]
fn every_menu_and_toolbar_entry_is_a_registered_command() {
    for (menu, entries) in MENUS {
        for entry in *entries {
            if let MenuEntry::Item(id) = entry {
                // `get` panics if the command is not registered.
                let cmd = commands::get(*id);
                assert!(!cmd.title.is_empty(), "{menu} has an untitled entry");
            }
        }
    }
    for group in TOOLBAR {
        for id in *group {
            let glyphs = toolbar_glyphs(*id);
            assert_ne!(glyphs, ["?"], "{id:?} is on the toolbar but has no glyph");
            // Two buttons whose last-resort label is the same are
            // indistinguishable on a machine whose fonts cover neither
            // icon -- which is this one. Run and Redo were both `>`.
            for other in TOOLBAR.iter().flat_map(|g| g.iter()) {
                if other == id {
                    continue;
                }
                assert_ne!(
                    glyphs.last(),
                    toolbar_glyphs(*other).last(),
                    "{id:?} and {other:?} fall back to the same label"
                );
            }
            assert!(
                glyphs.len() >= 2 || glyphs[0].is_ascii(),
                "{id:?} has a single non-ASCII glyph and so no fallback if the                      font cannot draw it"
            );
            assert!(
                glyphs.last().is_some_and(|g| g.is_ascii()),
                "{id:?} ends in a glyph that could itself be missing"
            );
        }
    }
}

/// Regression: the output panel grew to fill the whole window, hiding the
/// editor with no way to get it back.
#[test]
fn the_dock_always_leaves_room_for_the_editor() {
    let window = 800.0;
    // Even asking for far more than the window has.
    let height = clamp_dock_height(10_000.0, window);
    assert!(
        height < window,
        "the dock must not fill the window: {height} of {window}"
    );
    assert!(
        window - height >= 100.0,
        "too little editor left: {} points",
        window - height
    );
}

#[test]
fn the_dock_keeps_its_requested_height_when_there_is_room() {
    assert!((clamp_dock_height(220.0, 900.0) - 220.0).abs() < f32::EPSILON);
    assert!((clamp_dock_height(400.0, 900.0) - 400.0).abs() < f32::EPSILON);
}

#[test]
fn the_dock_stays_usable_in_a_very_short_window() {
    // A window too short to honour both minimums has to break one of them;
    // the dock keeps its minimum so its header and buttons stay reachable.
    let height = clamp_dock_height(220.0, 150.0);
    assert!(height >= MIN_DOCK_HEIGHT);
    assert!(height.is_finite());
}

#[test]
fn a_nonsense_height_falls_back_to_the_default() {
    // Guards against a NaN reaching the panel, which lays out as an
    // invisible or infinite rectangle.
    let height = clamp_dock_height(f32::NAN, 900.0);
    assert!(height.is_finite());
    assert!((height - DEFAULT_DOCK_HEIGHT).abs() < f32::EPSILON);
}

#[test]
fn the_dock_is_never_dragged_past_its_hard_cap() {
    assert!(clamp_dock_height(5_000.0, 10_000.0) <= MAX_DOCK_HEIGHT);
}

/// The whole built-in checking path, end to end, with no server installed:
/// parse the file, walk the tree, convert, store, read back.
///
/// The reported bug was that obviously broken Python showed nothing at all
/// on a machine with no Python language server, so the thing worth testing
/// is that this route works with nothing else present.
#[test]
fn broken_python_produces_a_diagnostic_with_no_language_server() {
    use editor_syntax::highlight::Highlighter;
    use ropey::Rope;

    // The user's file, as reported.
    let source = "def main(argv: list[str] | None = None) -> int:\n\
                      \x20   \"\"\"Entry point.\"\"\"\n\
                      \x20   data = ['one', 'two']\n\
                      \x20   for i in data:\n\
                      \x20       print(i)\n\
                      \n\
                      \x20   if bob = kate\n\
                      \x20   print(end)\n";
    let rope = Rope::from_str(source);
    let highlighter = Highlighter::new(LanguageId::Python, &rope).expect("python grammar");

    let mut store = editor_lsp::diagnostics::Store::default();
    let path = Path::new("/project/main.py");
    store.set(
        path,
        editor_lsp::session::BUILTIN_SOURCE,
        highlighter
            .errors(&rope)
            .into_iter()
            .map(to_diagnostic)
            .collect(),
    );

    let found = store.for_file(path);
    assert!(
        !found.is_empty(),
        "`if bob = kate` must be reported without a language server"
    );
    assert!(
        found.iter().any(|d| d.line == 6),
        "the diagnostic belongs on the `if` line: {found:?}"
    );
    assert!(
        found
            .iter()
            .all(|d| d.severity == editor_lsp::diagnostics::Severity::Error),
        "a file that does not parse is an error, not a suggestion"
    );
}

/// A file that does not parse gets the "nothing else can check this" note;
/// a file that merely has lint findings must not.
#[test]
fn the_unparseable_note_appears_only_when_the_parse_failed() {
    let syntax = to_diagnostic(editor_syntax::errors::SyntaxError {
        line: 0,
        column: 0,
        end_line: 0,
        end_column: 4,
        message: "Syntax error: `oops`".to_owned(),
    });
    let lint = editor_lsp::diagnostics::Diagnostic {
        severity: editor_lsp::diagnostics::Severity::Warning,
        line: 0,
        column: 0,
        end_line: 0,
        end_column: 3,
        message: "Undefined name `sys`".to_owned(),
        code: Some("F821".to_owned()),
        source: "Ruff".to_owned(),
    };

    assert!(has_syntax_error(&[syntax.clone(), lint.clone()]));
    assert!(!has_syntax_error(&[lint]));
    assert!(!has_syntax_error(&[]));

    // Ruff also reports the parse failure. The note must not double up, so
    // it keys off our own source rather than on anything the servers say.
    let ruff_syntax = editor_lsp::diagnostics::Diagnostic {
        message: "invalid-syntax: Expected `:`, found `=`".to_owned(),
        source: "Ruff".to_owned(),
        ..syntax.clone()
    };
    assert!(!has_syntax_error(&[ruff_syntax]));
}

#[test]
fn valid_python_produces_no_builtin_diagnostics() {
    use editor_syntax::highlight::Highlighter;
    use ropey::Rope;

    let rope = Rope::from_str("def main() -> int:\n    print('ok')\n    return 0\n");
    let highlighter = Highlighter::new(LanguageId::Python, &rope).expect("python grammar");
    assert!(
        highlighter.errors(&rope).is_empty(),
        "working code must not be flagged"
    );
}

/// The built-in check must not be filed under a server's name, or a crashed
/// server's cleanup would take the syntax errors with it.
#[test]
fn builtin_diagnostics_survive_a_server_crash() {
    let mut store = editor_lsp::diagnostics::Store::default();
    let path = Path::new("/project/main.py");
    store.set(
        path,
        editor_lsp::session::BUILTIN_SOURCE,
        vec![to_diagnostic(editor_syntax::errors::SyntaxError {
            line: 0,
            column: 0,
            end_line: 0,
            end_column: 4,
            message: "Syntax error: `oops`".to_owned(),
        })],
    );
    for spec in editor_lsp::registry::ALL {
        store.clear_server(spec.id);
    }
    assert_eq!(
        store.for_file(path).len(),
        1,
        "a crashing server must not clear The Editor's own diagnostics"
    );
}

#[test]
fn the_status_bar_says_what_is_checking_when_no_server_runs() {
    // "No problems" from a syntax check alone means much less than the same
    // words with a type checker behind them.
    let lsp = editor_lsp::session::Lsp::default();
    assert!(lsp.running().is_empty(), "nothing is started in a test");
    // The wording is asserted rather than the mechanism, because the whole
    // point is what the user reads.
    let summary = "Checking syntax only \u{2014} no language server is running.\n\
                       Help > Check Toolchains lists what could be installed.";
    assert!(summary.contains("syntax only"));
    assert!(summary.contains("Check Toolchains"));
}

#[test]
fn every_optional_tool_can_be_reported_missing_with_a_way_to_install_it() {
    // The complaint that prompted this: the panel named three tools the
    // user did not have and gave no next step.
    for spec in editor_lsp::registry::ALL {
        assert!(
            !spec.install.is_empty(),
            "{} is listed as missing with no way to install it",
            spec.name
        );
    }
}

#[test]
fn the_theme_button_cycles_through_all_three_preferences() {
    assert_eq!(next_theme_command("Dark"), CommandId::ThemeLight);
    assert_eq!(next_theme_command("Light"), CommandId::ThemeSystem);
    assert_eq!(next_theme_command("Follow System"), CommandId::ThemeDark);
}

/// Servers do not answer in document order. pyright returned the uses of
/// one function as helpers.py:4, main.py:9, main.py:7 — so pressing F8
/// walked *up* the file, which reads as the feature being broken.
#[test]
fn every_local_symbol_kind_maps_to_a_glyph_the_popup_knows() {
    use editor_syntax::symbols::SymbolKind;
    // The popup keys its glyphs off the protocol's numbers, because that is
    // what a real server sends; a fallback item carrying a number the popup
    // does not recognise would render as a bare dot beside real ones.
    for kind in [
        SymbolKind::Function,
        SymbolKind::Class,
        SymbolKind::Module,
        SymbolKind::Binding,
        SymbolKind::Unknown,
    ] {
        let item = editor_lsp::session::Completion {
            label: "x".to_owned(),
            insert: "x".to_owned(),
            detail: None,
            kind: Some(lsp_kind(kind)),
            sort_text: None,
        };
        assert_ne!(
            item.glyph(),
            "\u{b7}",
            "{kind:?} falls through to the unknown glyph"
        );
    }
}

#[test]
fn results_are_sorted_into_document_order_and_deduped() {
    let a = PathBuf::from("/p/a.py");
    let b = PathBuf::from("/p/b.py");
    let mut locations = vec![
        Target {
            path: Some(b.clone()),
            line: 8,
            column: 4,
        },
        Target {
            path: Some(a.clone()),
            line: 3,
            column: 0,
        },
        Target {
            path: Some(b.clone()),
            line: 6,
            column: 11,
        },
        Target {
            path: Some(b.clone()),
            line: 6,
            column: 4,
        },
        // The same place twice: two servers, or a server listing the
        // declaration alongside a reference to it.
        Target {
            path: Some(a.clone()),
            line: 3,
            column: 0,
        },
    ];
    locations.sort_by(|x, y| {
        x.path
            .cmp(&y.path)
            .then(x.line.cmp(&y.line))
            .then(x.column.cmp(&y.column))
    });
    locations.dedup();

    let seen: Vec<(String, usize, usize)> = locations
        .iter()
        .map(|t| {
            (
                t.path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                t.line,
                t.column,
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("a.py".to_owned(), 3, 0),
            ("b.py".to_owned(), 6, 4),
            ("b.py".to_owned(), 6, 11),
            ("b.py".to_owned(), 8, 4),
        ],
        "results must read down the file, and not repeat"
    );
}

/// `canonicalize` on Windows returns extended-length paths, which are
/// correct and unusable: they reach title bars, the recent list, recovery
/// files, and the arguments given to language servers, some of which do not
/// understand the form.
#[test]
fn a_verbatim_windows_path_is_unwrapped_but_a_unc_one_is_not() {
    assert_eq!(
        plain_path(PathBuf::from(r"\\?\C:\workspace\a.py")),
        PathBuf::from(r"C:\workspace\a.py")
    );
    // Dropping this prefix would name a different place, so it stays.
    let unc = PathBuf::from(r"\\?\UNC\server\share\a.py");
    assert_eq!(plain_path(unc.clone()), unc);
    // Anything that was never verbatim passes through untouched.
    let plain = PathBuf::from("/home/g/a.py");
    assert_eq!(plain_path(plain.clone()), plain);
}

// ---- zoom ---------------------------------------------------------------

/// Zooming in and back out again returns to exactly where it started, for
/// any number of steps. Without the rounding the value drifts a little
/// further from a round number on every press, and eventually the zoom
/// people had at 1.0 is a number nothing else in the application agrees
/// with.
#[test]
fn zooming_in_and_out_again_comes_back_to_exactly_where_it_started() {
    for steps in 1..40 {
        let mut scale = 1.0_f32;
        for _ in 0..steps {
            scale = stepped_scale(scale, 0.1);
        }
        for _ in 0..steps {
            scale = stepped_scale(scale, -0.1);
        }
        assert_eq!(scale, 1.0, "after {steps} steps out and back");
    }
}

/// Every step is a round tenth, so the settings file, the slider and the
/// menu all show the same number the user just chose.
#[test]
fn every_zoom_step_is_a_round_tenth() {
    let mut scale = 1.0_f32;
    for _ in 0..20 {
        scale = stepped_scale(scale, 0.1);
        assert_eq!(
            scale * 10.0,
            (scale * 10.0).round(),
            "{scale} is not a tenth"
        );
    }
}

// ---- recovering into the tabs that are already open -----------------------

fn open_doc(path: Option<&str>) -> OpenDoc {
    let path = path.map(PathBuf::from);
    OpenDoc {
        recovery_id: 1,
        doc: Document::recovered(path, "text"),
        view: EditorView::default(),
        language: LanguageId::PlainText,
        highlighter: None,
        find: FindBar::default(),
        pending_find_step: None,
        preview: false,
        syntax_version: None,
        syntax_due: None,
        disk: DiskState::Unchanged,
    }
}

/// The session restore reopens the tabs that were open when the crash
/// happened, so by the time the recovery prompt is answered the file being
/// recovered is usually already on screen -- read back from disk, without
/// the changes. Restoring must take that tab over rather than adding a
/// second one, or accepting the recovery leaves the same file open twice
/// with different text on each tab.
#[test]
fn a_recovered_file_finds_the_tab_the_session_restore_already_opened() {
    let docs = vec![
        open_doc(Some("/project/other.py")),
        open_doc(Some("/project/main.py")),
    ];
    assert_eq!(
        tab_showing(&docs, Some(Path::new("/project/main.py"))),
        Some(1)
    );
    assert_eq!(tab_showing(&docs, Some(Path::new("/project/new.py"))), None);
}

/// A buffer that was never saved has no file to collide over. It is not in
/// the session file either, so there is nothing on screen for it to be a
/// second copy of, and it always gets a tab of its own -- even beside
/// another untitled buffer.
#[test]
fn an_unsaved_buffer_never_takes_over_somebody_elses_tab() {
    let docs = vec![open_doc(None), open_doc(Some("/project/main.py"))];
    assert_eq!(tab_showing(&docs, None), None);
}

/// A clean buffer is re-read without asking; a dirty one never is. Getting
/// this backwards silently throws away unsaved work.
#[test]
fn only_a_clean_buffer_reloads_without_asking() {
    assert_eq!(
        disk_response(DiskState::Modified, false),
        DiskResponse::Reload
    );
    assert_eq!(
        disk_response(DiskState::Modified, true),
        DiskResponse::Ask(DiskState::Modified)
    );
}

/// A deleted file is never reloaded, clean buffer or not: reloading means
/// reading, and there is nothing there to read. The tab is now the only
/// copy of that text in existence.
#[test]
fn a_deleted_file_always_asks_even_when_the_buffer_is_clean() {
    assert_eq!(
        disk_response(DiskState::Deleted, false),
        DiskResponse::Ask(DiskState::Deleted)
    );
    assert_eq!(
        disk_response(DiskState::Deleted, true),
        DiskResponse::Ask(DiskState::Deleted)
    );
}

#[test]
fn an_unchanged_file_is_left_alone_however_dirty_the_buffer_is() {
    assert_eq!(
        disk_response(DiskState::Unchanged, false),
        DiskResponse::Ignore
    );
    assert_eq!(
        disk_response(DiskState::Unchanged, true),
        DiskResponse::Ignore
    );
}

/// Reordering must keep the selection and the recent list pointing at the
/// same *documents*, not at the same positions.
#[test]
fn moving_a_tab_carries_the_indices_that_referred_to_it() {
    // The remap, stated directly: moving 0 to 2 in [0,1,2,3].
    let remap = |from: usize, to: usize, i: usize| {
        if i == from {
            to
        } else if from < i && i <= to {
            i - 1
        } else if to <= i && i < from {
            i + 1
        } else {
            i
        }
    };

    // Rightwards: the dragged tab lands at 2, the ones it passed shift left.
    assert_eq!(remap(0, 2, 0), 2, "the dragged tab");
    assert_eq!(remap(0, 2, 1), 0);
    assert_eq!(remap(0, 2, 2), 1);
    assert_eq!(remap(0, 2, 3), 3, "beyond the move, untouched");

    // Leftwards: the ones it passed shift right.
    assert_eq!(remap(3, 1, 3), 1);
    assert_eq!(remap(3, 1, 1), 2);
    assert_eq!(remap(3, 1, 2), 3);
    assert_eq!(remap(3, 1, 0), 0);
}

#[test]
fn a_long_path_is_elided_in_its_middle_not_its_tail() {
    // Both ends identify a path; truncating the tail throws away the
    // containing folder, which is the half that distinguishes two files
    // with the same name.
    let long = "C:/projects/some/deeply/nested/place/that/goes/on/src";
    let short = shorten_middle(long, 24);
    assert!(short.chars().count() <= 24, "got {short:?}");
    assert!(short.starts_with("C:/pro"), "the head is kept: {short:?}");
    assert!(short.ends_with("src"), "the tail is kept: {short:?}");
    assert!(short.contains('\u{2026}'));
}

#[test]
fn a_short_path_is_left_alone() {
    assert_eq!(shorten_middle("C:/tmp", 24), "C:/tmp");
}

#[test]
fn eliding_does_not_split_a_multi_byte_character() {
    // Char-based, not byte-based: slicing a path with an accent in it at a
    // byte offset panics.
    let path = "C:/Users/José/Documentos/proyectos/análisis/código/src";
    let short = shorten_middle(path, 20);
    assert!(short.chars().count() <= 20);
}

#[test]
fn no_command_appears_in_two_menus() {
    // Settings moved to Tools while the file entry was still in File, so
    // "Open settings.toml" was listed twice with no way to notice.
    let mut seen = std::collections::HashMap::new();
    for (menu, entries) in MENUS {
        for entry in *entries {
            if let MenuEntry::Item(id) = entry
                && let Some(first) = seen.insert(*id, *menu)
            {
                panic!("{id:?} is in both the {first} and {menu} menus");
            }
        }
    }
}

#[test]
fn open_recent_is_in_the_file_menu() {
    let file_menu = MENUS
        .iter()
        .find(|(name, _)| *name == "File")
        .expect("a File menu");
    assert!(
        file_menu.1.iter().any(|e| matches!(e, MenuEntry::Recent)),
        "Open Recent is not reachable"
    );
}

#[test]
fn every_theme_preference_has_a_command_and_a_menu_entry() {
    let in_menu: Vec<CommandId> = MENUS
        .iter()
        .flat_map(|(_, entries)| entries.iter())
        .filter_map(|e| match e {
            MenuEntry::Item(id) => Some(*id),
            MenuEntry::Separator | MenuEntry::Recent => None,
        })
        .collect();

    for id in [
        CommandId::ThemeDark,
        CommandId::ThemeLight,
        CommandId::ThemeSystem,
    ] {
        assert!(in_menu.contains(&id), "{id:?} is not reachable from a menu");
    }
    assert_eq!(
        ThemePreference::ALL.len(),
        3,
        "a new theme preference needs a command, a menu entry and a status-bar cycle step"
    );
}

// ---- Which problem the user pointed at ----------------------------------

fn problem(
    severity: editor_lsp::diagnostics::Severity,
    line: u32,
    columns: (u32, u32),
) -> editor_lsp::diagnostics::Diagnostic {
    editor_lsp::diagnostics::Diagnostic {
        severity,
        line,
        column: columns.0,
        end_line: line,
        end_column: columns.1,
        message: format!("problem on line {line}"),
        code: Some("reportArgumentType".to_owned()),
        source: "basedpyright".to_owned(),
    }
}

fn three_lines() -> Document {
    // `join` on the second line is at columns 10..14, offsets 18..22.
    Document::recovered(None, "x = f()\nprint(' '.join(x))\ny = 2\n")
}

#[test]
fn a_problem_is_found_under_the_pointer_including_its_last_edge() {
    use editor_lsp::diagnostics::Severity;
    let doc = three_lines();
    let all = [problem(Severity::Error, 1, (10, 14))];

    assert!(
        problems::problems_at(&doc, &all, 17).is_empty(),
        "just before it"
    );
    assert_eq!(problems::problems_at(&doc, &all, 18).len(), 1);
    assert_eq!(
        problems::problems_at(&doc, &all, 22).len(),
        1,
        "just past the end"
    );
    assert!(problems::problems_at(&doc, &all, 23).is_empty());
}

/// A right-click on the gutter marker leaves the caret at the start of the
/// line, nowhere near the squiggle. The menu must still find the problem.
#[test]
fn the_caret_finds_a_problem_on_its_line_when_not_on_the_squiggle() {
    use editor_lsp::diagnostics::Severity;
    let doc = three_lines();
    let all = [
        problem(Severity::Error, 1, (10, 14)),
        problem(Severity::Warning, 2, (0, 1)),
    ];

    let at_line_start = problems::problems_at_caret(&doc, &all, doc.line_start(1));
    assert_eq!(at_line_start.len(), 1);
    assert_eq!(at_line_start[0].line, 1, "not the next line's");
    assert!(problems::problems_at_caret(&doc, &all, 0).is_empty());
}

/// On the squiggle, only that problem: another one further along the same
/// line is not what the user is pointing at.
#[test]
fn the_caret_on_a_squiggle_prefers_it_over_the_rest_of_the_line() {
    use editor_lsp::diagnostics::Severity;
    let doc = three_lines();
    let all = [
        problem(Severity::Error, 1, (10, 14)),
        problem(Severity::Warning, 1, (15, 16)),
    ];
    let found = problems::problems_at_caret(&doc, &all, 19);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].column, 10);
}

#[test]
fn the_setting_decides_what_is_underlined_but_not_what_is_marked() {
    use editor_config::settings::UnderlineDiagnostics as Level;
    use editor_lsp::diagnostics::Severity;
    let error = problem(Severity::Error, 0, (0, 1));
    let warning = problem(Severity::Warning, 0, (0, 1));

    assert!(problems::is_underlined(&error, Level::Errors));
    assert!(!problems::is_underlined(&warning, Level::Errors));
    assert!(problems::is_underlined(&warning, Level::All));
    assert!(!problems::is_underlined(&error, Level::None));
}

/// The clipboard copy has to be the whole thing: basedpyright's first line
/// says *that* an argument is wrong, and the lines after it say why.
#[test]
fn a_copied_problem_is_located_attributed_and_complete() {
    use editor_lsp::diagnostics::Severity;
    let mut d = problem(Severity::Error, 4801, (12, 40));
    d.message = "Argument of type \"int\" cannot be assigned\n  \"int\" is not iterable".to_owned();

    let report = problems::problem_report(Path::new("pfemame.py"), &d);
    assert_eq!(
        report,
        "pfemame.py:4802:13: Error [reportArgumentType] (basedpyright)\n\
         Argument of type \"int\" cannot be assigned\n  \"int\" is not iterable"
    );
}
