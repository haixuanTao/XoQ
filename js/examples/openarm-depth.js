// openarm-depth.js — Video/depth decoding (WebTransport, WebCodecs, MSE)

import { log } from "./openarm-log.js";

// ─── Helpers ─────────────────────────────────────────
function rsConcat(...arrs) {
  const len = arrs.reduce((s, a) => s + a.length, 0);
  const r = new Uint8Array(len); let o = 0;
  for (const a of arrs) { r.set(a, o); o += a.length; }
  return r;
}
function rsEncodeVarInt(v) {
  if (v < 0x40) return new Uint8Array([v]);
  if (v < 0x4000) return new Uint8Array([0x40 | (v >> 8), v & 0xff]);
  if (v < 0x40000000) return new Uint8Array([
    0x80 | ((v >>> 24) & 0x3f), (v >>> 16) & 0xff, (v >>> 8) & 0xff, v & 0xff
  ]);
  const hi = Math.floor(v / 0x100000000), lo = v >>> 0;
  return new Uint8Array([
    0xc0 | ((hi >>> 24) & 0x3f), (hi >>> 16) & 0xff, (hi >>> 8) & 0xff, hi & 0xff,
    (lo >>> 24) & 0xff, (lo >>> 16) & 0xff, (lo >>> 8) & 0xff, lo & 0xff
  ]);
}
function rsEncodeString(s) { const b = new TextEncoder().encode(s); return rsConcat(rsEncodeVarInt(b.length), b); }
function rsSizePrefix(p) { return rsConcat(rsEncodeVarInt(p.length), p); }

class RsBufReader {
  constructor(d) { this.d = d; this.p = 0; }
  readVarInt() {
    const f = this.d[this.p], tag = (f & 0xc0) >> 6, len = 1 << tag;
    let v = f & 0x3f;
    for (let i = 1; i < len; i++) v = v * 256 + this.d[this.p + i];
    this.p += len; return v;
  }
  readBytes(n) { const r = this.d.slice(this.p, this.p + n); this.p += n; return r; }
  readString() { const n = this.readVarInt(); return new TextDecoder().decode(this.readBytes(n)); }
}

class RsStreamReader {
  constructor(r) { this.reader = r; this.buf = new Uint8Array(0); this.pos = 0; }
  avail() { return this.buf.length - this.pos; }
  async ensure(n) {
    while (this.avail() < n) {
      const { value, done } = await this.reader.read();
      if (done) throw new Error('Stream ended');
      const v = new Uint8Array(value);
      const nb = new Uint8Array(this.avail() + v.length);
      nb.set(this.buf.subarray(this.pos)); nb.set(v, this.avail());
      this.buf = nb; this.pos = 0;
    }
  }
  async readVarInt() {
    await this.ensure(1);
    const f = this.buf[this.pos], tag = (f & 0xc0) >> 6, len = 1 << tag;
    await this.ensure(len);
    let v = f & 0x3f;
    for (let i = 1; i < len; i++) v = v * 256 + this.buf[this.pos + i];
    this.pos += len; return v;
  }
  async readBytes(n) { await this.ensure(n); const r = this.buf.slice(this.pos, this.pos + n); this.pos += n; return r; }
  async readMessage() { const s = await this.readVarInt(); return await this.readBytes(s); }
}

function rsEncodeClientSetup() {
  return rsSizePrefix(rsConcat(
    rsEncodeVarInt(2), rsEncodeVarInt(0xff0dad02), rsEncodeVarInt(0xff0dad01),
    rsEncodeVarInt(0)
  ));
}
function rsEncodeAnnouncePlease(pfx) { return rsSizePrefix(rsEncodeString(pfx)); }
function rsEncodeSubscribe(id, bc, tk, pri) {
  return rsSizePrefix(rsConcat(rsEncodeVarInt(id), rsEncodeString(bc), rsEncodeString(tk), new Uint8Array([(pri+128)&0xff])));
}

