# ADR-0109 — The step budget counts taps in a browser, because a handler that exists is not a tap a thumb can reach

**Status** Accepted · **Owner** @maintainers-ui · **Date** 2026-09-05
· Completes the gap [`gate-register.md`](../gate-register.md) **A4** names
· Extends [ADR-0020](0020-i18n-runtime.md)'s front-end gates
· Relates to [ADR-0018](0018-http-websocket-stack.md) (the edge serves the built UI),
[ADR-0030](0030-pairing-and-offline-auth.md) (pairing and sign-in),
[ADR-0084](0084-device-authentication.md) (the boot gate a harness must pass)

## The problem

`docs/ui-ux.md` §6 states design principle 1 as a rule with a number in it: a common action takes at
most **two** taps from the role's home screen, a rare one at most **three**. `ui/scripts/step-budget.mjs`
enforces it, and does more than count — it resolves every declared tap against the source, so the
route must exist in `App.tsx`, its screen must exist, and the named action must actually be invoked
by an interactive element on that screen. A renamed or deleted handler fails a pull request.

Its own header states what it cannot do, and has since it was written:

> It cannot see a tap nobody declared. Add a required confirm dialog to the pay flow and leave this
> file alone, and the gate stays green while the flow is one tap worse.

That is not a small hole. It is the *only* way the budget is realistically breached. Nobody edits
`step-budget.mjs` to raise a number — the number is the thing they would have to argue for. What
happens instead is that a dialog, an "are you sure", a required field or a second confirm gets added
to a screen for a good local reason, the declaration is not touched because nothing points at it, and
the gate reports the flow at its old cost forever. The rule decays exactly one convenient extra
dialog at a time, which is the sentence the script's own header uses.

**And the static resolution cannot close it, in principle.** The analyser reads the source and asks
"does an interactive element on this screen call `fire`?". It cannot ask "and is `fire` reachable
without touching anything else first", because that is a question about the rendered page, not about
the syntax tree. Every answer to it needs something that renders.

## The decision

**1. A browser drives the built app and replays each declared flow. Playwright, as a dev
dependency of `ui/`.**

This is the dependency this record exists to authorise (`AGENTS.md` §2: a dependency needs a merged
ADR first). It is `devDependencies` only and never reaches a shipped bundle — the edge serves
`ui/dist`, which Vite builds from `src/`, and a test runner is not in that graph.

**2. Replay, not search.** The harness does not try to *discover* the cheapest path through the UI.
Deciding the shortest interaction sequence to a goal is not something a script gets right, and a
gate that guesses is a gate that gets disabled. Instead it walks **exactly the taps the declaration
names, in order**, and asserts the flow reaches its stated outcome.

That is precisely the shape that catches the hole. Insert a required confirmation into the pay flow
and the declared three taps no longer end in a paid bill — the third tap lands on a dialog instead
of on the money. The gate goes red **with the declaration untouched**, which is the case the current
script is blind to and the whole reason for the cost.

**3. The two gates lock together, and neither is sufficient alone.** A browser cannot know that a
button is "the one that calls `fire`" — the source symbol in the declaration is not a selector. So
each declared tap's element carries `data-step="<action>"`, and:

* the **static** gate is extended to require that the element it already resolves also carries the
  attribute, so an attribute cannot name a handler that does not exist, and a handler cannot be
  declared without one; and
* the **browser** gate finds the element by that attribute and clicks it, so an attribute cannot
  point at something unreachable.

Neither half can be satisfied by lying to the other. That is the property worth having: the
attribute is not test scaffolding bolted on, it is the join between a declaration and a rendered
page, and it fails a build in both directions.

**4. It runs against `examples/minimal-edge`, not a stub.** The edge boots on the in-memory fakes
with no database, no hardware and no network, and serves the same `ui/dist` a real till gets
(ADR-0018). Stubbing the API instead would make the harness cheaper and would also make it prove
less: a stub is a second definition of the edge's behaviour, and the one that drifts is the one
nobody runs.

Two things `minimal-edge` does not do yet, found by running it rather than by reading it:

* **It hardcodes `127.0.0.1:8787`.** Two runs collide, and a developer cannot drive the harness while
  their own edge is up — the second process dies with `AddrInUse`. It gains a bind override.
* **Its pairing code is printed only to the log**, freshly generated per boot
  (`pairing_url=…/pair?code=175566`). The harness reads it from the process output, which keeps the
  pairing path exactly as a real device experiences it (ADR-0030, ADR-0084) rather than adding a
  back door that would make the gate stop proving the boot gate works.

**5. The till first, the console later.** `ui/` has a decided ceiling to defend — §6's two and three.
`dashboard/scripts/step-budget.mjs` deliberately carries `budget: null` on every task, because
applying a till's number to a back-office console would be a figure picked to look strict rather than
one anybody argued for. There is no ceiling there to breach, so there is nothing yet for a browser to
defend, and the console's harness waits for the ceiling rather than arriving before it.

## What this deliberately does not do

* **It does not do visual regression.** No screenshots, no pixel diffs, no golden images. Those fail
  on font rendering and runner versions, and the cost of maintaining them is paid every week by
  everyone. The WCAG contrast gate already covers the presentational rule this tree actually states.
* **It does not replace the static gate.** The analyser is faster, runs on every pull request without
  a browser, and catches renames the browser would only catch as a mysterious missing element. Both
  run; they answer different questions.
* **It does not search for a shorter path**, per the decision above — so it cannot tell you a flow
  *could* be one tap cheaper. That remains a design conversation, and the printed map is what makes
  it a five-second one.
* **It does not assert timing.** "Fast enough" on a shared CI runner is a number about the runner.
* **It does not cover the cloud console**, per §5.
* **It does not claim the hardware paths.** A receipt printer, a cash drawer and a card terminal are
  in `gate-register.md` for a reason and stay there; the harness drives the flows a fake can serve.

## Consequences

* The named blind spot closes: an undeclared required tap in a covered flow fails a pull request,
  with the declaration untouched. That is the one thing the current gate cannot do.
* `ui/` gains a dev dependency and CI gains a job that needs a browser binary. On a GitHub runner that
  is a download the pull-request gate does not pay today — this record accepts that cost knowingly,
  and it is the largest single thing being traded for the property above.
* Every declared tap's element gains a `data-step` attribute. Roughly a dozen elements across the
  selling screens, and it is load-bearing rather than decoration: the static gate requires it.
* `examples/minimal-edge` gains a bind override, which also makes it nicer to run twice by hand.
* A flow that is *deliberately* made longer — a confirmation somebody actually wants — now has to say
  so in one place, by adding the step to the declaration, where the budget line makes the cost
  visible. That is the point, and it is also the only real ongoing cost to a contributor.
* **A red harness is never fixed by deleting a step from the declaration.** That turns the gate green
  by making the map lie, and it is the one failure mode this record wants written down: if the flow
  genuinely got longer, the declaration grows and the budget line goes red until someone argues the
  number in `docs/ui-ux.md` §6.
* `gate-register.md` **A4** stops being an open gap for `ui/` and narrows to the console, which is
  waiting on a decided ceiling rather than on a harness.
