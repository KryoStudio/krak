// Plays a clip's game and microphone tracks at the editor's volumes, in step with its video, so
// what you hear is what an export writes. The video's own sound is its "Mix" track, the
// microphone in it whatever the volumes: once the tracks are decoded (`get_audio_stems`), that
// sound is routed into Web Audio and left out (still pulled, so the video's clock runs), and
// the two tracks play instead, restarted on play, seek and rate changes, or when they drift.
// Clips over five minutes, or with other tracks, keep the video's sound (`state` 'unavailable').
//
// Stems layout (little-endian): sample rate u32, frames u32, game channels u16, microphone
// channels u16 (0: none), then the game's interleaved i16 samples and the microphone's.
import { api } from './api';

/** Seconds the tracks may drift from the video before they are restarted. */
const DRIFT = 0.08;

let context: AudioContext | null = null;
/** createMediaElementSource works once per element: kept for a clip renamed while open. */
const routed = new WeakMap<HTMLVideoElement, MediaElementAudioSourceNode>();

function audio(): AudioContext {
  context ??= new AudioContext({ sampleRate: 48_000 });
  return context;
}

function decode(ctx: AudioContext, data: ArrayBuffer): AudioBuffer[] {
  const view = new DataView(data);
  const rate = view.getUint32(0, true);
  const frames = view.getUint32(4, true);
  let offset = 12;
  const buffers: AudioBuffer[] = [];
  for (const channels of [view.getUint16(8, true), view.getUint16(10, true)]) {
    if (!channels || !frames) continue;
    const samples = new Int16Array(data, offset, frames * channels);
    offset += samples.byteLength;
    const buffer = ctx.createBuffer(channels, frames, rate);
    for (let c = 0; c < channels; c++) {
      const out = buffer.getChannelData(c);
      for (let i = 0; i < frames; i++) out[i] = samples[i * channels + c] / 32768;
    }
    buffers.push(buffer);
  }
  return buffers;
}

export class LiveMix {
  /** 'active': the tracks play, the video's own sound is left out. */
  state = $state<'idle' | 'loading' | 'active' | 'unavailable'>('idle');

  #video: HTMLVideoElement;
  #id: string;
  #gains = { game: 1, mic: 1 };
  #buffers: AudioBuffer[] = [];
  #nodes: { game: GainNode; mic: GainNode; master: GainNode; silent: GainNode } | null = null;
  #sources: AudioBufferSourceNode[] = [];
  /** Video time and context time when the tracks were started. */
  #origin = { video: 0, context: 0, rate: 1 };
  #off: (() => void)[] = [];
  #closed = false;

  constructor(video: HTMLVideoElement, id: string) {
    this.#video = video;
    this.#id = id;
  }

  /** Decodes the tracks (once) and switches to them. */
  async load() {
    if (this.state !== 'idle') return;
    this.state = 'loading';
    let data: ArrayBuffer;
    try {
      data = await api.getAudioStems(this.#id);
    } catch {
      if (!this.#closed) this.state = 'unavailable';
      return;
    }
    if (this.#closed) return;
    const ctx = audio();
    this.#buffers = decode(ctx, data);
    if (!this.#buffers.length) {
      this.state = 'unavailable';
      return;
    }
    this.#attach(ctx);
    this.state = 'active';
    if (!this.#video.paused) {
      ctx.resume().catch(() => {});
      this.#start();
    }
  }

  /** The editor's volumes (0..2), applied at once with a short ramp. */
  setGains(gains: { game: number; mic: number }) {
    this.#gains = { ...gains };
    if (!this.#nodes || !context) return;
    const now = context.currentTime;
    this.#nodes.game.gain.setTargetAtTime(gains.game, now, 0.015);
    this.#nodes.mic.gain.setTargetAtTime(gains.mic, now, 0.015);
  }

  destroy() {
    this.#closed = true;
    this.#stop();
    for (const off of this.#off) off();
    const nodes = this.#nodes;
    if (nodes) for (const node of Object.values(nodes)) node.disconnect();
    routed.get(this.#video)?.disconnect();
    this.#nodes = null;
    this.#buffers = [];
    context?.suspend().catch(() => {});
  }

  #attach(ctx: AudioContext) {
    const gain = (value: number) => {
      const node = ctx.createGain();
      node.gain.value = value;
      return node;
    };
    const nodes = { game: gain(this.#gains.game), mic: gain(this.#gains.mic), master: gain(1), silent: gain(0) };
    nodes.game.connect(nodes.master);
    nodes.mic.connect(nodes.master);
    nodes.master.connect(ctx.destination);
    nodes.silent.connect(ctx.destination);
    this.#nodes = nodes;
    // The video's own sound (the Mix) goes into the graph, at zero.
    let source = routed.get(this.#video);
    if (!source) {
      source = ctx.createMediaElementSource(this.#video);
      routed.set(this.#video, source);
    }
    source.connect(nodes.silent);
    this.#volume();

    const video = this.#video;
    const listen = (event: string, handler: () => void) => {
      video.addEventListener(event, handler);
      this.#off.push(() => video.removeEventListener(event, handler));
    };
    const restart = () => !video.paused && !video.seeking && this.#start();
    // The graph pulls the video's sound, so it runs while the video plays, and rests otherwise
    // (nothing left running in the tray).
    listen('play', () => ctx.resume().catch(() => {}));
    listen('pause', () => ctx.suspend().catch(() => {}));
    listen('playing', restart);
    listen('seeked', restart);
    listen('ratechange', restart);
    for (const event of ['pause', 'waiting', 'seeking', 'emptied', 'ended']) listen(event, () => this.#stop());
    listen('volumechange', () => this.#volume());
    listen('timeupdate', () => {
      if (this.#sources.length && Math.abs(this.#position() - this.#target()) > DRIFT) this.#start();
    });
  }

  /** The video's volume and mute button still work. */
  #volume() {
    if (this.#nodes) this.#nodes.master.gain.value = this.#video.muted ? 0 : this.#video.volume;
  }

  /** Where the tracks are now (video time). */
  #position() {
    const ctx = audio();
    return this.#origin.video + (ctx.currentTime - this.#origin.context) * this.#origin.rate;
  }

  /** Where they should be: what the video shows once our sound reaches the speakers. */
  #target() {
    const ctx = audio();
    return this.#video.currentTime + (ctx.outputLatency || ctx.baseLatency || 0) * this.#video.playbackRate;
  }

  #start() {
    if (!this.#nodes || this.#closed) return;
    this.#stop();
    const ctx = audio();
    if (ctx.state !== 'running') ctx.resume().catch(() => {});
    const offset = this.#target();
    const rate = this.#video.playbackRate;
    this.#origin = { video: offset, context: ctx.currentTime, rate };
    const outputs = [this.#nodes.game, this.#nodes.mic];
    this.#sources = this.#buffers.map((buffer, i) => {
      const source = ctx.createBufferSource();
      source.buffer = buffer;
      source.playbackRate.value = rate;
      source.connect(outputs[i]);
      if (offset < buffer.duration) source.start(0, Math.max(0, offset));
      return source;
    });
  }

  #stop() {
    for (const source of this.#sources) {
      try {
        source.stop();
      } catch {
        // never started (past the end)
      }
      source.disconnect();
    }
    this.#sources = [];
  }
}
