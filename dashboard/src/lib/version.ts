// The build stamp the console footer shows (ADR-0060, Track F1). The deploy/build injects
// `VITE_APP_VERSION` — a git short SHA or a release tag — so an operator, and any bug report they
// file, can name exactly which dashboard build is live. A local dev build with none set reads "dev".

const env = import.meta.env as Record<string, string | undefined>;

export const APP_VERSION: string = env.VITE_APP_VERSION ?? "dev";
