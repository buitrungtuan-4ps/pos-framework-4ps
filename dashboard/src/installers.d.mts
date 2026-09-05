// Types for `installers.mjs`. The module itself is plain ESM so that bare `node` can run it on a CI
// runner with no toolchain set up; this file is what lets the TypeScript side call it with the same
// strictness as any other module.

/** Everything the four generated artifacts need, and nothing that only a browser could supply. */
export interface InstallerValues {
  /** The store's display name, as a person typed it into the wizard. */
  readonly storeName: string;
  /** The store's ULID. */
  readonly storeId: string;
  /** The tenant's display name, falling back to its ULID when the name is not loaded. */
  readonly tenantLabel: string;
  /** The tenant's ULID. */
  readonly tenantId: string;
  /** The cloud's origin, e.g. `https://cloud.example.com` — what the box dials. */
  readonly cloudUrl: string;
  /** The cloud's hostname alone, for the event-bus URL the operator completes by hand. */
  readonly cloudHost: string;
  /** The listen port from the form. Empty means "the edge's default". */
  readonly bindPort: string;
  /** The scoped store key, shown once at issuance, or `null` when none was issued. */
  readonly key: string | null;
  /**
   * An absolute path for the SQLite store, written into `config.toml` when present. The Windows
   * installer sets it; on Linux the systemd unit's `WorkingDirectory=` makes the relative default
   * correct, so it is left commented.
   */
  readonly storePath?: string | undefined;
  /** Overrides the Windows state root. Defaults to `WINDOWS_ROOT`. */
  readonly windowsRoot?: string | undefined;
}

export declare const DEFAULT_BIND_PORT: string;
export declare const FLEET_STREAM: string;
export declare const FLEET_SUBJECT: string;
export declare const NATS_CLIENT_PORT: string;
export declare const WINDOWS_ROOT: string;

export declare function configToml(v: InstallerValues): string;
export declare function envFile(v: InstallerValues): string;
export declare function linuxInstaller(v: InstallerValues): string;
export declare function windowsInstaller(v: InstallerValues): string;
