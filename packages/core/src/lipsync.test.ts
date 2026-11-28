import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { FakeReplica, ScriptedBackend } from "./fakes.js";
import { startLipSync } from "./lipsync.js";
import { Session } from "./session.js";

class FakeNode {
  readonly to = new Set<FakeNode>();
  constructor(readonly context: FakeContext) {}
  connect(node: FakeNode): FakeNode {
    this.to.add(node);
    return node;
  }
  disconnect(node: FakeNode): void {
    this.to.delete(node);
  }
}

/** Its time-domain buffer holds sample index `i` as the value `i`. */
class FakeAnalyser extends FakeNode {
  fftSize = 0;
  getFloatTimeDomainData(into: Float32Array): void {
    const end = Math.round(this.context.currentTime * this.context.sampleRate);
    for (let i = 0; i < into.length; i++) into[i] = end - into.length + i;
  }
}

class FakeContext {
  static made: FakeContext[] = [];
  /** Whether a gesture has let audio start. */
  static gesture = true;
  readonly sampleRate = 48000;
  readonly destination: FakeNode = new FakeNode(this);
  readonly analysers: FakeAnalyser[] = [];
  readonly sources: FakeNode[] = [];
  currentTime = 1;
  state = FakeContext.gesture ? "running" : "suspended";
  closed = false;
  constructor() {
    FakeContext.made.push(this);
  }
  createAnalyser(): FakeAnalyser {
    const analyser = new FakeAnalyser(this);
    this.analysers.push(analyser);
    return analyser;
  }
  createMediaStreamSource(_stream: unknown): FakeNode {
    return this.#source();
  }
  createMediaElementSource(_element: unknown): FakeNode {
    return this.#source();
  }
  resume(): Promise<void> {
    if (!FakeContext.gesture) return Promise.reject(new Error("no gesture yet"));
    this.state = "running";
    return Promise.resolve();
  }
  close(): Promise<void> {
    this.closed = true;
    return Promise.resolve();
  }
  #source(): FakeNode {
    const source = new FakeNode(this);
    this.sources.push(source);
    return source;
  }
}

class FakeElement {}

class FakeStream {
  readonly stopped = [false, false];
  getTracks() {
    return this.stopped.map((_, i) => ({
      stop: () => {
        this.stopped[i] = true;
      },
    }));
  }
}

/** Animation frames run only when a test says so. */
let frames = new Map<number, () => void>();
let nextFrame = 1;
function runFrame(): void {
  const due = frames;
  frames = new Map();
  for (const callback of due.values()) callback();
}

let asked: unknown[] = [];
let microphone: FakeStream | undefined;
const saved = new Map<string, PropertyDescriptor | undefined>();
const globals: Record<string, unknown> = {
  AudioContext: FakeContext,
  HTMLMediaElement: FakeElement,
  requestAnimationFrame: (callback: () => void) => {
    frames.set(nextFrame, callback);
    return nextFrame++;
  },
  cancelAnimationFrame: (handle: number) => frames.delete(handle),
};

beforeEach(() => {
  FakeContext.made = [];
  FakeContext.gesture = true;
  frames = new Map();
  asked = [];
  microphone = undefined;
  for (const [name, value] of Object.entries(globals)) {
    saved.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
  }
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: {
      getUserMedia: (constraints: unknown) => {
        asked.push(constraints);
        microphone = new FakeStream();
        return Promise.resolve(microphone);
      },
    },
  });
});

afterEach(() => {
  for (const [name, descriptor] of saved) {
    if (descriptor) Object.defineProperty(globalThis, name, descriptor);
    else delete (globalThis as Record<string, unknown>)[name];
  }
  saved.clear();
  delete (navigator as unknown as Record<string, unknown>).mediaDevices;
});

/** A session whose replica records each pushed sample's index. */
function open(): { replica: FakeReplica; session: Session; pushed: number[] } {
  const replica = new FakeReplica();
  const pushed: number[] = [];
  const push = replica.pushVoice.bind(replica);
  replica.pushVoice = (samples, sampleRate) => {
    pushed.push(...samples);
    push(samples, sampleRate);
  };
  return { replica, session: new Session(new ScriptedBackend(), 1, replica), pushed };
}

function only<T>(items: T[]): T {
  expect(items).toHaveLength(1);
  return items[0] as T;
}

const stream = () => new FakeStream() as unknown as MediaStream;

describe("lip sync", () => {
  test("each frame pushes exactly the samples rendered since the last", async () => {
    const { replica, session, pushed } = open();
    await startLipSync(session, stream());
    const context = only(FakeContext.made);
    expect(only(context.sources).to.has(only(context.analysers))).toBe(true);

    runFrame();
    expect(pushed).toEqual([]);
    context.currentTime += 0.01;
    runFrame();
    context.currentTime += 0.005;
    runFrame();
    expect(pushed).toEqual(Array.from({ length: 720 }, (_, i) => 48000 + i));
    expect(replica.heard).toEqual({ sampleRate: 48000, samples: 720 });
  });

  test("a frame late past the tap's reach keeps only what the tap holds", async () => {
    const { session, pushed } = open();
    await startLipSync(session, stream());
    const context = only(FakeContext.made);
    context.currentTime += 1;
    runFrame();
    expect(pushed).toHaveLength(8192);
    expect(pushed.at(-1)).toBe(2 * 48000 - 1);
  });

  test("stopping clears the voice and closes the context, leaving the stream alone", async () => {
    const { replica, session } = open();
    const source = new FakeStream();
    const sync = await startLipSync(session, source as unknown as MediaStream);
    replica.speaking = { level: 0.5, visemes: { uw: 0.25 } };
    only(FakeContext.made).currentTime += 0.01;
    runFrame();
    expect(sync.voice).toEqual({ level: 0.5, visemes: { uw: 0.25 } });

    sync.stop();
    expect(sync.voice).toBeUndefined();
    expect(replica.heard).toBeUndefined();
    expect(frames.size).toBe(0);
    expect(only(FakeContext.made).closed).toBe(true);
    expect(source.stopped).toEqual([false, false]);
  });

  test("with no source it opens the microphone and closes it on stop", async () => {
    const { session } = open();
    const sync = await startLipSync(session);
    expect(asked).toEqual([
      { audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true } },
    ]);
    sync.stop();
    expect(microphone?.stopped).toEqual([true, true]);
  });

  test("an element is tapped once and keeps sounding after a stop", async () => {
    const { session } = open();
    const element = new FakeElement() as unknown as HTMLMediaElement;
    (await startLipSync(session, element)).stop();
    const sync = await startLipSync(session, element);
    const context = only(FakeContext.made);
    const tap = only(context.sources);
    expect(tap.to.has(context.destination)).toBe(true);
    expect(tap.to.has(context.analysers[1] as FakeAnalyser)).toBe(true);

    sync.stop();
    expect(tap.to).toEqual(new Set([context.destination]));
    expect(context.closed).toBe(false);
  });

  test("audio held for a gesture starts on the next one", async () => {
    const { session } = open();
    FakeContext.gesture = false;
    await startLipSync(session, stream());
    const context = only(FakeContext.made);
    expect(context.state).toBe("suspended");
    FakeContext.gesture = true;
    dispatchEvent(new Event("pointerdown"));
    expect(context.state).toBe("running");
  });

  test("closing the session stops it on the next frame", async () => {
    const { session } = open();
    await startLipSync(session, stream());
    session.close();
    runFrame();
    expect(frames.size).toBe(0);
    expect(only(FakeContext.made).closed).toBe(true);
  });
});
