// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

// HeronScript Web Audio: the AudioContext graph API with a real clock,
// parameter automation, scheduling and `ended` events, and native audio
// decoding (decodeAudioData). There is no audio output device yet, so the
// graph renders silence: analysers read silence and OfflineAudioContext
// renders the buffer sources it can reach.
(function (global) {
  'use strict';
  if (typeof global.AudioContext !== 'undefined') return;
  const N = global.__n;
  const B = global.__ohb_builtin || {};
  const activation = B.userActivation || { hasBeenActive: true };
  const waiters = B.activationWaiters || [];
  const FLT_MAX = 3.4028234663852886e38;
  const hidden = (o, k, v) => Object.defineProperty(o, k, { value: v, writable: true, configurable: true });
  const finite = (v, what) => { v = Number(v); if (!isFinite(v)) throw new TypeError(`Failed to execute '${what}': The provided float value is non-finite.`); return v; };
  const domErr = (msg, name) => new DOMException(msg, name);

  // ------------------------------------------------------------ AudioParam
  class AudioParam {
    constructor(ctx, value, min = -FLT_MAX, max = FLT_MAX, rate = 'a-rate', fixedRate = false) {
      hidden(this, '_ctx', ctx); hidden(this, '_v0', value); hidden(this, '_def', value);
      hidden(this, '_min', min); hidden(this, '_max', max); hidden(this, '_rate', rate); hidden(this, '_fixed', fixedRate); hidden(this, '_ev', []);
    }
    get defaultValue() { return this._def; }
    get minValue() { return this._min; }
    get maxValue() { return this._max; }
    get automationRate() { return this._rate; }
    set automationRate(v) { if (this._fixed && v !== this._rate) throw domErr(`Failed to set the 'automationRate' property on 'AudioParam': automationRate cannot be changed`, 'InvalidStateError'); if (v === 'a-rate' || v === 'k-rate') this._rate = v; }
    get value() { return this._at(this._ctx.currentTime); }
    set value(v) {
      v = finite(v, 'value');
      this._v0 = v;
      const t = this._ctx.currentTime;
      if (this._ev.length) this._insert({ type: 'set', t, v });
    }
    _insert(e) {
      const ev = this._ev; let i = ev.length;
      while (i > 0 && ev[i - 1].t > e.t) i--;
      ev.splice(i, 0, e);
      return this;
    }
    _time(t, what) { t = finite(t, what); if (t < 0) throw new RangeError(`Failed to execute '${what}' on 'AudioParam': The start time provided (${t}) is less than the minimum bound (0).`); return Math.max(t, 0); }
    setValueAtTime(v, t) { return this._insert({ type: 'set', v: finite(v, 'setValueAtTime'), t: this._time(t, 'setValueAtTime') }); }
    linearRampToValueAtTime(v, t) { return this._insert({ type: 'linear', v: finite(v, 'linearRampToValueAtTime'), t: this._time(t, 'linearRampToValueAtTime') }); }
    exponentialRampToValueAtTime(v, t) {
      v = finite(v, 'exponentialRampToValueAtTime');
      if (v === 0) throw new RangeError("Failed to execute 'exponentialRampToValueAtTime' on 'AudioParam': The float target value provided (0) should not be in the range (-1.40130e-45, 1.40130e-45).");
      return this._insert({ type: 'exp', v, t: this._time(t, 'exponentialRampToValueAtTime') });
    }
    setTargetAtTime(v, t, tc) {
      tc = finite(tc, 'setTargetAtTime');
      if (tc < 0) throw new RangeError("Failed to execute 'setTargetAtTime' on 'AudioParam': Time constant must be non-negative.");
      return this._insert({ type: 'target', v: finite(v, 'setTargetAtTime'), t: this._time(t, 'setTargetAtTime'), tc });
    }
    setValueCurveAtTime(curve, t, d) {
      const c = Array.from(curve, Number);
      if (c.length < 2) throw domErr("Failed to execute 'setValueCurveAtTime' on 'AudioParam': The curve length provided (" + c.length + ') is less than the minimum bound (2).', 'InvalidStateError');
      d = finite(d, 'setValueCurveAtTime');
      if (d <= 0) throw new RangeError("Failed to execute 'setValueCurveAtTime' on 'AudioParam': The duration provided (" + d + ') is less than or equal to the minimum bound (0).');
      return this._insert({ type: 'curve', c, t: this._time(t, 'setValueCurveAtTime'), d });
    }
    cancelScheduledValues(t) { t = this._time(t, 'cancelScheduledValues'); this._ev = this._ev.filter((e) => e.t < t); return this; }
    cancelAndHoldAtTime(t) { t = this._time(t, 'cancelAndHoldAtTime'); const v = this._at(t); this._ev = this._ev.filter((e) => e.t < t); return this._insert({ type: 'set', v, t }); }
    // The automation timeline (Web Audio §1.6.3) evaluated at time t.
    _at(t) {
      let v = this._v0, t0 = 0, target = null;
      for (const e of this._ev) {
        if (e.t > t) {
          if (target) break;
          if (e.type === 'linear') return this._clamp(v + (e.v - v) * (t - t0) / Math.max(e.t - t0, 1e-9));
          if (e.type === 'exp') return this._clamp(v !== 0 && v * e.v > 0 ? v * Math.pow(e.v / v, (t - t0) / Math.max(e.t - t0, 1e-9)) : v);
          break;
        }
        if (target) { v = target.v + (v - target.v) * Math.exp(-(e.t - target.t) / (target.tc || 1e-9)); target = null; }
        if (e.type === 'target') { target = e; t0 = e.t; continue; }
        if (e.type === 'curve') {
          if (t < e.t + e.d) { const k = (t - e.t) / e.d * (e.c.length - 1), i = Math.floor(k); return this._clamp(e.c[i] + (e.c[Math.min(i + 1, e.c.length - 1)] - e.c[i]) * (k - i)); }
          v = e.c[e.c.length - 1]; t0 = e.t + e.d; continue;
        }
        v = e.v; t0 = e.t;
      }
      if (target) v = target.v + (v - target.v) * Math.exp(-(t - target.t) / (target.tc || 1e-9));
      return this._clamp(v);
    }
    _clamp(v) { return Math.min(this._max, Math.max(this._min, v)); }
  }

  // ------------------------------------------------------------ AudioBuffer
  class AudioBuffer {
    constructor(options) {
      if (options === null || typeof options !== 'object') throw new TypeError("Failed to construct 'AudioBuffer': 1 argument required, but only 0 present.");
      const ch = options.numberOfChannels === undefined ? 1 : Number(options.numberOfChannels), len = Number(options.length), rate = Number(options.sampleRate);
      if (!(ch >= 1 && ch <= 32)) throw domErr(`Failed to construct 'AudioBuffer': The number of channels provided (${ch}) is outside the range [1, 32].`, 'NotSupportedError');
      if (!(len >= 1)) throw domErr(`Failed to construct 'AudioBuffer': The number of frames provided (${len}) is less than or equal to the minimum bound (0).`, 'NotSupportedError');
      if (!(rate >= 3000 && rate <= 768000)) throw domErr(`Failed to construct 'AudioBuffer': The sample rate provided (${rate}) is outside the range [3000, 768000].`, 'NotSupportedError');
      hidden(this, '_rate', rate); hidden(this, '_len', Math.floor(len));
      hidden(this, '_data', options._data || Array.from({ length: ch }, () => new Float32Array(Math.floor(len))));
    }
    get sampleRate() { return this._rate; }
    get length() { return this._len; }
    get duration() { return this._len / this._rate; }
    get numberOfChannels() { return this._data.length; }
    getChannelData(c) {
      if (!(c >= 0 && c < this._data.length)) throw domErr(`Failed to execute 'getChannelData' on 'AudioBuffer': channel index (${c}) exceeds number of channels (${this._data.length})`, 'IndexSizeError');
      return this._data[c];
    }
    copyFromChannel(dest, c, start = 0) { const src = this.getChannelData(c); const n = Math.max(0, Math.min(dest.length, src.length - start)); dest.set(src.subarray(start, start + n)); }
    copyToChannel(src, c, start = 0) { const d = this.getChannelData(c); const n = Math.max(0, Math.min(src.length, d.length - start)); d.set(src.subarray(0, n), start); }
  }

  // ------------------------------------------------------------ nodes
  const BaseCtx = () => BaseAudioContext;
  class AudioNode extends EventTarget {
    constructor(ctx, ins, outs, opts) {
      super();
      if (!(ctx instanceof BaseCtx())) throw new TypeError(`Failed to construct '${new.target.name}': parameter 1 is not of type 'BaseAudioContext'.`);
      hidden(this, '_ctx', ctx); hidden(this, '_ins', ins); hidden(this, '_outs', outs); hidden(this, '_out', new Set());
      hidden(this, '_cc', 2); hidden(this, '_ccm', 'max'); hidden(this, '_ci', 'speakers');
      if (opts) {
        if (opts.channelCount !== undefined) this.channelCount = opts.channelCount;
        if (opts.channelCountMode !== undefined) this.channelCountMode = opts.channelCountMode;
        if (opts.channelInterpretation !== undefined) this.channelInterpretation = opts.channelInterpretation;
      }
    }
    get context() { return this._ctx; }
    get numberOfInputs() { return this._ins; }
    get numberOfOutputs() { return this._outs; }
    get channelCount() { return this._cc; }
    set channelCount(v) { v = Number(v); if (!(v >= 1 && v <= 32)) throw domErr(`Failed to set the 'channelCount' property on 'AudioNode': The channel count provided (${v}) is outside the range [1, 32].`, 'NotSupportedError'); this._cc = v; }
    get channelCountMode() { return this._ccm; }
    set channelCountMode(v) { if (['max', 'clamped-max', 'explicit'].includes(v)) this._ccm = v; }
    get channelInterpretation() { return this._ci; }
    set channelInterpretation(v) { if (['speakers', 'discrete'].includes(v)) this._ci = v; }
    connect(dest, output = 0, input = 0) {
      if (dest instanceof AudioParam) {
        if (dest._ctx !== this._ctx) throw domErr("Failed to execute 'connect' on 'AudioNode': cannot connect to an AudioParam belonging to a different audio context.", 'InvalidAccessError');
        this._out.add(dest); return undefined;
      }
      if (!(dest instanceof AudioNode)) throw new TypeError("Failed to execute 'connect' on 'AudioNode': Overload resolution failed.");
      if (dest._ctx !== this._ctx) throw domErr("Failed to execute 'connect' on 'AudioNode': cannot connect to an AudioNode belonging to a different audio context.", 'InvalidAccessError');
      if (output >= this._outs) throw domErr(`Failed to execute 'connect' on 'AudioNode': output index (${output}) exceeds number of outputs (${this._outs}).`, 'IndexSizeError');
      if (input >= dest._ins) throw domErr(`Failed to execute 'connect' on 'AudioNode': input index (${input}) exceeds number of inputs (${dest._ins}).`, 'IndexSizeError');
      this._out.add(dest);
      return dest;
    }
    disconnect(dest) {
      if (dest === undefined || typeof dest === 'number') { this._out.clear(); return; }
      if (!this._out.has(dest)) throw domErr("Failed to execute 'disconnect' on 'AudioNode': the given destination is not connected.", 'InvalidAccessError');
      this._out.delete(dest);
    }
  }
  class AudioDestinationNode extends AudioNode {
    constructor(ctx, channels) { super(ctx, 1, 0); hidden(this, '_max', channels || 2); this._ccm = 'explicit'; }
    get maxChannelCount() { return this._max; }
  }
  class GainNode extends AudioNode {
    constructor(ctx, o = {}) { super(ctx, 1, 1, o); hidden(this, '_gain', new AudioParam(ctx, o.gain === undefined ? 1 : Number(o.gain))); }
    get gain() { return this._gain; }
  }
  // Sources: start/stop scheduling and `ended` on the context clock.
  class AudioScheduledSourceNode extends AudioNode {
    constructor(ctx, o) { super(ctx, 0, 1, o); hidden(this, '_start', null); hidden(this, '_stop', Infinity); hidden(this, '_timer', 0); hidden(this, '_done', false); }
    start(when = 0) {
      if (this._start !== null) throw domErr("Failed to execute 'start' on 'AudioScheduledSourceNode': cannot call start more than once.", 'InvalidStateError');
      when = finite(when, 'start');
      if (when < 0) throw new RangeError(`Failed to execute 'start' on 'AudioScheduledSourceNode': The start time provided (${when}) is less than the minimum bound (0).`);
      this._start = when; this._ctx._sources.add(this); this._arm();
    }
    stop(when = 0) {
      if (this._start === null) throw domErr("Failed to execute 'stop' on 'AudioScheduledSourceNode': cannot call stop without calling start first.", 'InvalidStateError');
      when = finite(when, 'stop');
      if (when < 0) throw new RangeError(`Failed to execute 'stop' on 'AudioScheduledSourceNode': The stop time provided (${when}) is less than the minimum bound (0).`);
      this._stop = when; this._arm();
    }
    _end() { return this._stop; }
    _arm() {
      clearTimeout(this._timer); this._timer = 0;
      if (this._done || this._start === null || this._ctx._state !== 'running') return;
      const end = Math.max(this._end(), this._start);
      if (!isFinite(end)) return;
      const wait = (end - this._ctx.currentTime) * 1000;
      if (wait <= 0) { this._finish(); return; }
      this._timer = setTimeout(() => this._arm(), Math.min(wait + 1, 2 ** 31 - 1));
    }
    _finish() {
      if (this._done) return;
      this._done = true; this._ctx._sources.delete(this);
      this.dispatchEvent(new Event('ended'));
    }
  }
  for (const C of [AudioScheduledSourceNode]) Object.defineProperty(C.prototype, 'onended', { get() { return this._onended || null; }, set(f) { this._onended = typeof f === 'function' ? f : null; }, configurable: true });
  class AudioBufferSourceNode extends AudioScheduledSourceNode {
    constructor(ctx, o = {}) {
      super(ctx, o);
      hidden(this, '_buf', null); hidden(this, '_offset', 0); hidden(this, '_dur', Infinity);
      hidden(this, '_rateP', new AudioParam(ctx, o.playbackRate === undefined ? 1 : Number(o.playbackRate), -FLT_MAX, FLT_MAX, 'k-rate', true));
      hidden(this, '_detune', new AudioParam(ctx, o.detune === undefined ? 0 : Number(o.detune), -FLT_MAX, FLT_MAX, 'k-rate', true));
      this.loop = !!o.loop; this.loopStart = Number(o.loopStart || 0); this.loopEnd = Number(o.loopEnd || 0);
      if (o.buffer) this.buffer = o.buffer;
    }
    get buffer() { return this._buf; }
    set buffer(b) {
      if (b !== null && !(b instanceof AudioBuffer)) throw new TypeError("Failed to set the 'buffer' property on 'AudioBufferSourceNode': The provided value is not of type 'AudioBuffer'.");
      if (b && this._buf) throw domErr("Failed to set the 'buffer' property on 'AudioBufferSourceNode': Cannot set buffer to non-null after it has been already been set to a non-null buffer.", 'InvalidStateError');
      this._buf = b; this._arm();
    }
    get playbackRate() { return this._rateP; }
    get detune() { return this._detune; }
    start(when = 0, offset = 0, duration) {
      this._offset = Math.max(0, finite(offset, 'start'));
      if (duration !== undefined) { duration = finite(duration, 'start'); if (duration < 0) throw new RangeError(`Failed to execute 'start' on 'AudioBufferSourceNode': The duration provided (${duration}) is less than the minimum bound (0).`); this._dur = duration; }
      super.start(when);
    }
    _end() {
      let end = this._stop;
      if (this._buf && !this.loop) {
        const rate = Math.abs(this._rateP.value * Math.pow(2, this._detune.value / 1200)) || 1;
        end = Math.min(end, this._start + Math.min(this._dur, Math.max(0, this._buf.duration - this._offset)) / rate);
      } else if (!this._buf) end = Math.min(end, this._dur === Infinity ? Infinity : this._start + this._dur);
      else if (this._dur !== Infinity) end = Math.min(end, this._start + this._dur);
      return end;
    }
  }
  class OscillatorNode extends AudioScheduledSourceNode {
    constructor(ctx, o = {}) {
      super(ctx, o);
      hidden(this, '_type', 'sine');
      hidden(this, '_freq', new AudioParam(ctx, o.frequency === undefined ? 440 : Number(o.frequency), -ctx.sampleRate / 2, ctx.sampleRate / 2));
      hidden(this, '_detune', new AudioParam(ctx, o.detune === undefined ? 0 : Number(o.detune), -153600, 153600));
      if (o.periodicWave) this.setPeriodicWave(o.periodicWave); else if (o.type) this.type = o.type;
    }
    get type() { return this._type; }
    set type(v) {
      if (v === 'custom') throw domErr("Failed to set the 'type' property on 'OscillatorNode': 'type' cannot be set directly to 'custom'.  Use setPeriodicWave() to create a custom Oscillator type.", 'InvalidStateError');
      if (['sine', 'square', 'sawtooth', 'triangle'].includes(v)) this._type = v;
    }
    get frequency() { return this._freq; }
    get detune() { return this._detune; }
    setPeriodicWave(w) { if (!(w instanceof PeriodicWave)) throw new TypeError("Failed to execute 'setPeriodicWave' on 'OscillatorNode': parameter 1 is not of type 'PeriodicWave'."); this._type = 'custom'; }
  }
  class ConstantSourceNode extends AudioScheduledSourceNode {
    constructor(ctx, o = {}) { super(ctx, o); hidden(this, '_offsetP', new AudioParam(ctx, o.offset === undefined ? 1 : Number(o.offset))); }
    get offset() { return this._offsetP; }
  }
  const params = (C, defs) => {
    for (const [k, def, min, max, rate, fixed] of defs) Object.defineProperty(C.prototype, k, { get() { let p = this['_p_' + k]; if (!p) { p = new AudioParam(this._ctx, def, min, max, rate, fixed); if (this._o && this._o[k] !== undefined) p._v0 = Number(this._o[k]); hidden(this, '_p_' + k, p); } return p; }, configurable: true });
  };
  class BiquadFilterNode extends AudioNode {
    constructor(ctx, o = {}) { super(ctx, 1, 1, o); hidden(this, '_o', o); hidden(this, '_type', 'lowpass'); if (o.type) this.type = o.type; }
    get type() { return this._type; }
    set type(v) { if (['lowpass', 'highpass', 'bandpass', 'lowshelf', 'highshelf', 'peaking', 'notch', 'allpass'].includes(v)) this._type = v; }
    getFrequencyResponse(freq, mag, phase) {
      if (mag.length < freq.length || phase.length < freq.length) throw domErr("Failed to execute 'getFrequencyResponse' on 'BiquadFilterNode': The length of the arrays must be at least the length of frequencyHz.", 'InvalidAccessError');
      const coef = biquad(this._type, this.frequency.value, this.Q.value, this.gain.value, this.detune.value, this._ctx.sampleRate);
      for (let i = 0; i < freq.length; i++) { const r = responseAt(coef, freq[i], this._ctx.sampleRate); mag[i] = r[0]; phase[i] = r[1]; }
    }
  }
  params(BiquadFilterNode, [['frequency', 350, 0, 24000], ['detune', 0, -153600, 153600], ['Q', 1], ['gain', 0, -FLT_MAX, 1541]]);
  // Audio EQ Cookbook coefficients (normalized by a0).
  function biquad(type, f0, Q, gain, detune, rate) {
    const f = f0 * Math.pow(2, detune / 1200), w0 = 2 * Math.PI * f / rate, cos = Math.cos(w0), sin = Math.sin(w0);
    const A = Math.pow(10, gain / 40), alphaQ = sin / (2 * Math.pow(10, Q / 20)), alpha = sin / (2 * (Q || 1e-4));
    let b0, b1, b2, a0, a1, a2;
    switch (type) {
      case 'highpass': b0 = (1 + cos) / 2; b1 = -(1 + cos); b2 = b0; a0 = 1 + alphaQ; a1 = -2 * cos; a2 = 1 - alphaQ; break;
      case 'bandpass': b0 = alpha; b1 = 0; b2 = -alpha; a0 = 1 + alpha; a1 = -2 * cos; a2 = 1 - alpha; break;
      case 'notch': b0 = 1; b1 = -2 * cos; b2 = 1; a0 = 1 + alpha; a1 = -2 * cos; a2 = 1 - alpha; break;
      case 'allpass': b0 = 1 - alpha; b1 = -2 * cos; b2 = 1 + alpha; a0 = 1 + alpha; a1 = -2 * cos; a2 = 1 - alpha; break;
      case 'peaking': b0 = 1 + alpha * A; b1 = -2 * cos; b2 = 1 - alpha * A; a0 = 1 + alpha / A; a1 = -2 * cos; a2 = 1 - alpha / A; break;
      case 'lowshelf': { const s = 2 * Math.sqrt(A) * sin / 2 * Math.SQRT2; b0 = A * ((A + 1) - (A - 1) * cos + s); b1 = 2 * A * ((A - 1) - (A + 1) * cos); b2 = A * ((A + 1) - (A - 1) * cos - s); a0 = (A + 1) + (A - 1) * cos + s; a1 = -2 * ((A - 1) + (A + 1) * cos); a2 = (A + 1) + (A - 1) * cos - s; break; }
      case 'highshelf': { const s = 2 * Math.sqrt(A) * sin / 2 * Math.SQRT2; b0 = A * ((A + 1) + (A - 1) * cos + s); b1 = -2 * A * ((A - 1) + (A + 1) * cos); b2 = A * ((A + 1) + (A - 1) * cos - s); a0 = (A + 1) - (A - 1) * cos + s; a1 = 2 * ((A - 1) - (A + 1) * cos); a2 = (A + 1) - (A - 1) * cos - s; break; }
      default: b0 = (1 - cos) / 2; b1 = 1 - cos; b2 = b0; a0 = 1 + alphaQ; a1 = -2 * cos; a2 = 1 - alphaQ;
    }
    return { b: [b0 / a0, b1 / a0, b2 / a0], a: [1, a1 / a0, a2 / a0] };
  }
  function responseAt(c, f, rate) {
    if (!(f >= 0 && f <= rate / 2)) return [NaN, NaN];
    const w = 2 * Math.PI * f / rate;
    const ev = (k) => { let re = 0, im = 0; for (let i = 0; i < k.length; i++) { re += k[i] * Math.cos(-w * i); im += k[i] * Math.sin(-w * i); } return [re, im]; };
    const [nr, ni] = ev(c.b), [dr, di] = ev(c.a);
    const d = dr * dr + di * di, re = (nr * dr + ni * di) / d, im = (ni * dr - nr * di) / d;
    return [Math.hypot(re, im), Math.atan2(im, re)];
  }
  class IIRFilterNode extends AudioNode {
    constructor(ctx, o) {
      super(ctx, 1, 1, o);
      if (!o || !o.feedforward || !o.feedback) throw new TypeError("Failed to construct 'IIRFilterNode': required member feedforward is undefined.");
      const ff = Array.from(o.feedforward, Number), fb = Array.from(o.feedback, Number);
      if (fb[0] === 0) throw domErr("Failed to construct 'IIRFilterNode': First feedback coefficient cannot be zero.", 'InvalidStateError');
      hidden(this, '_c', { b: ff.map((x) => x / fb[0]), a: fb.map((x) => x / fb[0]) });
    }
    getFrequencyResponse(freq, mag, phase) { for (let i = 0; i < freq.length; i++) { const r = responseAt(this._c, freq[i], this._ctx.sampleRate); mag[i] = r[0]; phase[i] = r[1]; } }
  }
  class DelayNode extends AudioNode {
    constructor(ctx, o = {}) {
      super(ctx, 1, 1, o);
      const max = o.maxDelayTime === undefined ? 1 : Number(o.maxDelayTime);
      if (!(max > 0 && max < 180)) throw domErr(`Failed to construct 'DelayNode': The max delay time provided (${max}) is outside the range (0, 180).`, 'NotSupportedError');
      hidden(this, '_delay', new AudioParam(ctx, o.delayTime === undefined ? 0 : Number(o.delayTime), 0, max));
    }
    get delayTime() { return this._delay; }
  }
  class DynamicsCompressorNode extends AudioNode {
    constructor(ctx, o = {}) { super(ctx, 1, 1, o); hidden(this, '_o', o); this._ccm = 'clamped-max'; }
    get reduction() { return 0; }
  }
  params(DynamicsCompressorNode, [['threshold', -24, -100, 0, 'k-rate', true], ['knee', 30, 0, 40, 'k-rate', true], ['ratio', 12, 1, 20, 'k-rate', true], ['attack', 0.003, 0, 1, 'k-rate', true], ['release', 0.25, 0, 1, 'k-rate', true]]);
  class StereoPannerNode extends AudioNode {
    constructor(ctx, o = {}) { super(ctx, 1, 1, o); hidden(this, '_o', o); this._ccm = 'clamped-max'; }
  }
  params(StereoPannerNode, [['pan', 0, -1, 1]]);
  class PannerNode extends AudioNode {
    constructor(ctx, o = {}) {
      super(ctx, 1, 1, o); hidden(this, '_o', o); this._ccm = 'clamped-max';
      this.panningModel = o.panningModel || 'equalpower'; this.distanceModel = o.distanceModel || 'inverse';
      this.refDistance = o.refDistance === undefined ? 1 : o.refDistance; this.maxDistance = o.maxDistance === undefined ? 10000 : o.maxDistance;
      this.rolloffFactor = o.rolloffFactor === undefined ? 1 : o.rolloffFactor; this.coneInnerAngle = o.coneInnerAngle === undefined ? 360 : o.coneInnerAngle;
      this.coneOuterAngle = o.coneOuterAngle === undefined ? 360 : o.coneOuterAngle; this.coneOuterGain = o.coneOuterGain || 0;
    }
    setPosition(x, y, z) { this.positionX.value = x; this.positionY.value = y; this.positionZ.value = z; }
    setOrientation(x, y, z) { this.orientationX.value = x; this.orientationY.value = y; this.orientationZ.value = z; }
  }
  params(PannerNode, [['positionX', 0], ['positionY', 0], ['positionZ', 0], ['orientationX', 1], ['orientationY', 0], ['orientationZ', 0]]);
  class AnalyserNode extends AudioNode {
    constructor(ctx, o = {}) {
      super(ctx, 1, 1, o);
      hidden(this, '_fft', 2048); this.minDecibels = -100; this.maxDecibels = -30; this.smoothingTimeConstant = 0.8;
      for (const k of ['fftSize', 'minDecibels', 'maxDecibels', 'smoothingTimeConstant']) if (o[k] !== undefined) this[k] = o[k];
    }
    get fftSize() { return this._fft; }
    set fftSize(v) { v = Number(v); if (!(v >= 32 && v <= 32768 && (v & (v - 1)) === 0)) throw domErr(`Failed to set the 'fftSize' property on 'AnalyserNode': The value provided (${v}) is not a power of two, or is outside the range [32, 32768].`, 'IndexSizeError'); this._fft = v; }
    get frequencyBinCount() { return this._fft / 2; }
    getFloatFrequencyData(a) { a.fill(-Infinity); }
    getByteFrequencyData(a) { a.fill(0); }
    getFloatTimeDomainData(a) { a.fill(0); }
    getByteTimeDomainData(a) { a.fill(128); }
  }
  class ConvolverNode extends AudioNode {
    constructor(ctx, o = {}) { super(ctx, 1, 1, o); hidden(this, '_buf', null); this.normalize = !o.disableNormalization; this._ccm = 'clamped-max'; if (o.buffer) this.buffer = o.buffer; }
    get buffer() { return this._buf; }
    set buffer(b) { if (b !== null && !(b instanceof AudioBuffer)) throw new TypeError("Failed to set the 'buffer' property on 'ConvolverNode': The provided value is not of type 'AudioBuffer'."); this._buf = b; }
  }
  class WaveShaperNode extends AudioNode {
    constructor(ctx, o = {}) { super(ctx, 1, 1, o); hidden(this, '_curve', null); this.oversample = o.oversample || 'none'; if (o.curve) this.curve = o.curve; }
    get curve() { return this._curve; }
    set curve(c) { if (c !== null && c.length < 2) throw domErr("Failed to set the 'curve' property on 'WaveShaperNode': The curve length provided (" + c.length + ') is less than the minimum bound (2).', 'InvalidAccessError'); this._curve = c === null ? null : Float32Array.from(c); }
  }
  class ChannelSplitterNode extends AudioNode {
    constructor(ctx, o = {}) { const n = o.numberOfOutputs === undefined ? 6 : Number(o.numberOfOutputs); super(ctx, 1, n, o); this._cc = n; this._ccm = 'explicit'; this._ci = 'discrete'; }
  }
  class ChannelMergerNode extends AudioNode {
    constructor(ctx, o = {}) { const n = o.numberOfInputs === undefined ? 6 : Number(o.numberOfInputs); super(ctx, n, 1, o); this._cc = 1; this._ccm = 'explicit'; }
  }
  class AudioProcessingEvent extends Event {
    constructor(type, init) { super(type, init); this.playbackTime = init.playbackTime; this.inputBuffer = init.inputBuffer; this.outputBuffer = init.outputBuffer; }
  }
  class ScriptProcessorNode extends AudioNode {
    constructor(ctx, size, ins, outs) {
      super(ctx, 1, 1);
      hidden(this, '_size', size); hidden(this, '_inCh', ins); hidden(this, '_outCh', outs); hidden(this, '_timer', 0); hidden(this, '_onap', null);
    }
    get bufferSize() { return this._size; }
    get onaudioprocess() { return this._onap; }
    set onaudioprocess(f) { this._onap = typeof f === 'function' ? f : null; this._pump(); }
    connect(d, o, i) { const r = super.connect(d, o, i); this._pump(); return r; }
    _pump() {
      if (this._timer || !this._onap) return;
      const period = this._size / this._ctx.sampleRate * 1000;
      const tick = () => {
        this._timer = 0;
        if (!this._onap || this._ctx._state === 'closed') return;
        if (this._ctx._state === 'running') {
          const mk = (n) => new AudioBuffer({ numberOfChannels: Math.max(1, n), length: this._size, sampleRate: this._ctx.sampleRate });
          const ev = new AudioProcessingEvent('audioprocess', { playbackTime: this._ctx.currentTime, inputBuffer: mk(this._inCh), outputBuffer: mk(this._outCh) });
          try { this._onap.call(this, ev); } catch (e) { setTimeout(() => { throw e; }, 0); }
        }
        this._timer = setTimeout(tick, Math.max(period, 16));
      };
      this._timer = setTimeout(tick, Math.max(period, 16));
    }
  }
  class MediaElementAudioSourceNode extends AudioNode {
    constructor(ctx, o) { super(ctx, 0, 1); if (!o || !(o.mediaElement instanceof HTMLMediaElement)) throw new TypeError("Failed to construct 'MediaElementAudioSourceNode': required member mediaElement is undefined."); hidden(this, '_el', o.mediaElement); }
    get mediaElement() { return this._el; }
  }
  // Media streams (no capture devices; streams are empty or synthetic).
  if (typeof global.MediaStream === 'undefined') {
    let seq = 0;
    class MediaStreamTrack extends EventTarget {
      constructor(kind, label) { super(); this.kind = kind; this.label = label || ''; this.id = 'track-' + (++seq); this.enabled = true; this.muted = false; this.readyState = 'live'; this.contentHint = ''; }
      stop() { this.readyState = 'ended'; }
      clone() { return new MediaStreamTrack(this.kind, this.label); }
      getSettings() { return {}; } getConstraints() { return {}; } getCapabilities() { return {}; } applyConstraints() { return Promise.resolve(); }
    }
    class MediaStream extends EventTarget {
      constructor(tracks) { super(); this.id = 'stream-' + (++seq); this._t = tracks instanceof MediaStream ? tracks.getTracks() : Array.from(tracks || []); }
      get active() { return this._t.some((t) => t.readyState === 'live'); }
      getTracks() { return this._t.slice(); } getAudioTracks() { return this._t.filter((t) => t.kind === 'audio'); } getVideoTracks() { return this._t.filter((t) => t.kind === 'video'); }
      getTrackById(id) { return this._t.find((t) => t.id === id) || null; } addTrack(t) { if (!this._t.includes(t)) this._t.push(t); } removeTrack(t) { this._t = this._t.filter((x) => x !== t); }
      clone() { return new MediaStream(this._t.map((t) => t.clone())); }
    }
    hidden(global, 'MediaStream', MediaStream); hidden(global, 'MediaStreamTrack', MediaStreamTrack);
  }
  class MediaStreamAudioSourceNode extends AudioNode {
    constructor(ctx, o) { super(ctx, 0, 1); if (!o || !o.mediaStream) throw new TypeError("Failed to construct 'MediaStreamAudioSourceNode': required member mediaStream is undefined."); if (!o.mediaStream.getAudioTracks().length) throw domErr("Failed to construct 'MediaStreamAudioSourceNode': MediaStream has no audio track", 'InvalidStateError'); hidden(this, '_s', o.mediaStream); }
    get mediaStream() { return this._s; }
  }
  class MediaStreamAudioDestinationNode extends AudioNode {
    constructor(ctx, o) { super(ctx, 1, 0, o); hidden(this, '_s', new global.MediaStream([new global.MediaStreamTrack('audio', 'MediaStreamAudioDestinationNode')])); this._ccm = 'explicit'; }
    get stream() { return this._s; }
  }
  class AudioWorkletNode extends AudioNode {
    constructor(ctx, name, o = {}) {
      super(ctx, o.numberOfInputs === undefined ? 1 : o.numberOfInputs, o.numberOfOutputs === undefined ? 1 : o.numberOfOutputs, o);
      if (!ctx._worklets.has(String(name))) throw domErr(`Failed to construct 'AudioWorkletNode': AudioWorkletNode cannot be created: The node name '${name}' is not defined in AudioWorkletGlobalScope.`, 'InvalidStateError');
      const ch = new MessageChannel(); hidden(this, '_port', ch.port1);
      const params = new Map(); hidden(this, '_params', params);
      for (const d of ctx._worklets.get(String(name)) || []) params.set(d.name, new AudioParam(ctx, d.defaultValue || 0, d.minValue === undefined ? -FLT_MAX : d.minValue, d.maxValue === undefined ? FLT_MAX : d.maxValue));
    }
    get port() { return this._port; }
    get parameters() { return this._params; }
  }
  class PeriodicWave {
    constructor(ctx, o = {}) {
      if (!(ctx instanceof BaseAudioContext)) throw new TypeError("Failed to construct 'PeriodicWave': parameter 1 is not of type 'BaseAudioContext'.");
      const re = o.real ? Float32Array.from(o.real) : null, im = o.imag ? Float32Array.from(o.imag) : null;
      if (re && im && re.length !== im.length) throw domErr("Failed to construct 'PeriodicWave': length of real array (" + re.length + ') and length of imaginary array (' + im.length + ') must match.', 'IndexSizeError');
      hidden(this, '_re', re); hidden(this, '_im', im);
    }
  }
  class AudioListener {
    constructor(ctx) { hidden(this, '_ctx', ctx); hidden(this, '_o', null); }
    setPosition(x, y, z) { this.positionX.value = x; this.positionY.value = y; this.positionZ.value = z; }
    setOrientation(x, y, z, ux, uy, uz) { this.forwardX.value = x; this.forwardY.value = y; this.forwardZ.value = z; this.upX.value = ux; this.upY.value = uy; this.upZ.value = uz; }
  }
  params(AudioListener, [['positionX', 0], ['positionY', 0], ['positionZ', 0], ['forwardX', 0], ['forwardY', 0], ['forwardZ', -1], ['upX', 0], ['upY', 1], ['upZ', 0]]);

  // ------------------------------------------------------------ contexts
  class AudioWorklet {
    constructor(ctx) { hidden(this, '_ctx', ctx); }
    addModule(url) {
      // The module is fetched (so failures surface like Chrome's) but processors don't run;
      // registerProcessor calls are recorded so AudioWorkletNode can be constructed.
      const ctx = this._ctx;
      return fetch(url).then((r) => { if (!r.ok) throw domErr('Unable to load a worklet\'s module.', 'AbortError'); return r.text(); }).then((src) => {
        const re = /registerProcessor\s*\(\s*(['"`])([^'"`]+)\1/g; let m;
        while ((m = re.exec(src))) if (!ctx._worklets.has(m[2])) ctx._worklets.set(m[2], []);
      });
    }
  }
  const contexts = new Set();
  class BaseAudioContext extends EventTarget {
    constructor(rate, channels) {
      super();
      if (new.target === BaseAudioContext) throw new TypeError('Illegal constructor');
      hidden(this, '_rate', rate); hidden(this, '_state', 'suspended'); hidden(this, '_elapsed', 0); hidden(this, '_since', 0);
      hidden(this, '_sources', new Set()); hidden(this, '_worklets', new Map());
      hidden(this, '_dest', new AudioDestinationNode(this, channels)); hidden(this, '_listener', new AudioListener(this)); hidden(this, '_worklet', new AudioWorklet(this));
      hidden(this, '_onstatechange', null);
    }
    get sampleRate() { return this._rate; }
    // The rendering clock, quantized to 128-frame render quanta like Chrome's.
    get currentTime() {
      let t = this._elapsed;
      if (this._state === 'running') t += (performance.now() - this._since) / 1000;
      return Math.floor(t * this._rate / 128) * 128 / this._rate;
    }
    get destination() { return this._dest; }
    get listener() { return this._listener; }
    get state() { return this._state; }
    get audioWorklet() { return this._worklet; }
    get onstatechange() { return this._onstatechange; }
    set onstatechange(f) { this._onstatechange = typeof f === 'function' ? f : null; }
    _setState(s) {
      if (this._state === s) return;
      if (this._state === 'running') this._elapsed += (performance.now() - this._since) / 1000;
      this._state = s;
      if (s === 'running') this._since = performance.now();
      for (const src of this._sources) src._arm();
      setTimeout(() => this.dispatchEvent(new Event('statechange')), 0);
    }
    _alive(what) { if (this._state === 'closed' && what) { /* Chrome still creates nodes on closed contexts */ } }
    createGain() { return new GainNode(this); }
    createBuffer(ch, len, rate) { return new AudioBuffer({ numberOfChannels: ch, length: len, sampleRate: rate }); }
    createBufferSource() { return new AudioBufferSourceNode(this); }
    createOscillator() { return new OscillatorNode(this); }
    createConstantSource() { return new ConstantSourceNode(this); }
    createBiquadFilter() { return new BiquadFilterNode(this); }
    createIIRFilter(ff, fb) { return new IIRFilterNode(this, { feedforward: ff, feedback: fb }); }
    createDelay(max) { return new DelayNode(this, { maxDelayTime: max === undefined ? 1 : max }); }
    createDynamicsCompressor() { return new DynamicsCompressorNode(this); }
    createStereoPanner() { return new StereoPannerNode(this); }
    createPanner() { return new PannerNode(this); }
    createAnalyser() { return new AnalyserNode(this); }
    createConvolver() { return new ConvolverNode(this); }
    createWaveShaper() { return new WaveShaperNode(this); }
    createChannelSplitter(n) { return new ChannelSplitterNode(this, { numberOfOutputs: n === undefined ? 6 : n }); }
    createChannelMerger(n) { return new ChannelMergerNode(this, { numberOfInputs: n === undefined ? 6 : n }); }
    createPeriodicWave(real, imag, o) { return new PeriodicWave(this, Object.assign({ real, imag }, o || {})); }
    createScriptProcessor(size = 0, ins = 2, outs = 2) {
      if (size === 0) size = 2048;
      if (![256, 512, 1024, 2048, 4096, 8192, 16384].includes(size)) throw domErr(`Failed to execute 'createScriptProcessor' on 'BaseAudioContext': buffer size (${size}) must be 0 or a power of two between 256 and 16384.`, 'IndexSizeError');
      return new ScriptProcessorNode(this, size, ins, outs);
    }
    decodeAudioData(data, ok, fail) {
      return new Promise((resolve, reject) => {
        const done = (err, buf) => setTimeout(() => {
          if (err) { if (typeof fail === 'function') { try { fail(err); } catch (e) { setTimeout(() => { throw e; }, 0); } } reject(err); }
          else { if (typeof ok === 'function') { try { ok(buf); } catch (e) { setTimeout(() => { throw e; }, 0); } } resolve(buf); }
        }, 0);
        if (!(data instanceof ArrayBuffer)) { done(new TypeError("Failed to execute 'decodeAudioData' on 'BaseAudioContext': parameter 1 is not of type 'ArrayBuffer'.")); return; }
        if (data.byteLength === 0) { done(domErr('Unable to decode audio data', 'EncodingError')); return; }
        let r = null;
        try { r = N.audioDecode(data, this._rate); } catch (_) { r = null; }
        if (!r || !r[2]) { done(domErr('Unable to decode audio data', 'EncodingError')); return; }
        const [rate, ch, frames, bytes] = r;
        const chans = [];
        for (let c = 0; c < ch; c++) chans.push(new Float32Array(bytes, c * frames * 4, frames));
        done(null, new AudioBuffer({ numberOfChannels: ch, length: frames, sampleRate: rate, _data: chans }));
      });
    }
  }
  for (const [name, C] of [['createMediaElementSource', MediaElementAudioSourceNode], ['createMediaStreamSource', MediaStreamAudioSourceNode]]) {
    BaseAudioContext.prototype[name] = function (x) { return new C(this, name === 'createMediaElementSource' ? { mediaElement: x } : { mediaStream: x }); };
  }
  class AudioContext extends BaseAudioContext {
    constructor(o = {}) {
      if (o !== null && typeof o !== 'object') throw new TypeError("Failed to construct 'AudioContext': The provided value is not of type 'AudioContextOptions'.");
      const rate = o && o.sampleRate !== undefined ? Number(o.sampleRate) : 48000;
      if (!(rate >= 3000 && rate <= 768000)) throw domErr(`Failed to construct 'AudioContext': The provided sample rate (${rate}) is outside the range [3000, 768000].`, 'NotSupportedError');
      super(rate, 2);
      hidden(this, '_resumes', []); hidden(this, '_sink', (o && o.sinkId) || '');
      hidden(this, '_latency', o && typeof o.latencyHint === 'number' ? o.latencyHint : o && o.latencyHint === 'playback' ? 0.04 : o && o.latencyHint === 'balanced' ? 0.02 : 0.01);
      contexts.add(this);
      // Autoplay policy: contexts start running only after a user gesture
      // (pending resume() calls go through once one happens).
      if (activation.hasBeenActive) this._setState('running');
    }
    get baseLatency() { return this._latency; }
    get outputLatency() { return this._state === 'running' ? 0.02 : 0; }
    get sinkId() { return this._sink; }
    createMediaStreamDestination() { return new MediaStreamAudioDestinationNode(this); }
    setSinkId(id) { this._sink = id; return Promise.resolve(); }
    getOutputTimestamp() { return { contextTime: this.currentTime, performanceTime: this._state === 'running' ? performance.now() : 0 }; }
    _grant() {
      if (this._state === 'closed') return;
      if (this._resumes.length) { this._setState('running'); for (const f of this._resumes.splice(0)) f(); }
    }
    resume() {
      if (this._state === 'closed') return Promise.reject(domErr('Cannot resume a closed AudioContext.', 'InvalidStateError'));
      return new Promise((res) => {
        if (activation.hasBeenActive) { this._setState('running'); res(); }
        else { this._resumes.push(res); if (!waiters.includes(this._grantBound)) { hidden(this, '_grantBound', () => this._grant()); waiters.push(this._grantBound); } }
      });
    }
    suspend() {
      if (this._state === 'closed') return Promise.reject(domErr('Cannot suspend a closed AudioContext.', 'InvalidStateError'));
      this._setState('suspended'); return Promise.resolve();
    }
    close() {
      if (this._state === 'closed') return Promise.reject(domErr('Cannot close a closed AudioContext.', 'InvalidStateError'));
      this._setState('closed'); contexts.delete(this); return Promise.resolve();
    }
  }
  class OfflineAudioCompletionEvent extends Event {
    constructor(type, init) { super(type, init); this.renderedBuffer = init && init.renderedBuffer; }
  }
  class OfflineAudioContext extends BaseAudioContext {
    constructor(a, len, rate) {
      const o = typeof a === 'object' && a !== null ? a : { numberOfChannels: a, length: len, sampleRate: rate };
      const ch = o.numberOfChannels === undefined ? 1 : Number(o.numberOfChannels);
      const length = Number(o.length), r = Number(o.sampleRate);
      if (!(ch >= 1 && ch <= 32)) throw domErr(`Failed to construct 'OfflineAudioContext': The number of channels provided (${ch}) is outside the range [1, 32].`, 'NotSupportedError');
      if (!(length >= 1)) throw domErr(`Failed to construct 'OfflineAudioContext': The number of frames provided (${length}) is less than the minimum bound (1).`, 'NotSupportedError');
      if (!(r >= 3000 && r <= 768000)) throw domErr(`Failed to construct 'OfflineAudioContext': The sampleRate provided (${r}) is outside the range [3000, 768000].`, 'NotSupportedError');
      super(r, ch);
      hidden(this, '_len', Math.floor(length)); hidden(this, '_ch', ch); hidden(this, '_rendering', null); hidden(this, '_oncomplete', null);
    }
    get length() { return this._len; }
    get currentTime() { return this._elapsed; }
    get oncomplete() { return this._oncomplete; }
    set oncomplete(f) { this._oncomplete = typeof f === 'function' ? f : null; }
    startRendering() {
      if (this._rendering) return Promise.reject(domErr('cannot call startRendering more than once', 'InvalidStateError'));
      this._rendering = new Promise((resolve) => {
        this._state = 'running';
        setTimeout(() => {
          const out = new AudioBuffer({ numberOfChannels: this._ch, length: this._len, sampleRate: this._rate });
          renderOffline(this, out);
          this._elapsed = Math.ceil(this._len / 128) * 128 / this._rate; this._state = 'closed';
          this.dispatchEvent(new Event('statechange'));
          for (const s of [...this._sources]) if (s._start !== null && s._end() <= this._elapsed) s._finish();
          this.dispatchEvent(new OfflineAudioCompletionEvent('complete', { renderedBuffer: out }));
          resolve(out);
        }, 0);
      });
      return this._rendering;
    }
    resume() { return this.startRendering().then(() => undefined); }
    suspend() { return Promise.resolve(); }
  }
  // Offline rendering of buffer sources through gain nodes (other nodes pass
  // their input through unchanged).
  function renderOffline(ctx, out) {
    const dest = ctx._dest, len = out.length, rate = ctx._rate;
    const gainTo = (node, seen) => {
      if (node === dest) return 1;
      if (seen.has(node)) return 0;
      seen.add(node);
      let g = 0;
      for (const n of node._out) if (n instanceof AudioNode) g += gainTo(n, seen) * (n instanceof GainNode ? n.gain.value : 1);
      seen.delete(node);
      return g;
    };
    for (const src of ctx._sources) {
      if (src._start === null) continue;
      const g = gainTo(src, new Set());
      if (!g) continue;
      const from = Math.round(src._start * rate), to = Math.min(len, Math.round(Math.min(src._end(), len / rate) * rate));
      if (src instanceof AudioBufferSourceNode && src._buf) {
        const b = src._buf, step = (src.playbackRate.value * Math.pow(2, src.detune.value / 1200)) * b.sampleRate / rate;
        for (let c = 0; c < out.numberOfChannels; c++) {
          const o = out.getChannelData(c), inp = b.getChannelData(Math.min(c, b.numberOfChannels - 1));
          let pos = src._offset * b.sampleRate;
          const ls = src.loopStart * b.sampleRate, le = src.loopEnd > 0 ? src.loopEnd * b.sampleRate : b.length;
          for (let i = from; i < to; i++, pos += step) {
            if (src.loop && pos >= le) pos = ls + (pos - le) % Math.max(1, le - ls);
            const k = Math.floor(pos); if (k >= b.length) break;
            o[i] += inp[k] * g;
          }
        }
      } else if (src instanceof OscillatorNode || src instanceof ConstantSourceNode) {
        for (let c = 0; c < out.numberOfChannels; c++) {
          const o = out.getChannelData(c);
          for (let i = from; i < to; i++) {
            const t = i / rate;
            let v;
            if (src instanceof ConstantSourceNode) v = src.offset._at(t);
            else {
              const ph = ((t - src._start) * src.frequency._at(t) * Math.pow(2, src.detune._at(t) / 1200)) % 1;
              v = src._type === 'square' ? (ph < 0.5 ? 1 : -1) : src._type === 'sawtooth' ? 2 * ph - 1 : src._type === 'triangle' ? 1 - 4 * Math.abs(ph - 0.5) : Math.sin(2 * Math.PI * ph);
            }
            o[i] += v * g;
          }
        }
      }
    }
  }

  const exports = {
    AudioContext, OfflineAudioContext, BaseAudioContext, AudioNode, AudioParam, AudioBuffer, AudioDestinationNode, AudioListener, AudioScheduledSourceNode,
    AudioBufferSourceNode, OscillatorNode, ConstantSourceNode, GainNode, BiquadFilterNode, IIRFilterNode, DelayNode, DynamicsCompressorNode, StereoPannerNode,
    PannerNode, AnalyserNode, ConvolverNode, WaveShaperNode, ChannelSplitterNode, ChannelMergerNode, ScriptProcessorNode, AudioProcessingEvent,
    MediaElementAudioSourceNode, MediaStreamAudioSourceNode, MediaStreamAudioDestinationNode, AudioWorklet, AudioWorkletNode, PeriodicWave,
    OfflineAudioCompletionEvent,
  };
  for (const [k, v] of Object.entries(exports)) hidden(global, k, v);
  if (B.MediaError) { hidden(global, 'MediaError', B.MediaError); hidden(global, 'TimeRanges', B.TimeRanges); }
})(globalThis);
