# UI and UX guideline

**Status** Accepted · **Owner** @maintainers-domain · **Last reviewed** 2026-08-18

The real users are staff standing for eight-hour shifts, with wet or greasy hands, during a rush, on devices ranging from a phone to a kitchen display read from two metres away. Every rule below serves that person first and the office user second.

**Philosophy: minimal frame, dense content, no surprises.**

| Direction | How it applies here |
|---|---|
| Modern minimal | Minimal in *decoration* — no gradients, no showcase animations. **Not** minimal in *information density* on cashier and kitchen screens: operators need to see many things at once. |
| Light and responsive | The server already answers in under 5 ms; the UI adds optimistic updates and a sub-100 ms perceived response. |
| Device adaptive | One application, **a distinct layout per device class** — not one layout that stretches. |
| Context aware | Adapts by role, system state, and shift phase — with one hard constraint: primary controls never move. |

---

## 1. Ten principles

1. **Perceived speed is feature number one.** Optimistic UI: a tap updates the screen immediately and synchronisation follows (the outbox guarantees correctness). Skeletons instead of spinners; never a blank waiting screen. Animations last at most 150 ms and never appear in a money path.
2. **Touch targets for real fingers.** Minimum 48 px; 56–64 px for cash keypads and primary actions; at least 8 px between targets. No hover-only affordances, no hidden gestures on primary paths.
3. **Stable positions — muscle memory is an asset.** Pay, fire, and open-table buttons never move. Adaptive behaviour may change *what is shown and in what priority*, never *where the primary controls live*.
4. **Disciplined context awareness.** By role: a server starts on the floor plan, a cashier on the payment queue, a cook on their station. By state: offline and printer errors surface where they matter. By phase: end-of-shift raises the close action. Never hide a function that is currently needed to "keep it clean".
5. **System state is always visible.** A thin persistent bar shows online/offline, print queue, and sync lag. No modals, no covering content. Offline is a normal working state, not an error.
6. **One screen answers one question.** Common actions take at most two taps from the role's home screen; rare actions at most three.
7. **Errors always have an exit.** Every error state offers the next action — retry, switch to the backup printer, call a manager by PIN. No dead ends, and never a raw error code in front of staff.
8. **Money is king.** The largest type on the screen, tabular figures, locale-aware thousands separators, and never a displayed value that differs from the real one for cosmetic rounding.
9. **Four device classes, four layouts.**
   - *POS terminal* (13"+): two columns — categories and items on the left, the open bill on the right.
   - *Tablet*: large item grid, bill slides up; usable one-handed.
   - *Phone*: single column, primary action anchored at the bottom within thumb reach.
   - *Kitchen display*: readable from two metres — 24–28 px type, high contrast, dark background, whole-card tap targets or a bump bar.
10. **Internationalisation from the first commit.** No hardcoded strings; dates, numbers, and money render per the store's locale; layouts survive text 30% longer than English; language is configurable per store and per device.

## 2. Design tokens

| Group | Values |
|---|---|
| Spacing | 4 px scale: 4 · 8 · 12 · 16 · 24 · 32 |
| Touch targets | 48 px standard; 56–64 px for money and primary actions |
| Type | 12 (meta) · 14 (label) · 16 (body) · 20 (heading) · 28 (KDS) · 40+ tabular (totals) |
| Semantic colour | success / warning / error / info at WCAG AA contrast. **Never carry meaning by colour alone** — always pair with an icon or text (8% of men have red-green colour deficiency). |
| Shape | One radius (8 px), one border width (1 px). That is enough. |
| Motion | 100–150 ms ease-out, only for entrance and orientation |
| Theme | Light and dark across the product, configured per device (kitchen displays default to dark). Colour tokens are separate from structural tokens. |
| On-screen keyboard | Shared component for numeric and text entry on touch devices without a physical keyboard |

Tokens live in one theme file (SolidJS + Tailwind config), so per-tenant branding later means changing tokens, not components.

## 3. Core screens

**Order (server, tablet or phone).** The floor plan is home: colour-coded table states, tap a table to open its order. Item grid by category; sold-out items grey out and strike through on every device instantly. Adding an item is one tap; required modifier groups open immediately. Half-and-half items are chosen in a single flow and appear as one line showing both halves. When seats are enabled, the seat is chosen before items and shown on each line. Each item displays **available-to-make** ("~6 left"), refreshed within 50 ms of any fire. The fire button is fixed and shows the count of unfired lines. When two staff edit one table, the other person's lines appear live with their name.

**Cashier (POS terminal).** The right column is the bill: lines, discounts, service charge, tax, and the **total as the largest element on screen**. A large numeric keypad for cash with change displayed immediately, plus **quick-cash** denomination buttons. Payment methods are a fixed row of large buttons. Tips (when enabled) appear after a card method is chosen, as suggested percentages plus manual entry. Splitting drags lines into a new panel. Permission-gated actions open a PIN field in place — never navigate away. Opening the payment screen takes a **soft lock** and other devices see "X is taking payment"; the screen auto-locks after N idle minutes and reopens with a PIN.

**Kitchen display.** Cards by order and course, an age timer per card, colour changes at configured thresholds. Bump is a tap anywhere on the card. Recall keeps the last 60 seconds at the edge of the screen. Void tickets appear in red for 10 seconds with an audible alert. Nothing decorative — this screen is a production tool.

**Admin dashboard (web).** Today's figures read from rollup tables (<10 ms): revenue, bill count, hourly curve, payment mix. A green/red fleet heatmap. Every configuration table can show which store is running which version. High information density is correct here — the user is an administrator, not a cashier.

**The store profile decides the starting screen and flow** ([pos-spec.md](pos-spec.md) §10): full-service starts on the floor plan and pays afterwards; a counter cafe starts on the order screen, pays first, and issues a queue number; retail starts on the barcode field. Same components, different assembly — not three applications.

## 4. Degraded states

| Situation | What the UI does |
|---|---|
| No internet | Status bar reads "Offline — selling normally", counts pending events, and blocks nothing |
| Station printer failure | Red badge on that station's kitchen card plus exits: reprint, switch to backup printer; the print queue shows pending tickets |
| Offline beyond the configured threshold | Warning banner, escalating to a per-shift manager acknowledgement — never an automatic block on selling |
| Card result unknown | Bill parked in an amber state with two clear options (confirm manually against the terminal, or cancel), and it appears in the reconciliation list |
| Store server restarting | Clients reconnect via `pos.local` showing "Reconnecting…" for a few seconds; tables and shifts are preserved |
| QR ordering unavailable (store or cloud offline) | The guest page reads "Please ask a staff member" — staff are always the fallback |

## 5. Dashboard screen inventory

**Specified:** setup wizard · configuration tree · fleet heatmap · Today · system status · audit log · device management and revocation · printer approval · invitation links · store export · Developers (API keys, webhook endpoints, delivery log with redeliver, test event) · Localization (enable languages, translation grid, completion, CSV).

**Backlog, mechanisms already exist:** OTA release management with rollout progress and kill switch · reconciliation viewer for nightly checks and unknown-result payments · store detail page (health, version, last backup, invoice queue depth, pull logs) · chain reports over date ranges · tenant-level staff management across stores · recovery actions (reissue activation code, reset lease).

## 6. Deliberately not done

Trend decoration (glassmorphism, parallax) · long onboarding tours — a sample menu plus a readiness checklist teaches by doing · dense tooltips · hidden gestures on primary paths · dark patterns · confirmation dialogs for reversible actions (offer undo instead, before firing or settling) · sound effects beyond the functional ones (bump, print failure, void ticket).

---

**Definition of success**, verified by observation during the pilot, not by survey: a new employee completes a full sale within five minutes without training, and an experienced cashier works an entire shift without ever hunting for a button.
