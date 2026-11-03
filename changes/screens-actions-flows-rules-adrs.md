---
kind: added
---
- **ADR-0190, 0191, 0192, 0193 and 0196, accepted: how a package draws screens, runs acts, flows and
  rules, and paints first.** A screen is a tree of typed components from a catalogue each release
  declares; every act is a typed catalogue entry with its permission and its effect class; a flow is
  a graph of required, optional and conditional steps that the till runs and the edge re-checks;
  conditions and reactions are written in a restricted CEL with one evaluator on the edge and in the
  till; every act paints at once, while the drawer and the receipt still wait for the edge's commit.
  The code follows in its own changes.
