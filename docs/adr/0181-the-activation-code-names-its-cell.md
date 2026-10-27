# ADR-0181 — The activation code names its cell

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-10-27 · Amends
[ADR-0050](0050-activation-code-exchange.md) · Implements item C4 of
[ADR-0179](0179-cells-the-directory-and-the-front-door.md)

Waits on the owner's approval. It changes `pos-core`'s `ActivationCode`, which needs an owner
review, and no code changes until it is Accepted.

## Context

[ADR-0179](0179-cells-the-directory-and-the-front-door.md) says a device resolves its cell once,
"from the activation-code prefix (`VN1-…`), or from `/.well-known/pos-cell`", and then stores its
`cloud_url`. Today a box learns its cloud only from its own configuration file, so a technician
replacing a till in a two-cell fleet has to know which cell's address to type, and a wrong one fails
only after the code is presented to a cloud that never issued it.

[ADR-0050](0050-activation-code-exchange.md) fixes the code's format: eleven Crockford base-32
payload symbols and a checksum symbol, shown as `XXXX-XXXX-XXXX`. `ActivationCode::parse` discards
every hyphen and space, folds `I`/`L` to `1` and `O` to `0`, and requires exactly twelve symbols. Two
facts follow, and they constrain any prefix:

- `V`, `N` and `1` are all in the alphabet. Typed straight in front of today's code, `VN1-…` is just
  three more symbols and fails on length, which is safe but unhelpful.
- Folding would rewrite a prefix. A cell id such as `jp-io1` would be read as `JP10` symbols, so a
  prefix cannot pass through the payload's normalisation.

## Options considered

**A. Keep the code as it is and resolve the cell from `/.well-known/pos-cell` alone.** No format
change, but the box must already know some cell's address, which is the thing a technician lacks.

**B. A prefix in front of the code, split off before the code's normalisation.**
`VN1-ABCD-EFGH-JKMN`. The prefix is a cell id, compared case-insensitively against the directory, and
is never folded. A code of twelve symbols with no prefix still parses as today.

**C. Encode the cell in the payload.** Costs entropy that ADR-0050 counted on, and needs a cell
number register that ADR-0179 deliberately does not have.

## Decision

**Option B.**

- **Format.** An activation code is `[<cell_id>-]XXXX-XXXX-XXXX`. The prefix is optional and is a
  cell id as ADR-0179 C1 defines it: 3 to 32 of `a-z`, `0-9` and `-`, starting with a letter. It is
  printed upper-case on a setup sheet and read case-insensitively.
- **Parsing.** If the whole input normalises, as today, to exactly twelve valid symbols, it is a code
  with no prefix. Otherwise the code is read from the **end**: `ActivationCode::parse` walks back
  over the input, skipping hyphens and spaces, until it has twelve symbols, and parses those as
  today. Whatever comes before, less one separating hyphen, is the prefix, checked as a cell id
  without folding. Reading from the end matters because a cell id may itself contain hyphens
  (`jp-east-2`) and letters outside the code's alphabet (`u`). A prefix that is not a cell id, a
  prefix with no hyphen before the code, or a remainder that is not a valid code is refused with its
  own `CodeError` variant, so the keyboard check of ADR-0050 still names the mistake.
- **What the prefix does.** It is a routing hint and never a credential. The box looks the cell up in
  the signed directory it already carries (ADR-0179 C2), and, failing that, asks
  `/.well-known/pos-cell` of the cloud it was configured with. It then presents the code to that cell's
  `/activate` and stores the `cloud_url`. The prefix is not hashed into the stored code, so a code is
  redeemed by its twelve symbols and a prefix cannot make a spent code live again.
- **Issuing.** `POST /admin/activation-codes` returns the code with this cell's id as its prefix once
  a `[cell]` section is configured. A single default cell issues codes without a prefix, as today.
- **A wrong prefix** is refused by the cell it reaches. That cell holds no record of the code, so the
  answer is the same `403` an unknown code gets today, plus the directory's answer for where the
  prefix points.

## Consequences

- `pos-core`'s `ActivationCode` gains an optional cell prefix and one or two `CodeError` variants.
  Both are additive. Every existing twelve-symbol code parses unchanged.
- No `pos-proto` change: the exchange request carries the twelve-symbol code as it does now.
- Setup sheets printed before this change stay valid.
- The edge needs the directory (C2) and the `/.well-known/pos-cell` answer (C3) to act on a prefix.
  Until it has them, it ignores the prefix and uses its configured cloud.
- `docs/architecture.md` §7 and ADR-0050's format paragraph are read together with this record.
