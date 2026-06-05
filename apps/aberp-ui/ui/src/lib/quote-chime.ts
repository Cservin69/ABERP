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
