/**
 * Lip sync from audio the page plays or hears. Web Audio taps the source, and
 * every animation frame hands the session the samples rendered since the
 * last, from which its replica reads the mouth.
 */

import type { Session, Voice } from "./session.js";

/** A microphone or call stream, or an `<audio>` or `<video>` element. */
export type VoiceSource = MediaStream | HTMLMediaElement;

export interface LipSync {
  /** What the voice gives the mouth now; `undefined` once stopped. */
  readonly voice: Voice | undefined;
  /** Stops listening. A source passed in stays the caller's and plays on. */
  stop(): void;
}

/**
 * How far back the tap can reach, in samples: a frame later than this drops
 * the oldest of them. 8192 is 170 ms at 48 kHz.
 */
const REACH = 8192;

/**
 * A media element can be given to Web Audio only once, and from then on it
 * sounds only through that graph. Each keeps its node, wired to its own
 * context's speakers, for good.
 */
const elements = new WeakMap<HTMLMediaElement, { context: AudioContext; node: AudioNode }>();

function tapElement(element: HTMLMediaElement): { context: AudioContext; node: AudioNode } {
  let tap = elements.get(element);
  if (!tap) {
    const context = new AudioContext();
    const node = context.createMediaElementSource(element);
    node.connect(context.destination);
    tap = { context, node };
    elements.set(element, tap);
  }
  return tap;
}

function isElement(source: VoiceSource): source is HTMLMediaElement {
  return typeof HTMLMediaElement !== "undefined" && source instanceof HTMLMediaElement;
}

/**
 * Starts the mouth following `source`, or the microphone when there is none,
 * which asks for permission. An element keeps playing through Web Audio after
 * `stop`.
 */
export async function startLipSync(session: Session, source?: VoiceSource): Promise<LipSync> {
  let context: AudioContext;
  let tap: AudioNode;
  let opened: MediaStream | undefined;
  // An element's context outlives this lip sync; a stream's is its own.
  let ownsContext = false;
  if (source && isElement(source)) {
    ({ context, node: tap } = tapElement(source));
  } else {
    const stream =
      source ??
      (opened = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      }));
    context = new AudioContext();
    tap = context.createMediaStreamSource(stream);
    ownsContext = true;
  }
  if (context.state === "suspended") {
    // Browsers hold audio until the page has had a gesture.
    const resume = () => void context.resume().catch(() => undefined);
    resume();
    addEventListener("pointerdown", resume, { once: true });
    addEventListener("keydown", resume, { once: true });
  }

  const analyser = context.createAnalyser();
  analyser.fftSize = REACH;
  tap.connect(analyser);
  const recent = new Float32Array(REACH);
  const rendered = () => Math.round(context.currentTime * context.sampleRate);
  let heard = rendered();

  let frame = 0;
  let stopped = false;
  const stop = () => {
    if (stopped) return;
    stopped = true;
    cancelAnimationFrame(frame);
    tap.disconnect(analyser);
    for (const track of opened?.getTracks() ?? []) track.stop();
    if (ownsContext) void context.close().catch(() => undefined);
    session.clearVoice();
  };
  const listen = () => {
    if (session.closed) {
      stop();
      return;
    }
    const now = rendered();
    const fresh = Math.min(now - heard, REACH);
    heard = now;
    if (fresh > 0) {
      analyser.getFloatTimeDomainData(recent);
      session.pushVoice(recent.subarray(REACH - fresh), context.sampleRate);
    }
    frame = requestAnimationFrame(listen);
  };
  frame = requestAnimationFrame(listen);

  return {
    get voice() {
      return stopped ? undefined : session.voice();
    },
    stop,
  };
}
