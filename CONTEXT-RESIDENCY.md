# Context residency

Date: 2026-10-15. Status: framing agreed on 2026-10-15; see Decisions.
Nothing here is built yet.
Source baseline: `38f7431`.

## Why this note exists

Project guidance (`AGENTS.md` discovery, in progress) and agent skills (next)
both add standing text to every model request. Small models lose quality long
before their context window is full. The harness already encodes that belief:
`pressure_band` in `src/agent.rs` reports yellow at 15% utilization, orange at
25%, and red at 40%. So the useful question is not "does it fit" but "how little
standing context can each turn carry".

Activation decides what enters context. Ejection decides what leaves. With small
windows, ejection matters at least as much, and today the harness only ejects
tool output.

## What exists today

From the trace of the 2026-10-15 pacing task
(`~/.local/state/microgentic/runs/run-R1q399/traces/run-20261015T080403Z.jsonl`,
Qwen3.8-27B-oQ4e-mtp), the first request carried:

| Component | Characters | Estimated tokens |
| --- | --- | --- |
| Base system prompt | 1,825 | 457 |
| User message | 62 | 16 |
| Tool descriptors | 3,686 | about 920 |

Tool descriptors are already twice the size of the system prompt, and the
calculator tool will add more. The context window for this model is unknown to
the harness, so the trace records `pressure_band: "unknown"` for every request.

Existing ejection and compaction mechanisms, all for tool output:

- `compact_retained_tool_results` summarizes old, large tool payloads.
- `coalesce_retained_file_observations` merges repeated reads of a file, and
  never crosses a mutation boundary.
- Repair handoffs use repair-packet compaction only under red pressure.

Each context component in `llm.context_assembly.ledger` already has an
`inclusion_reason` and an estimated token count. That ledger is the natural
place to extend.

## Framing: residency

Treat every item in a request as resident, with a reason to be there and a cost.

Each resident item records:

- an identity and a kind: base prompt, tool descriptor, root guidance, nested
  guidance, skill catalog, skill body, tool result, file observation,
  conversation turn;
- its owner and inclusion reason;
- estimated tokens;
- its position class: stable prefix or volatile tail;
- the turn it was activated and the turn it was last referenced;
- whether it is pinned.

The operations on an item are activate, reference, demote (replace with a stub
or summary), eject, and reactivate. The current tool-result compaction becomes
one demotion rule among several, not a special case.

## Activation

**Guidance.** Root and ancestor `AGENTS.md` files load at the start. Nested files
load the first time the agent touches their directory. That is the first lazily
activated item, and it lands in the current guidance round.

**Skills.** Use progressive disclosure. A small catalog (name and one-line
description per skill) stays resident under a fixed budget. A skill's body loads
only on activation. Two activators are candidates:

- Model-driven: the model calls an activation tool by skill name.
- Harness-driven: deterministic matching of the request and recent turns against
  skill descriptions, with no extra model call.

Small models may not choose skills reliably, so this is a question to measure,
not to assume. Both activators should exist behind a switch so they can be
compared in the same cell.

**Tool descriptors.** These are residents too. Some modes already expose
different tool sets. Activating rarely used tools on demand is a candidate, but
it changes the provider request shape, so it needs its own hypothesis.

## Ejection

**Pinned, never ejected:** the base system prompt, the current user message and
task statement, root guidance, and the skill catalog.

**Candidates:** skill bodies, nested guidance, old tool results (as today), and
file observations made stale by a later edit.

**Signals:**

- Turns since last reference. Define "reference" cheaply and deterministically:
  the model names the skill or guidance path, or a tool call targets files the
  guidance governs.
- The pressure band.
- Exchange boundaries. A new user message in a conversation is the natural point
  to drop items the previous exchange needed.

**Stub on ejection.** Replace an ejected item with one line stating what it was
and how to load it again, for example
`[skill rust-testing unloaded after 3 unused turns; activate it again if needed]`.
The model keeps the knowledge that the item exists.

**Timing.** Prefer ejection at exchange boundaries, so a running task does not
churn. Allow mid-exchange ejection only under red pressure.

## The cost on the other side: prompt reuse

Local servers can reuse the processed prefix of a prompt between requests (the
KV cache). Changing the middle of a prompt discards the reused work after the
change, and the server must process the rest again. On local hardware that is
slow; the pacing task above took 11 minutes in total. Ejection is therefore not
free.

Layout rules that follow:

- Keep stable items first: base prompt, tool descriptors, root guidance, skill
  catalog.
- Append activated items near the point of activation, not into the system
  prompt, so activation does not discard the reused prefix.
- Batch ejections at exchange boundaries, so the cost is paid once.

Prefix reuse behaviour differs by provider and version. Verify it for oMLX and
Ollama before relying on it.

## Measurement

**Measurement-first changes (no policy change):**

- Residency events in the trace: `context.residency.activated`, `referenced`,
  `demoted`, `ejected`, `reactivated`, each with item identity, kind, tokens,
  reason, and turn.
- Time to first token per request, as an explicit trace field. It is derivable
  today from progress events, but not reported.
- Context window discovery from the provider where it is advertised, so the
  pressure band stops reading "unknown" for installed models.

**Metrics per run:**

- resident tokens by kind, per turn (maximum and mean);
- reactivation rate (high means ejection came too early);
- ejected items never needed again (high means ejection works);
- time to first token per request;
- context pressure range;
- the outcome counts from Gate 5 of `EXPERIMENTAL_METHOD.md`.