// ─── RsMoqSubscriber ─────────────────────────────────
export class RsMoqSubscriber {
  constructor() { this.transport = null; this.nextId = 0; this.handlers = new Map(); this.running = false; }
  async connect(url, certHash) {
    const opts = {};
    if (certHash) opts.serverCertificateHashes = [{ algorithm: 'sha-256', value: (() => {
      const hex = certHash.replace(/[^0-9a-fA-F]/g, '');
      const b = new Uint8Array(hex.length / 2);
      for (let i = 0; i < b.length; i++) b[i] = parseInt(hex.substr(i * 2, 2), 16);
      return b.buffer;
    })() }];
    this.transport = new WebTransport(url, opts);
    await this.transport.ready; this.running = true;
    log('[depth] WebTransport connected', 'success');
    this.transport.closed.then(() => { this.running = false; log('[depth] Transport closed'); })
      .catch(e => { this.running = false; log(`[depth] Transport: ${e.message}`, 'error'); });
    await this.setup();
    this.receiveStreams();
  }
  async setup() {
    const b = await this.transport.createBidirectionalStream();
    const w = b.writable.getWriter(), r = new RsStreamReader(b.readable.getReader());
    await w.write(rsConcat(rsEncodeVarInt(0), rsEncodeClientSetup()));
    const msg = await r.readMessage(); const br = new RsBufReader(msg);
    log(`[depth] Server version: 0x${br.readVarInt().toString(16)}`, 'data', { toast: false });
  }
  async waitBroadcast() {
    const b = await this.transport.createBidirectionalStream();
    const w = b.writable.getWriter(), r = new RsStreamReader(b.readable.getReader());
    await w.write(rsConcat(rsEncodeVarInt(1), rsEncodeAnnouncePlease("")));
    const msg = await r.readMessage(); const br = new RsBufReader(msg);
    const cnt = br.readVarInt(), paths = [];
    for (let i = 0; i < cnt; i++) paths.push(br.readString());
    log(`[depth] Broadcasts: [${paths.map(p => `"${p}"`).join(', ')}]`, 'data', { toast: false });
    if (cnt > 0) return paths[0];
    log('[depth] Waiting for publisher...', 'info');
    while (true) {
      const m = await r.readMessage();
      if (m[0] === 1) { const pbr = new RsBufReader(m.subarray(1)); return pbr.readString(); }
    }
  }
  async subscribe(bc, tk, onData) {
    const id = this.nextId++; this.handlers.set(id, onData);
    const b = await this.transport.createBidirectionalStream();
    const w = b.writable.getWriter(), r = new RsStreamReader(b.readable.getReader());
    await w.write(rsConcat(rsEncodeVarInt(2), rsEncodeSubscribe(id, bc, tk, 0)));
    await r.readMessage();
    log(`[depth] Subscribed "${tk}" (id=${id})`, 'success', { toast: false }); return id;
  }
  async receiveStreams() {
    const sr = this.transport.incomingUnidirectionalStreams.getReader();
    while (this.running) {
      try {
        const { value, done } = await sr.read(); if (done) break;
        this.handleData(value);
      } catch (e) { if (this.running) log(`[depth] Recv: ${e.message}`, 'error'); break; }
    }
  }
  async handleData(stream) {
    try {
      const r = new RsStreamReader(stream.getReader());
      if (await r.readVarInt() !== 0) return;
      const hdr = await r.readMessage(); const br = new RsBufReader(hdr);
      const subId = br.readVarInt(); br.readVarInt();
      const frame = await r.readMessage();
      const h = this.handlers.get(subId); if (h) h(frame);
    } catch {}
  }
  disconnect() { this.running = false; if (this.transport) { this.transport.close(); this.transport = null; } }
}

// ─── MP4 Helpers ─────────────────────────────────────
export function detectCodec(data) {
  const d = new Uint8Array(data), h = n => n.toString(16).padStart(2, '0').toUpperCase();
  for (let i = 0; i < d.length - 11; i++) {
    if (d[i+4]===0x61 && d[i+5]===0x76 && d[i+6]===0x63 && d[i+7]===0x43) {
      const o = i + 8; if (o + 4 <= d.length) return `avc1.${h(d[o+1])}${h(d[o+2])}${h(d[o+3])}`;
    }
    if (d[i+4]===0x61 && d[i+5]===0x76 && d[i+6]===0x31 && d[i+7]===0x43) {
      const o = i + 8; if (o + 4 <= d.length) {
        const profile = (d[o+1] >> 5) & 0x7;
        const level = d[o+1] & 0x1F;
        const tier = (d[o+2] >> 7) & 1;
        const highBitDepth = (d[o+2] >> 6) & 1;
        const twelveBit = (d[o+2] >> 5) & 1;
        const bitDepth = highBitDepth ? (twelveBit ? 12 : 10) : 8;
        const levelStr = String(level).padStart(2, '0');
        const tierChar = tier ? 'H' : 'M';
        return `av01.${profile}.${levelStr}${tierChar}.${String(bitDepth).padStart(2, '0')}`;
      }
    }
  }
  return null;
}

export function hasFtyp(d) { return d.length >= 8 && d[4]===0x66 && d[5]===0x74 && d[6]===0x79 && d[7]===0x70; }

