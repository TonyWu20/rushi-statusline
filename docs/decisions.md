# Decisions

The decision log for this repo. Each entry records a decision, its
reason, and where it is enforced in code.

## 2026-09-16 — session-scoped usage counters

### Problem

The stats pill accumulated token usage across the TUI process
lifetime. A session switch added the new session's replayed history
onto the old session's totals. The counts never refreshed on switch.

### Decision

Usage events accumulate into a pending batch. Each tick commits the
batch to the counters:

- Session changed, or first tick. The host just replayed that
  session's full usage history. Replace the totals with the batch.
- Session unchanged. The batch is live growth. Add it to the totals.
- No session in the tick payload. Fall back to additive accumulation.

### Host contract relied on

On start and on every session switch, the TUI clears the reply
caches and re-sends the session's usage-bearing events. The switch
paths in the TUI source call clear_replies and send_history. The tick
payload carries the active session name. This is documented in the
ui-extension doc section 4 of the TUI repo.

Why replace instead of add on a switch. The replay is the full log of
the switched-to session. Adding would double count the history.

Also session scoped:

- The ctx metric rides on the last assistant_message input of the
  active session. It commits with the totals on the tick.
- An empty session hides the ctx section. No stale value leaks in.
- The P/G gauge window resets on a switch. Near-zero replay durations
  never show absurd rates.

### Known edge

Old-session events delivered in the last instant before a switch
could ride into the new session's replace. The host swaps the watch
receiver on switch, so this is effectively zero.

### Enforced by

The integration test drives the real binary over stdio. It covers
startup replay, live growth, switch, switch back, an empty session,
and the no-session fallback. Run it with cargo test.

## 2026-09-16 — compaction_summary in kinds

The manifest now lists both assistant_message and compaction_summary.
The code sums both usages into the session totals. The host replay
includes compaction events too. Before this change, live compaction
usage only reached the extension on the next switch or restart.

## 2026-09-16 — regression test in Rust

The regression test lives in the tests dir of this repo. It drives
the real binary through CARGO_BIN_EXE. No new dependencies were
added. cargo test builds the binary and runs the test.

## 2026-09-16 — distribution path

Run nix build in this repo to refresh the result symlink. The TUI
resolves the shipped package through that symlink. Restart the TUI to
pick up a new binary. There is no hot reload.
