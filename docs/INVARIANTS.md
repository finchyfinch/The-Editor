# Invariants

The rules the rest of the code relies on, and what stops each from quietly
ceasing to be true. A rule with nothing enforcing it is marked so: that is the
list to shrink.

Test names are given so they can be found with a search; `cargo xtest` runs
them all, and `cargo test -p editor-widgets --bench budgets` runs the budgets.

## Text

| Rule | Enforced by |
|---|---|
| Text changes only through `Document::apply`, `undo` and `redo`, which queue every change for the highlighter and record how lines moved. | The rope is a private field; nothing else can reach it. |
| A transaction and its inverse restore the text exactly, however many edits it holds. | `edit::tests::a_multi_edit_transaction_inverts_as_a_unit`, `many_edits_of_differing_lengths_all_invert_correctly` |
| A document version identifies one text across every document, so caches keyed on it survive a reload. | `document::tests::no_two_documents_share_a_version` |
| The buffer holds only `\n`; the file's own endings are restored on save. | `a_crlf_file_survives_open_and_save_byte_for_byte`, `a_mostly_lf_file_with_stray_crlf_lines_is_all_lf_in_the_buffer` |
| Breakpoints follow the lines they were set on, through any edit. | `a_multi_edit_transaction_shifts_by_every_edit_above`, `undo_moves_lines_back` |

## Files on disk

| Rule | Enforced by |
|---|---|
| A save is atomic, never touches a file it did not create, and keeps hard links, symlinks and permissions. | `save::tests::*` (the symlink and permission tests run on Unix only) |
| A save never writes a character the file's encoding cannot hold; it refuses and says which. | `a_character_the_encoding_cannot_hold_refuses_the_save_and_writes_nothing` |
| Opening a folder runs nothing in it until the folder is trusted. | `an_untrusted_projects_environment_supplies_no_tools_and_is_not_run`, `trust::tests::*` |

## Language servers

| Rule | Enforced by |
|---|---|
| The session is the only record of what each server has been told, so starting afresh there starts afresh everywhere. | `documents_are_offered_again_after_the_project_changes` |
| A server is sent the buffer, and only after its handshake. | `a_server_is_sent_the_buffer_after_its_handshake_and_its_columns_are_converted` |
| Every column crossing the protocol is converted between characters and the server's units. | the test above, and `position::tests::every_column_round_trips` |
| Nothing that starts or stops a server blocks the UI thread. | `looking_for_a_server_does_not_block_the_caller`; stopping is not tested for timing |
| A stopped server takes everything it started with it. | `a_server_that_will_not_stop_is_killed_with_everything_it_started` |

## Interface

| Rule | Enforced by |
|---|---|
| No library crate below `editor-widgets` depends on the toolkit. | `crates/app/tests/toolkit_boundary.rs` |
| Every action is a registered command, and menus, toolbar and palette are built from the registry. | `every_command_id_is_registered`, `every_menu_and_toolbar_entry_is_a_registered_command`, `no_command_appears_in_two_menus` |
| A shortcut fires only on its own modifiers, except where a layout needs Shift to type the key. | `an_extra_modifier_on_a_letter_is_a_different_shortcut`, `altgr_with_a_letter_runs_nothing`, `shift_on_a_punctuation_key_is_forgiven` |
| The gutter is laid out once; painting and clicking read the same geometry. | `every_column_is_its_own_zone_and_the_zones_do_not_overlap`, `every_zone_the_pointer_calls_clickable_is_one_the_click_handler_acts_on` |
| Per-frame cost follows the viewport, not the file. | `crates/widgets/benches/budgets.rs` (PLAN.md §2.4 budgets) |
| Nothing asks the filesystem on every frame. | **Not enforced.** `Environment` caches what used to be; a new per-frame `stat` would not be caught. |

## Errors

| Rule | Enforced by |
|---|---|
| No `unwrap` on a path that touches user data. | `#![deny(clippy::unwrap_used)]` in `editor-core`, `editor-lsp` and `editor-proc`; `cargo xlint` |
| No slice indexing in the text model, where a panic loses a file. | `#![deny(clippy::indexing_slicing)]` in `editor-core` |