## Draft hypotheses

IDs are placeholders. Assign real ones when the cells are registered under
`~/Work/Sandbox2/`.

```text
HYP-R-1
Observation: standing guidance and skill bodies stay resident for the whole
  conversation once loaded.
Hypothesis: ejecting unused skill bodies and nested guidance at exchange
  boundaries lowers maximum resident tokens without lowering task success.
Nearest alternative explanation: the ejected text was still helping, and
  success falls even though pressure improves.
Changed variable: ejection policy on or off.
Predicted confirming: maximum resident tokens fall by at least 30%; success
  count unchanged; reactivation rate under 20%.
Predicted refuting: success count falls, or reactivation rate is 40% or more.
Minimum replicate count: 5 per arm, multi-exchange conversations.
```

```text
HYP-R-2
Observation: small models may not choose the right skill on their own.
Hypothesis: harness-driven activation selects the correct skill more often than
  model-driven activation for Qwen3.8 27B.
Nearest alternative explanation: the model chooses well, and deterministic
  matching adds false activations that cost tokens.
Changed variable: activator.
Predicted confirming: correct-activation rate higher by at least 2 in 5 on a
  labelled prompt set, with false activations no higher.
Predicted refuting: equal or lower correct-activation rate.
Minimum replicate count: 5 per prompt in a labelled set.
```

```text
HYP-R-3
Observation: changing the middle of a prompt can discard reused prefix work.
Hypothesis: appending activations at the tail and batching ejections at
  exchange boundaries keeps time to first token within 20% of a no-ejection
  baseline, while inserting into the system prompt does not.
Nearest alternative explanation: the provider does not reuse prefixes, so
  layout does not matter.
Changed variable: layout policy.
Minimum replicate count: 5 per arm, per provider.
```

```text
HYP-R-4 (baseline, run first)
Observation: no measurement yet shows whether standing guidance helps or hurts
  small models on these tasks.
Hypothesis: root guidance of up to 8k tokens improves convention-following
  without lowering success.
Nearest alternative explanation: the guidance tokens cost more attention than
  the guidance is worth.
Changed variable: guidance on or off. A follow-up cell varies the guidance
  share of the window (for example 4%, 8%, and 12.5%) once the limit is a
  run setting.
Minimum replicate count: 5 per arm.
```

Fixed variables for every cell follow Gate 4 of `EXPERIMENTAL_METHOD.md`.

## Sequencing

1. `AGENTS.md` discovery (in progress). It provides the first lazily activated
   item and traces what it discovers.
2. Measurement-first: residency events, time to first token, context window
   discovery.
3. Baseline cell for HYP-R-4.
4. Ejection policy for guidance, with tool-result compaction moved under the
   same policy. Cells for HYP-R-1 and HYP-R-3.
5. Skills: catalog, both activators behind a switch, ejection. Cell for HYP-R-2.

Benchmark mode stays unchanged throughout, unless a cell deliberately varies it
and records that.

## Decisions

Stacey reviewed this draft on 2026-10-15 and agreed the framing and the
sequencing above. The open questions are answered as follows.

**Where skills live.** User skills live in `~/.config/microgentic/skills/`,
beside the personal `AGENTS.md`. Project skills live in `.agents/skills/` in
the repository. Microgentic does not read `~/.claude/skills/` or other agents'
skill directories by default: a catalog of dozens of skills would use a small
model's whole catalog budget.

**Visibility.** `/context` (or Ctrl-O) shows the context panel beside the
conversation. It lists every element of the latest model request with its kind,
estimated tokens, a short preview, and whether it is new this round. `/guidance`
stays as the guidance-specific listing. When residency lands, the panel gains
each item's inclusion reason and residency state. The status bar keeps one
compact resident count beside the context gauge. The trace records the
residency events listed under Measurement.

**Tool descriptors.** Record them in the trace as pinned residents, so their
cost is measured, but do not activate or eject them yet. On-demand tools change
the provider request shape and need their own hypothesis before any work.

**Budget defaults.** All standing context (guidance, skill catalog, and active
skill bodies) stays below the 15% yellow pressure band. Each kind has its own
cap, a fraction of the discovered context window with a floor and a ceiling.
The caps add up to about 20%, so they cannot all be full at once: the combined
15% limit is the binding one. When standing context would pass it, ejection
removes active skill bodies first, then nested guidance. Root guidance and the
catalog are pinned.

| Kind | Fraction of window | Floor | Ceiling |
| --- | --- | --- | --- |
| Guidance (unchanged) | 1/8 | 500 | 8,000 |
| Skill catalog | 1/64 | 250 | 1,000 |
| Active skill bodies, combined | 1/16 | 500 | 4,000 |

When the window is unknown, the skill budgets apply to an assumed 32,000-token
window: 500 for the catalog and 2,000 for active bodies. Guidance keeps its
existing unknown-window budget of 8,000, which is 25% of that assumed window and
breaks the yellow-band rule. Context window discovery in the measurement step
should make an unknown window rare; revisit the guidance fallback then.

**The percentages are variables.** The combined standing-context limit (15%)
and each per-kind fraction are run settings, not constants, so cells can
measure performance at different levels. Expose them as documented advanced
configuration with a matching run flag, never in the everyday interface.
Record the effective values in the trace beside each context ledger. The values
above are only the defaults. Benchmark mode keeps its current fixed behaviour
unless a cell sets these values explicitly and records them.