export function findBoxOffset(data, type) {
  const t = [type.charCodeAt(0), type.charCodeAt(1), type.charCodeAt(2), type.charCodeAt(3)];
  let i = 0;
  while (i < data.length - 8) {
    const sz = (data[i]<<24)|(data[i+1]<<16)|(data[i+2]<<8)|data[i+3];
    if (data[i+4]===t[0] && data[i+5]===t[1] && data[i+6]===t[2] && data[i+7]===t[3]) return i;
    if (sz < 8) break;
    i += sz;
  }
  return -1;
}

export function findMdatContent(data) {
  const i = findBoxOffset(data, 'mdat');
  if (i < 0) return null;
  const sz = (data[i]<<24)|(data[i+1]<<16)|(data[i+2]<<8)|data[i+3];
  return data.subarray(i + 8, i + sz);
}

export function extractAv1C(data) {
  for (let i = 0; i < data.length - 11; i++) {
    if (data[i+4]===0x61 && data[i+5]===0x76 && data[i+6]===0x31 && data[i+7]===0x43) {
      const sz = (data[i]<<24)|(data[i+1]<<16)|(data[i+2]<<8)|data[i+3];
      return data.slice(i + 8, i + sz);
    }
  }
  return null;
}

// Strip 8-byte timestamp prefix, update latency stats object
export function stripTimestamp(bytes, latencyStats) {
  if (bytes.length < 8) return bytes;
  const lo = bytes[0]|(bytes[1]<<8)|(bytes[2]<<16)|(bytes[3]<<24);
  const hi = bytes[4]|(bytes[5]<<8)|(bytes[6]<<16)|(bytes[7]<<24);
  const ms = (hi >>> 0) * 4294967296 + (lo >>> 0);
  if (ms > 1700000000000 && ms < Date.now() + 60000) {
    latencyStats.ms = Date.now() - ms;
    latencyStats.lastUpdate = Date.now();
    latencyStats.sum += latencyStats.ms;
    latencyStats.samples++;
  }
  return bytes.subarray(8);
}

// ─── DepthDecoder (WebCodecs AV1 monochrome) ────────
const HAS_WEBCODECS = typeof VideoDecoder !== 'undefined';
export { HAS_WEBCODECS };

