// S256 / PR-245 — a single subtle arrival chime (brief §B.11). WebAudio
// so there's no asset to bundle and no new dependency. ONE short tone,
// never continuous. The CALLER is responsible for the demo-mode
// suppression ([[aberp-workshop-demo-mode]]) and the soundEnabled
// preference gate — this function just plays once when asked.

type AudioCtor = typeof AudioContext;

function audioCtor(): AudioCtor | null {
  if (typeof window === "undefined") return null;
  const w = window as unknown as {
    AudioContext?: AudioCtor;
    webkitAudioContext?: AudioCtor;
  };
  return w.AudioContext ?? w.webkitAudioContext ?? null;
}

/** Play a single soft two-note chime. Best-effort: silently no-ops when
 * WebAudio is unavailable (e.g. vitest jsdom) or construction throws. */
export function playArrivalChime(): void {
  const Ctor = audioCtor();
  if (Ctor === null) return;
  try {
    const ctx = new Ctor();
    const now = ctx.currentTime;
    const gain = ctx.createGain();
    gain.connect(ctx.destination);
    // Quick attack, gentle decay; peak well below 1.0 so it's subtle.
    gain.gain.setValueAtTime(0.0001, now);
    gain.gain.exponentialRampToValueAtTime(0.08, now + 0.02);
    gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.45);

    const osc = ctx.createOscillator();
    osc.type = "sine";
    osc.frequency.setValueAtTime(880, now); // A5
    osc.frequency.setValueAtTime(1175, now + 0.12); // D6 — a small lift
    osc.connect(gain);
    osc.start(now);
    osc.stop(now + 0.46);
    osc.onended = () => {
      try {
        void ctx.close();
      } catch {
        // ignore
      }
    };
  } catch {
    // No audio device / autoplay-blocked / jsdom — chime is optional.
  }
}

/** S258 / PR-247 — single alert tone for a Workshop adapter going
 * degraded/unhealthy. Deliberately distinct from `playArrivalChime`: two
 * DESCENDING notes (a "something dropped" cue) rather than the arrival's
 * upward lift, and a touch louder. Still ONE short burst, never
 * continuous — the CALLER owns the demo-mode + boot-grace + debounce
 * gating; this just plays once when asked. Best-effort: silently no-ops
 * when WebAudio is unavailable (vitest jsdom) or construction throws. */
export function playAdapterAlert(): void {
  const Ctor = audioCtor();
  if (Ctor === null) return;
  try {
    const ctx = new Ctor();
    const now = ctx.currentTime;
    const gain = ctx.createGain();
    gain.connect(ctx.destination);
    gain.gain.setValueAtTime(0.0001, now);
    gain.gain.exponentialRampToValueAtTime(0.12, now + 0.02);
    gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.5);

    const osc = ctx.createOscillator();
    osc.type = "triangle";
    osc.frequency.setValueAtTime(660, now); // E5
    osc.frequency.setValueAtTime(440, now + 0.14); // A4 — a downward drop
    osc.connect(gain);
    osc.start(now);
    osc.stop(now + 0.51);
    osc.onended = () => {
      try {
        void ctx.close();
      } catch {
        // ignore
      }
    };
  } catch {
    // No audio device / autoplay-blocked / jsdom — alert is optional.
  }
}
