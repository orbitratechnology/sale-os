/**
 * A short, low error tone (E07-T04): with a "not found" or refusal message, never instead of
 * it. Web Audio, so no sound file ships; silently does nothing where audio is unavailable.
 */
let ctx: AudioContext | null = null;

export function errorBeep(): void {
  try {
    ctx ??= new AudioContext();
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.type = "square";
    osc.frequency.value = 220;
    gain.gain.value = 0.08;
    osc.connect(gain).connect(ctx.destination);
    const now = ctx.currentTime;
    osc.start(now);
    osc.stop(now + 0.18);
  } catch {
    // No audio device or no Web Audio: the message on screen is the cue.
  }
}