export class DepthDecoder {
  constructor() {
    this.decoder = null; this.configured = false; this.configuredCodec = null;
    this.latestY = null;
    this.is10bit = false; this.width = 0; this.height = 0;
    this.frameCount = 0; this.copyBuf = null;
    this.disabled = !HAS_WEBCODECS;
    if (this.disabled && !DepthDecoder._warned) {
      DepthDecoder._warned = true;
      log('Depth not available (WebCodecs not supported in this browser)', 'info');
    }
  }
  onData(data) {
    if (this.disabled) return;
    const d = new Uint8Array(data);
    const isInit = hasFtyp(d);
    if (isInit) {
      if (!this.configured) {
        const av1c = extractAv1C(d);
        const codec = detectCodec(d);
        if (av1c && codec) this.configure(codec, av1c);
      }
      const moofOff = findBoxOffset(d, 'moof');
      if (moofOff >= 0) this.decodeSample(d.subarray(moofOff), true);
    } else if (this.configured) {
      this.decodeSample(d, false);
    }
  }
  configure(codec, av1c) {
    if (this.configuredCodec === codec) return;
    if (this.decoder) try { this.decoder.close(); } catch {}
    this.decoder = new VideoDecoder({
      output: (frame) => this.processFrame(frame).catch(e => console.error('Depth frame error:', e)),
      error: (e) => console.error('Depth decoder error:', e),
    });
    const desc = av1c.buffer.slice(av1c.byteOffset, av1c.byteOffset + av1c.byteLength);
    this.decoder.configure({ codec, description: desc, hardwareAcceleration: 'prefer-software' });
    this.configured = true;
    this.configuredCodec = codec;
    log(`Depth WebCodecs: ${codec}`, "data", { toast: false });
  }
  decodeSample(segData, isKey) {
    const mdat = findMdatContent(segData);
    if (!mdat) return;
    const ts = this.frameCount * (1000000 / 30);
    this.decoder.decode(new EncodedVideoChunk({ type: isKey ? 'key' : 'delta', timestamp: ts, data: mdat }));
    this.frameCount++;
  }
  async processFrame(frame) {
    try {
      const w = frame.displayWidth, h = frame.displayHeight;
      const ySize = w * h;
      const fmt = frame.format;

      let is10bit = false;

      if (fmt) {
        const totalSize = frame.allocationSize();
        if (!this.copyBuf || this.copyBuf.byteLength < totalSize) this.copyBuf = new ArrayBuffer(totalSize);
        const layouts = await frame.copyTo(this.copyBuf);
        const yOff = layouts[0].offset, yStride = layouts[0].stride;
        this._rawDiag = { fmt, w, h, totalSize, numLayouts: layouts.length,
          layouts: layouts.map(l => ({offset: l.offset, stride: l.stride})),
          codedWidth: frame.codedWidth, codedHeight: frame.codedHeight,
          displayWidth: frame.displayWidth, displayHeight: frame.displayHeight };

        const isPackedRGB = fmt.includes('BGR') || fmt.includes('RGB');

        if (!this.fmtLogged) {
          const pxPerRow = isPackedRGB ? Math.floor(yStride / 4) : yStride;
          log(`Depth frame: fmt=${fmt}, ${w}x${h}, stride=${yStride}, pxPerRow=${pxPerRow}, packed=${isPackedRGB}`, "data");
          this.fmtLogged = true;
        }

        if (isPackedRGB) {
          const src = new Uint8Array(this.copyBuf);
          const bpp = 4;
          if (!this._halfWidthDetected) {
            const lastRowOff = yOff + (h - 1) * yStride;
            let zeroCount = 0, checkCount = 0;
            for (let c = Math.floor(w / 2); c < w; c += 10) {
              if (src[lastRowOff + c * bpp + 1] === 0) zeroCount++;
              checkCount++;
            }
            this._isHalfWidth = (checkCount > 0 && zeroCount / checkCount > 0.8);
            this._halfWidthDetected = true;
            if (this._isHalfWidth) log(`Depth BGRX: half-width bug detected, scaling ${w/2} → ${w}`, "info");
          }
          const srcW = this._isHalfWidth ? Math.floor(w / 2) : w;
          const source10bit = this.configuredCodec && this.configuredCodec.endsWith('.10');
          if (source10bit) {
            if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint16Array(ySize);
            for (let row = 0; row < h; row++) {
              const rowOff = yOff + row * yStride;
              for (let col = 0; col < w; col++) {
                const srcCol = srcW < w ? Math.floor(col * srcW / w) : col;
                const g = src[rowOff + srcCol * bpp + 1];
                this.latestY[row * w + col] = g < 2 ? 0 : Math.min(1023, Math.round(g * 3.435 + 64));
              }
            }
            is10bit = true;
          } else {
            if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint8Array(ySize);
            for (let row = 0; row < h; row++) {
              const rowOff = yOff + row * yStride;
              for (let col = 0; col < w; col++) {
                const srcCol = srcW < w ? Math.floor(col * srcW / w) : col;
                const g = src[rowOff + srcCol * bpp + 1];
                this.latestY[row * w + col] = g < 2 ? 0 : Math.min(255, Math.round(g / 1.164 + 16));
              }
            }
            is10bit = false;
          }
        } else {
          is10bit = fmt.includes('10') || fmt.includes('12');
          if (is10bit) {
            if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint16Array(ySize);
            const stride16 = yStride / 2;
            const src = new Uint16Array(this.copyBuf);
            if (stride16 === w) {
              this.latestY.set(new Uint16Array(this.copyBuf, yOff, ySize));
            } else {
              for (let r = 0; r < h; r++) this.latestY.set(src.subarray(yOff/2 + r*stride16, yOff/2 + r*stride16 + w), r*w);
            }
          } else {
            if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint8Array(ySize);
            const src = new Uint8Array(this.copyBuf);
            if (yStride === w) {
              this.latestY.set(new Uint8Array(this.copyBuf, yOff, ySize));
            } else {
              for (let r = 0; r < h; r++) this.latestY.set(src.subarray(yOff + r*yStride, yOff + r*yStride + w), r*w);
            }
          }
        }
      } else {
        if (!this.fmtLogged) {
          log(`Depth frame: fmt=null, ${w}x${h}, canvas fallback`, "info");
          this.fmtLogged = true;
        }
        if (!this.offCanvas || this.offCanvas.width !== w || this.offCanvas.height !== h) {
          this.offCanvas = new OffscreenCanvas(w, h);
          this.offCtx = this.offCanvas.getContext('2d', { willReadFrequently: true });
        }
        this.offCtx.drawImage(frame, 0, 0);
        const rgba = this.offCtx.getImageData(0, 0, w, h).data;
        if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint8Array(ySize);
        for (let i = 0; i < ySize; i++) this.latestY[i] = rgba[i * 4];
        is10bit = false;
      }

      this.is10bit = is10bit; this.width = w; this.height = h;
    } finally { frame.close(); }
  }
  destroy() {
    if (this.decoder) try { this.decoder.close(); } catch {}
    this.decoder = null; this.configured = false; this.latestY = null;
  }
}

