// The kitchen board's chime: two short notes when new food reaches the board.
//
// Made with the Web Audio API rather than played from a sound file, so the till ships no asset and a
// board that has never reached the internet still rings. Two rising notes rather than one beep,
// because a single tone in a kitchen full of fryer timers is one more beep nobody turns round for.
//
// # Why it needs a tap first
//
// A browser will not start sound on a page nobody has touched: an `AudioContext` created without a
// user gesture starts suspended and stays silent (every engine's autoplay policy). So the context is
// made, or resumed, inside a tap — the cook's tap on the sound toggle, or any tap on the board after
// a reload — and until then `chime` does nothing rather than queueing sound for later.

let context: AudioContext | null = null;

// Whether the chime can sound right now: a context exists and the browser has let it run.
export function chimeReady(): boolean {
  return context !== null && context.state === "running";
}

// Makes the context, or wakes it, from inside a user gesture. Answers whether sound is now allowed.
// Safe to call on every tap: once running, it does nothing.
export async function unlockChime(): Promise<boolean> {
  if (typeof AudioContext === "undefined") {
    return false;
  }
  try {
    context ??= new AudioContext();
    if (context.state !== "running") {
      await context.resume();
    }
  } catch {
    return false;
  }
  return chimeReady();
}

// The two notes, about a third of a second in all. Nothing happens until a tap has unlocked sound.
export function chime(): void {
  if (context === null || context.state !== "running") {
    return;
  }
  const start = context.currentTime;
  for (const [offset, frequency] of [
    [0, 880],
    [0.16, 1320],
  ] as const) {
    const tone = context.createOscillator();
    const volume = context.createGain();
    tone.type = "sine";
    tone.frequency.setValueAtTime(frequency, start + offset);
    // A fast rise and a short fall, so the note does not click on and off.
    volume.gain.setValueAtTime(0.0001, start + offset);
    volume.gain.exponentialRampToValueAtTime(0.3, start + offset + 0.02);
    volume.gain.exponentialRampToValueAtTime(0.0001, start + offset + 0.15);
    tone.connect(volume);
    volume.connect(context.destination);
    tone.start(start + offset);
    tone.stop(start + offset + 0.16);
  }
}
