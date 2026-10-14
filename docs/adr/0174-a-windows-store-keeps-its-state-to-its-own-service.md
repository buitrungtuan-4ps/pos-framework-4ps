# ADR-0174 — A Windows store keeps its state to its own service

**Status** Accepted · **Owner** @maintainers-edge · **Date** 2026-10-14
· Supersedes the deferral in [ADR-0117](0117-a-headless-store-keeps-a-log.md) decision 9
· Relates to [ADR-0086](0086-edge-keyvault-and-activation.md),
[ADR-0151](0151-a-headless-linux-box-seals-its-secrets-with-systemd-creds.md),
[ADR-0055](0055-edge-ota-updater.md), [ADR-0111](0111-a-second-origin-may-address-the-edge.md)

owner delegated approval 2026-10-14

## Context

The Windows installer (`deploy/edge/install-pos-edge.ps1`, generated from
`dashboard/src/installers.mjs`) registers `pos-edge` to run as **LocalSystem**. It keeps
everything in one directory, `C:\ProgramData\pos-edge`, under the ACL that directory inherits:

| File | Holds |
|---|---|
| `store.sqlite` (and its `-wal` and `-shm` files) | device token digests, staff sessions, subject rows |
| `config.toml` | which store and cloud; no credential |
| `pos-edge.log`, `pos-edge.log.1` | the run's log, which carries no credential or personal data (ADR-0117) |
| `pairing-url.txt` | a five-minute pairing code |
| `bin\` (`slot-a`, `slot-b`, `current`), `pos-edge.exe` | the update slots (ADR-0055) and a rescue copy |

`ProgramData`'s default ACL lets the local **Users** group read and execute what is created
beneath it. So any account on a shop-floor PC can copy the database. The device credential
(ADR-0086) is in Windows Credential Manager, in LocalSystem's profile.

ADR-0117 decision 9 declined to fix this with an `icacls`, for two reasons:
- **A lockout.** `/inheritance:r` would leave SYSTEM and Administrators only, locking a standard-user
  till out of the log the installer says it can read.
- **A parameter.** `-Root` is a script parameter, so an elevated ACL reset on a mistyped value would
  rewrite some other directory's permissions. The installer also discards every external
  command's exit code.

[Gate-register](../gate-register.md) row P12 records the exposure. The owner decided on 2026-10-14
that a record settles the principals and the ACLs, and the installer applies them.

Linux is better placed but not fixed: the unit runs as user `pos`, but `StateDirectory=` creates
`/var/lib/pos-edge` with mode 0755 and the default umask leaves the database 0644.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Keep LocalSystem; restrict the existing directory | LocalSystem still has every privilege on the machine, and the log cannot be split from the database |
| B | A dedicated local user account | A password to create, store and rotate on every box |
| C | **A virtual service account, `NT SERVICE\pos-edge`, and a split layout** | Two privileges to manage, and a migration for existing boxes |

## Decision

Option **C**.

1. **The service runs as `NT SERVICE\pos-edge`.**
   - It is Windows' per-service virtual account: no password, and nothing but this service can be
     it.
   - Its privileges are restricted to `SeChangeNotifyPrivilege` and
     `SeCreateSymbolicLinkPrivilege` (`sc.exe privs`). The second is granted in local security
     policy, because the edge retargets `bin\current` with a symbolic link on an update
     (ADR-0055).
2. **One fixed path, split three ways.** The path is `%ProgramData%\pos-edge`, resolved from the
   system, never from a parameter.

   | Path | Holds | Access |
   |---|---|---|
   | `pos-edge\` | `config.toml`, the rescue `pos-edge.exe` | Inheritance off. SYSTEM and Administrators full; the service and Users read |
   | `bin\` | the update slots | As its parent, and the service may modify |
   | `data\` | the database and its WAL files, the sealed vault, the pairing file | **Inheritance off. SYSTEM and Administrators full; the service may modify. Nobody else** |
   | `logs\` | `pos-edge.log` and `pos-edge.log.1` | As its parent: the service may modify, and **Users read** |

   - **Where the edge looks.** `store_path`, `POS_EDGE_LOG_FILE` and `POS_EDGE_PAIRING_FILE` point
     into these paths.
   - **Who reads the pairing file.** It moves to `data\`, so only an administrator reads it; the
     installer prints the URL in its own elevated window, as it does today. The log stays readable
     by a standard-user account, as ADR-0117 promised.
3. **The device credential is sealed with DPAPI.**
   - **What is sealed:** each secret the `KeyVault` holds (the device credential, the sync key, the
     broker token) is sealed on disk in `data\vault\`, in the format ADR-0151 uses on Linux.
   - **The vault key:** 256 bits, protected with DPAPI (`CryptProtectData`) in machine scope.
     DPAPI binds the secrets to this machine, so a copied disk or backup cannot open them. The
     `data\` ACL keeps every other local account from reading the file.
   - **Why not user scope:** a user-scope key would bind the secrets to the service's profile, and
     add nothing against an administrator, who can run as the service anyway.
   - **Credential Manager is no longer used on Windows:** it is tied to a logon profile, and is
     unreliable under a service account.
4. **The installer sets ACLs only on the path it created, and checks them afterwards.**
   - **No other path.** A `-Root` other than the fixed path stops the installer before it changes
     anything.
   - **No redirected path.** A path that is a reparse point (a symbolic link or junction), or is not
     owned by SYSTEM or Administrators, is refused the same way.
   - **Exit codes are read.** Every `icacls` and `sc.exe` call checks `$LASTEXITCODE`.
   - **The result is checked.** The installer reads each ACL back with `Get-Acl` and compares it
     with the table above. A difference is a **FAIL** line in its summary, and the service is not
     moved off LocalSystem.
5. **The edge checks its own state directory, and the setup screen says when it is wrong.**
   - **When:** at start, and on each `GET /api/activation`.
   - **What it checks:** whether any principal outside SYSTEM, Administrators and its own account
     can read `data\`. On Linux the same check is that the state directory grants nothing to group
     or others.
   - **How it reports:** `GET /api/activation` gains an optional `state_access`: `STATE_ACCESS_OK`,
     `STATE_ACCESS_TOO_OPEN` or `STATE_ACCESS_UNCHECKED`. The field is additive (ADR-0111), and an
     older till ignores it.
   - **What the operator sees:** a translated warning on the setup screen, saying to run the
     installer again. A warning is also logged at start.
   - **What it reveals:** that local accounts can read the state, never which file or how. That is
     only useful to someone already sitting at the box.
   - **Where the check lives:** a Windows-only adapter crate that reads the DACL through
     `windows-sys`, under a crate-level `#![expect(unsafe_code, reason = …)]`, the exception the
     workspace lint table in `Cargo.toml` reserves for an FFI adapter. That crate also holds the
     DPAPI calls, and is the first to use the exception.
