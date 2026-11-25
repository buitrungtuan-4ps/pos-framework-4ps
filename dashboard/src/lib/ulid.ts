// A new ULID, minted in the browser for an id the console gives something it creates in a draft,
// such as a package's custom field, list, list entry or object type
// ([ADR-0194](../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)
// decision 1: each has a stable id, and `pos-proto` types every one of them as a ULID).
//
// The layout is the ULID specification's: 48 bits of milliseconds, then 80 random bits, written as
// 26 characters of Crockford base 32. The random bits come from `crypto.getRandomValues`.

const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/** What a ULID looks like: 26 characters of Crockford base 32, the first at most `7`. */
export const ULID_PATTERN = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/u;

/** Fills `bytes` with random values; the browser's own unless a test gives another. */
export type RandomFill = (bytes: Uint8Array) => Uint8Array;

/** A new ULID for `now` (milliseconds since the epoch), its random part from `random`. */
export function newUlid(now: number = Date.now(), random: RandomFill = (bytes) => crypto.getRandomValues(bytes)): string {
  let time = "";
  let rest = Math.floor(now);
  for (let index = 0; index < 10; index += 1) {
    time = CROCKFORD[rest % 32] + time;
    rest = Math.floor(rest / 32);
  }
  // 80 random bits are 16 characters of 5 bits each: read them from 10 bytes, 5 bits at a time.
  const bytes = random(new Uint8Array(10));
  let bits = 0;
  let held = 0;
  let tail = "";
  for (const byte of bytes) {
    held = (held << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      tail += CROCKFORD[(held >> bits) & 31];
    }
    held &= (1 << bits) - 1;
  }
  return time + tail;
}
