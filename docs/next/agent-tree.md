# The agent tree

The agent panel can nest each agent under the agent that spawned it, rather than
listing them flat. This note records the design decision that made it possible,
because the interesting half is not the rendering.

## The parent had to be recorded, not derived

herdr knew every fact about a pane except the one the tree needs: who opened it.
Nothing in the pane, the tab, the workspace, or the process tree answers it —
a spawned agent's pane looks exactly like a pane a human opened.

The parent is knowable at exactly one instant, the spawn, and was thrown away
there. So `agent start` takes an optional `--parent NAME`, and the terminal keeps
it.

## Why the parent's NAME, and not an id

The obvious candidates are the parent's pane id and its terminal id, and both are
wrong for the same reason — neither survives:

| Identifier | Unique | Survives restart | Meaningful to a reader |
|---|---|---|---|
| pane id | within a workspace | no, reassigned | no |
| terminal id | yes | no, regenerated | no |
| agent name | yes, enforced at start | yes, persisted and restored | yes |

The name is also what a consumer already holds on the parent's own row, so
grouping is a self-join on `name` with no id translation. The cost is that a
rename must re-point the children, which `rename_agent_target` does.

## Absent is the ordinary case

Most agents have no parent and never will: anything started by hand, and
everything that predates the field. So `parent_agent` is optional, and the key is
**omitted** rather than serialised as `null` — a consumer testing key presence and
one deserialising into an `Option` then agree.

The rule that follows is the one worth keeping: **an unparented agent renders at
the root; it is never hidden and never dropped.** A panel that loses a working
agent is worse than a panel with no tree.

Three ways a row could vanish, and what each does instead:

1. **No parent recorded** → root.
2. **Parent named but not present** (exited, or not in this snapshot) → root,
   rather than nested under a row that is not there.
3. **A cycle** → every agent in it is emitted at the root. Cycles are rejected at
   spawn, so this should not arise, but "should not happen" is not a reason to
   lose an agent from a session restored from disk.

`tree_ordered_rows` is a pure function over the snapshot, which is what lets all
three be tested directly.

## Sorting

Siblings sort alphabetically **within each level**, case-insensitively, with the
pane id breaking ties so the order is stable rather than dependent on snapshot
arrival order. Case-insensitivity matters for a fleet named by convention, where
a byte-order sort would file every capitalised agent above every lowercase one.

The flat `a-z` mode is kept — the tree supersedes it as the default way to read
the panel, not as the only way.

## What this retires

Numeric label prefixes (`1 ASSISTANT`, `2 ORCH`, `3 WORKER`) existed only to fake
a hierarchy through lexical sorting. A real tree replaces them.

## Navigation follows the view

`ordered_agent_pane_ids` returns tree order in tree mode, so keyboard navigation
moves in the order the panel displays. An ordering used for rendering but not for
navigation is a bug waiting to be reported as "the panel jumps".