6. **Linux keeps the same rule.** The systemd unit gains `StateDirectoryMode=0700` and `UMask=0077`.
7. **An existing Windows box migrates when the installer is run again.**
   1. Stop the service.
   2. Create `data\` and `logs\`, move the database, its WAL files and the pairing file into
      `data\` and the logs into `logs\`, and rewrite `config.toml`.
   3. **Start once, still as LocalSystem.** On that start the edge finds its secrets in Credential
      Manager and no sealed vault. It seals them into `data\vault\`, reads them back, and only then
      deletes the Credential Manager entries.
   4. Stop again, apply and verify the ACLs, grant the symbolic-link privilege, and move the service
      to `NT SERVICE\pos-edge`.
   5. Start it, and run the installer's existing health check.

   If any step fails, the summary says which. The service is left on LocalSystem, which can still
   read everything, so the store keeps trading. A box that is not re-installed keeps working on its
   old layout, and its setup screen warns.

## Consequences accepted

- **A new direct dependency:** `windows-sys` in one Windows-only adapter crate. It is already in
  `Cargo.lock`, so no new crate reaches the lock. That crate is the one place `unsafe` FFI is
  allowed.
- **A broader privilege for the service:** `SeCreateSymbolicLinkPrivilege`, held by a far narrower
  account than LocalSystem.
- **The pairing file is for administrators only.** A technician reads it from an elevated window, or
  from the installer's summary.
- **Migration is not automatic.** OTA updates do not perform it; a box is migrated when the
  installer is run on it.
- **What only a real Windows machine can confirm** stays in gate-register row P12:
  - that a standard-user till account can still use the till and read `logs\`, and cannot read
    `data\`;
  - that the virtual account starts and can update itself;
  - that DPAPI and the migration survive a reboot.
- **Out of scope:** the print agent's installer (`install-pos-print-agent.ps1`) runs as its own
  service with no store database. It is unchanged here.