// ─── MsePlayer (MSE video playback) ─────────────────
export class MsePlayer {
  constructor(videoEl, label) {
    this.video = videoEl; this.label = label;
    this.ms = null; this.sb = null; this.queue = []; this.ready = false;
    this.frames = 0; this.seekIv = null;
  }
  onData(data) {
    this.frames++;
    if (!this.ready) {
      if (!hasFtyp(data)) return;
      const codec = detectCodec(data);
      if (!codec) { log(`${this.label}: cannot detect codec`, "error"); return; }
      this.initMse(codec, data); return;
    }
    this.enqueue(data);
  }
  initMse(codec, initData) {
    const mime = `video/mp4; codecs="${codec}"`;
    log(`${this.label}: ${mime}`, "data");
    if (!MediaSource.isTypeSupported(mime)) { log(`${this.label}: unsupported`, "error"); return; }
    this.ms = new MediaSource();
    this.video.src = URL.createObjectURL(this.ms);
    this.ms.addEventListener('sourceopen', () => {
      try {
        this.sb = this.ms.addSourceBuffer(mime);
        this.sb.mode = 'segments';
        this.sb.addEventListener('updateend', () => this.flush());
        this.ready = true;
        this.enqueue(initData);
        this.video.play().catch(() => {});
        this.seekIv = setInterval(() => {
          if (this.video.buffered.length > 0) {
            const start = this.video.buffered.start(0);
            const end = this.video.buffered.end(this.video.buffered.length - 1);
            if (this.video.currentTime < start || end - this.video.currentTime > 0.5) {
              this.video.currentTime = Math.max(start, end - 0.05);
            }
            if (end - start > 10 && !this.sb.updating) try { this.sb.remove(start, end - 5); } catch {}
          }
        }, 500);
      } catch (e) { log(`${this.label}: init failed: ${e.message}`, "error"); }
    });
  }
  enqueue(d) { this.queue.push(d); this.flush(); }
  flush() {
    if (!this.sb || this.sb.updating || !this.queue.length) return;
    try { this.sb.appendBuffer(this.queue.shift()); } catch (e) { log(`${this.label}: ${e.message}`, "error"); }
  }
  destroy() {
    if (this.seekIv) clearInterval(this.seekIv);
    this.queue = []; this.ready = false;
    if (this.ms && this.ms.readyState === 'open') try { this.ms.endOfStream(); } catch {}
    this.video.src = '';
  }
}

// ─── Point cloud update ─────────────────────────────
export function updatePointCloudGeneric(videoEl, decoder, cCtx, pArr, cArr, geom) {
  if (!videoEl.videoWidth) return;
  if (!decoder || !decoder.latestY) return;

  const dW = decoder.width, dH = decoder.height;
  const step = 1;
  const fx = 604.2 * dW / 640, fy = 603.5 * dH / 480;
  const cx = 322.7 * dW / 640, cy = 252.7 * dH / 480;

  if (cCtx.canvas.width !== dW || cCtx.canvas.height !== dH) {
    cCtx.canvas.width = dW; cCtx.canvas.height = dH;
  }
  cCtx.drawImage(videoEl, 0, 0, dW, dH);
  const dY = decoder.latestY;
  const cPx = cCtx.getImageData(0, 0, dW, dH).data;

  let n = 0;
  for (let v = 0; v < dH; v += step) {
    for (let u = 0; u < dW; u += step) {
      const dIdx = v * dW + u;
      const gray = dY[dIdx];
      if (gray < 2) continue;
      const depth = decoder.is10bit ? gray : (gray << 4);
      const i3 = n * 3;
      pArr[i3]     = -(u - cx) * depth / fx;
      pArr[i3 + 1] = -(v - cy) * depth / fy;
      pArr[i3 + 2] = depth;
      const cIdx = (v * dW + u) * 4;
      cArr[i3]     = cPx[cIdx] / 255;
      cArr[i3 + 1] = cPx[cIdx + 1] / 255;
      cArr[i3 + 2] = cPx[cIdx + 2] / 255;
      n++;
    }
  }
  geom.attributes.position.needsUpdate = true;
  geom.attributes.color.needsUpdate = true;
  geom.setDrawRange(0, n);
  geom.computeBoundingSphere();
}
