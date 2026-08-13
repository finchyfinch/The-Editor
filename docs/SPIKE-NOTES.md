# Spike — virtualised rope rendering

> **The spike itself has been removed.** It answered its question and M2 is
> done;  went with it, and this file is kept as the record of
> what it was for and what it found. The performance budgets it was a
> stand-in for are now measured continuously by
> `cargo bench -p editor-widgets` — see `crates/widgets/benches/budgets.rs`.

**Question.** Can a custom egui widget paint a large file backed by a
`ropey::Rope` inside the 60 fps frame budget, by laying out only the rows that
are actually on screen? And does editing that rope stay fast enough that typing
never stutters?

This is the assumption M2 (5–7 weeks) rests on. It is worth half a day to test
before committing to it.

**Status: built and running; the measurement itself still needs a human at the
keyboard.** What has been confirmed so far is that `spike.exe` compiles clean,
opens, generates a 50,000-line buffer at startup, and produces no stderr or
panic. The paint and edit timings below have *not* been read yet — they require
scrolling and typing in the window, which is the exercise.

## Running it

```bash
cargo spike
```

Release mode is not optional. A debug build measures the debug build, and the
numbers will be off by an order of magnitude.

## What to do

1. **Scroll from top to bottom** of the 50k-line buffer with the wheel and by
   dragging the scrollbar. Watch `Rows painted` and `Paint (avg)`.
2. **Press the 200k and 1M buttons.** Scroll again.
3. **Click into the text and type**, including Enter and Backspace, near the top
   of the file and again near the bottom. Watch `Last edit`.
4. **Untick "Virtualise"** on the 50k buffer, for the contrast. Re-tick it
   before doing anything else; at 1M lines with virtualisation off the window
   will effectively stop responding, which is the point being demonstrated.

## Pass / fail

| Measure | Budget | Meaning |
|---|---|---|
| `Rows painted` | ~60–90 regardless of file size | Virtualisation is working |
| `Paint (avg)` | **< 4000 µs** | Fits the 16.6 ms frame with room for the rest of the UI |
| `Paint (avg)` at 1M lines | same as at 50k | Cost tracks the viewport, not the document |
| `Last edit` | **< 1000 µs** | Rope edits are not the bottleneck |
| Edit at line 1 vs line 999,000 | roughly equal | Rope is behaving as O(log n), not O(n) |

**The assumption is validated if paint cost is flat across all three file
sizes.** If `Paint (avg)` scales with the line count, virtualisation is broken
and the row-range calculation is the place to look.

**The assumption is in trouble if** paint cost is flat but still above ~8000 µs.
That would mean egui's per-galley text layout is too expensive at this row
count, and M2 needs a galley cache keyed on line content before anything else —
see PLAN.md §2.4. It would not invalidate decision D1, but it would move roughly
a week of work to the front of M2.

## What this spike deliberately does not test

Each of these is a real cost that M2 has to absorb, and none of them are
measured here:

- **Syntax highlighting.** Painting one galley per line with a single colour is
  the cheap case. Tree-sitter capture iteration plus per-span galleys will cost
  meaningfully more. M3 should re-run this measurement.
- **Soft wrap**, which breaks the "one row per line" arithmetic that the whole
  virtualisation calculation rests on. This is the single biggest structural
  difference between the spike and the real widget.
- **Non-ASCII text.** The spike maps the caret column to an x position by
  multiplying by the width of `M`. That is wrong for CJK, for combining marks,
  and for any proportional fallback glyph. The real widget must use galley
  cursor mapping. It also means IME is completely untested.
- **Allocation.** `line_text()` allocates a `String` per visible line per frame.
  That is ~80 small allocations a frame — survivable, but the real widget should
  slice the rope without allocating.
- **Selections, undo, multi-cursor, folding, the gutter's diagnostic column.**

## Notes for M2

- The overlay's own timing (`Instant` around the paint loop) measures CPU-side
  layout and shape generation, not GPU submit. It is the right thing to watch,
  because text layout is where the time goes, but total frame time will be
  higher than the figure shown.
- Painting a 4-row margin above and below the viewport prevents blank rows
  during fast scrolling. Keep it.
- egui caches galleys internally by content hash within a frame, which flatters
  these numbers slightly on repeated identical lines. Real source has fewer
  duplicate lines than the synthetic generator produces — the generator should
  be made more varied if this is re-measured seriously.
