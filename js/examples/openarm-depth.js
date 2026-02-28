// openarm-depth.js — Video/depth decoding (WebTransport, WebCodecs, MSE)

import { log } from "./openarm-log.js";

// ─── Browser/codec diagnostics (call from console: window.xoqDiag()) ──
export async function diagnoseCodecSupport() {
  const results = {};
  results.userAgent = navigator.userAgent;
  results.isSafari = /Safari/.test(navigator.userAgent) && !/Chrome/.test(navigator.userAgent);
  results.hasWebTransport = typeof WebTransport !== 'undefined';
  results.hasMediaSource = typeof MediaSource !== 'undefined';
  results.hasVideoDecoder = typeof VideoDecoder !== 'undefined';
  results.hasEncodedVideoChunk = typeof EncodedVideoChunk !== 'undefined';
  results.hasOffscreenCanvas = typeof OffscreenCanvas !== 'undefined';

  // MSE codec support
  if (results.hasMediaSource) {
    results.mse = {};
    for (const codec of ['av01.0.05M.08', 'av01.0.05M.10', 'avc1.640028', 'avc1.42E01E', 'hev1.1.6.L93.B0']) {
      const mime = `video/mp4; codecs="${codec}"`;
      results.mse[codec] = MediaSource.isTypeSupported(mime);
    }
  }

  // WebCodecs VideoDecoder codec support
  if (results.hasVideoDecoder && typeof VideoDecoder.isConfigSupported === 'function') {
    results.videoDecoder = {};
    for (const codec of ['av01.0.05M.08', 'av01.0.05M.10', 'avc1.640028', 'avc1.42E01E']) {
      try {
        const r = await VideoDecoder.isConfigSupported({ codec });
        results.videoDecoder[codec] = r.supported;
      } catch (e) {
        results.videoDecoder[codec] = `error: ${e.message}`;
      }
    }
  }

  console.table(results.mse);
  console.table(results.videoDecoder);
  console.log('Full diagnostics:', results);
  return results;
}
// Expose globally for console access
if (typeof window !== 'undefined') {
  window.xoqDiag = diagnoseCodecSupport;
  // Auto-capture first depth keyframe for debugging
  window._xoqCapturedFrame = null;
  window._xoqCapturedAv1C = null;
  // Debug: try decoding hex bytes with a given codec string
  // Usage: await xoqTestDecode('0a0b...', 'av01.0.05M.10')
  window.xoqTestDecode = async (hexStr, codec) => {
    const hex = hexStr.replace(/\s/g, '');
    const data = new Uint8Array(hex.length / 2);
    for (let i = 0; i < data.length; i++) data[i] = parseInt(hex.substr(i * 2, 2), 16);
    return _tryDecode(data, codec, `test(${data.length}b)`);
  };
  // Try all decode strategies on captured frame: await xoqRetry()
  window.xoqRetry = async () => {
    const frame = window._xoqCapturedFrame;
    const av1c = window._xoqCapturedAv1C;
    if (!frame) { console.log('No captured frame yet — connect first'); return 'no frame'; }
    const info = parseSeqHdrInfo(av1c ? av1c.slice(4) : null);
    const correctCodec = info ? info.codec : 'av01.0.05M.10';
    // Original codec from av1C header (may have wrong level)
    let origCodec = correctCodec;
    if (av1c && av1c.length >= 4) {
      const p = (av1c[1] >> 5) & 7, l = av1c[1] & 0x1F, t = (av1c[2] >> 7) & 1;
      const h = (av1c[2] >> 6) & 1, tb = (av1c[2] >> 5) & 1;
      const bd = tb ? 12 : (h ? 10 : 8);
      origCodec = `av01.${p}.${String(l).padStart(2,'0')}${t?'H':'M'}.${String(bd).padStart(2,'0')}`;
    }
    console.log(`Captured: ${frame.length}b, av1C: ${av1c?av1c.length+'b':'none'}, correct=${correctCodec}, orig=${origCodec}, dims=${info?info.width+'x'+info.height:'?'}`);
    const strategies = [];
    const frameOnly = stripAv1NonFrameObus(frame);
    const seqHdr = av1c && av1c.length > 4 ? av1c.slice(4) : null;
    const stripped = stripAv1TemporalDelimiters(frame);
    const origDesc = av1c ? av1c.buffer.slice(av1c.byteOffset, av1c.byteOffset + av1c.byteLength) : null;
    const fixedAv1c = av1c ? av1c.slice() : null;
    if (fixedAv1c && info) fixedAv1c[1] = (info.profile << 5) | (info.level & 0x1F);
    const fixedDesc = fixedAv1c ? fixedAv1c.buffer.slice(fixedAv1c.byteOffset, fixedAv1c.byteOffset + fixedAv1c.byteLength) : null;
    const dims = info ? { codedWidth: info.width, codedHeight: info.height } : {};
    let withSeqHdr = null;
    if (seqHdr) {
      withSeqHdr = new Uint8Array(seqHdr.length + frameOnly.length);
      withSeqHdr.set(seqHdr, 0); withSeqHdr.set(frameOnly, seqHdr.length);
    }
    // Try combinations: data × codec × description × hwAccel
    const datas = [['TD-stripped', stripped]];
    if (withSeqHdr) datas.push(['SeqHdr+frame', withSeqHdr]);
    datas.push(['raw', frame]);
    const codecs = [[correctCodec, 'correct']];
    if (origCodec !== correctCodec) codecs.push([origCodec, 'orig']);
    const descs = [['fixedDesc', fixedDesc]];
    if (origDesc) descs.push(['origDesc', origDesc]);
    descs.push(['noDesc', null]);
    const hwAccels = ['prefer-software', 'no-preference'];
    for (const [dLabel, data] of datas) {
      for (const [c, cLabel] of codecs) {
        for (const [descLabel, desc] of descs) {
          for (const hw of hwAccels) {
            const label = `${dLabel}, ${cLabel}, ${descLabel}, ${hw}`;
            const r = await _tryDecode(data, c, label, desc, dims, hw);
            if (r.startsWith('OK')) { console.log(`\n=== SUCCESS: ${label} ===`); return r; }
          }
        }
      }
    }
    console.log('\n=== ALL STRATEGIES FAILED ===');
    return 'all failed';
  };
}
async function _tryDecode(data, codec, label, desc, dims, hwAccel) {
  const cfg = { codec };
  if (desc) cfg.description = desc;
  if (dims && dims.codedWidth) { cfg.codedWidth = dims.codedWidth; cfg.codedHeight = dims.codedHeight; }
  if (hwAccel) cfg.hardwareAcceleration = hwAccel;
  console.log(`[${label}] data=${data.length}b`);
  return new Promise(resolve => {
    const timer = setTimeout(() => { try { d.close(); } catch {} const r = 'TIMEOUT'; console.log(`[${label}] ${r}`); resolve(r); }, 5000);
    const d = new VideoDecoder({
      output: (f) => { clearTimeout(timer); const r = `OK: ${f.displayWidth}x${f.displayHeight} fmt=${f.format}`; console.log(`[${label}] ${r}`); f.close(); try{d.close();}catch{} resolve(r); },
      error: (e) => { clearTimeout(timer); const r = `ERR: ${e.message}`; console.log(`[${label}] ${r}`); try{d.close();}catch{} resolve(r); },
    });
    try { d.configure(cfg); } catch (e) { clearTimeout(timer); const r = `CONFIGURE_ERR: ${e.message}`; console.log(`[${label}] ${r}`); resolve(r); return; }
    d.decode(new EncodedVideoChunk({ type: 'key', timestamp: 0, data }));
    d.flush().catch(() => {});
  });
}

// ─── Helpers ─────────────────────────────────────────
export function rsConcat(...arrs) {
  const len = arrs.reduce((s, a) => s + a.length, 0);
  const r = new Uint8Array(len); let o = 0;
  for (const a of arrs) { r.set(a, o); o += a.length; }
  return r;
}
export function rsEncodeVarInt(v) {
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
export function rsEncodeString(s) { const b = new TextEncoder().encode(s); return rsConcat(rsEncodeVarInt(b.length), b); }
export function rsSizePrefix(p) { return rsConcat(rsEncodeVarInt(p.length), p); }

export class RsBufReader {
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

export class RsStreamReader {
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

export function rsEncodeClientSetup() {
  return rsSizePrefix(rsConcat(
    rsEncodeVarInt(2), rsEncodeVarInt(0xff0dad02), rsEncodeVarInt(0xff0dad01),
    rsEncodeVarInt(0)
  ));
}
export function rsEncodeAnnouncePlease(pfx) { return rsSizePrefix(rsEncodeString(pfx)); }
export function rsEncodeSubscribe(id, bc, tk, pri) {
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

// ─── AV1 OBU filtering for WebCodecs ─────────────────
// WebCodecs spec: EncodedVideoChunk data MUST NOT contain Temporal Delimiter OBUs (type 2).
// NVENC includes them; Chrome is lenient, Safari rejects them.
const OBU_TEMPORAL_DELIMITER = 2;
const OBU_SEQUENCE_HEADER = 1;

function readLeb128(data, offset) {
  let value = 0, bytes = 0;
  for (let i = 0; i < 8 && offset + i < data.length; i++) {
    const b = data[offset + i];
    value |= (b & 0x7F) << (i * 7);
    bytes++;
    if ((b & 0x80) === 0) break;
  }
  return [value, bytes];
}

// Check if data contains a Sequence Header OBU (type 1)
function hasSeqHdrObu(data) {
  let offset = 0;
  while (offset < data.length) {
    const hdr = data[offset];
    const obuType = (hdr >> 3) & 0x0F;
    if (obuType === OBU_SEQUENCE_HEADER) return true;
    const hasExtension = (hdr >> 2) & 1;
    const hasSize = (hdr >> 1) & 1;
    let headerSize = 1 + (hasExtension ? 1 : 0);
    if (!hasSize) break;
    const [obuSize, lebBytes] = readLeb128(data, offset + headerSize);
    headerSize += lebBytes;
    offset += headerSize + obuSize;
  }
  return false;
}

// Parse Sequence Header OBU to extract codec info (more accurate than av1C which may have wrong level).
// Returns { codec, profile, level, tier, bitDepth, width, height } or null.
function parseSeqHdrInfo(seqHdrObu) {
  const hasExt = (seqHdrObu[0] >> 2) & 1;
  const hasSize = (seqHdrObu[0] >> 1) & 1;
  let off = 1 + (hasExt ? 1 : 0);
  if (hasSize) {
    for (let i = 0; off + i < seqHdrObu.length; i++) {
      if (!(seqHdrObu[off + i] & 0x80)) { off += i + 1; break; }
    }
  }
  let bp = off * 8;
  const r = (n) => { let v = 0; for (let i = 0; i < n; i++) { v = (v << 1) | ((seqHdrObu[bp >> 3] >> (7 - (bp & 7))) & 1); bp++; } return v; };
  try {
    const prof = r(3); r(1); const rsh = r(1);
    let lvl, tier = 0;
    if (rsh) { lvl = r(5); }
    else {
      const tip = r(1); if (tip) return null;
      const iddp = r(1); const opCnt = r(5);
      r(12); lvl = r(5); if (lvl > 7) tier = r(1);
      if (iddp && r(1)) r(4);
      for (let i = 1; i <= opCnt; i++) { r(12); const sl = r(5); if (sl > 7) r(1); if (iddp && r(1)) r(4); }
    }
    const fwb = r(4) + 1, fhb = r(4) + 1;
    const width = r(fwb) + 1, height = r(fhb) + 1;
    if (!rsh && r(1)) r(7);
    r(3); // superblock, filter_intra, intra_edge_filter
    if (!rsh) {
      r(4); const eoh = r(1); if (eoh) r(2);
      const scsct = r(1); const sfsct = scsct ? 2 : r(1);
      if (sfsct > 0) { if (!r(1)) r(1); }
      if (eoh) r(3);
    }
    r(3); // superres, cdef, restoration
    const hbd = r(1); let tb = 0;
    if (prof === 2 && hbd) tb = r(1);
    const bitDepth = tb ? 12 : (hbd ? 10 : 8);
    // Parse color_config to find chroma_sample_position
    let chromaSamplePos = -1, chromaSampleBitPos = -1;
    const mono = (prof !== 1) ? r(1) : 0;
    const cdp = r(1);
    let mc = 2;
    if (cdp) { r(8); r(8); mc = r(8); }
    if (!mono && mc !== 0) {
      r(1); // color_range
      let sx, sy;
      if (prof === 0) { sx = 1; sy = 1; }
      else if (prof === 1) { sx = 0; sy = 0; }
      else {
        if (bitDepth === 12) { sx = r(1); sy = sx ? r(1) : 0; }
        else { sx = 1; sy = 0; }
      }
      if (sx === 1 && sy === 1) {
        chromaSampleBitPos = bp;
        chromaSamplePos = r(2);
      }
    }
    const tierChar = tier ? 'H' : 'M';
    const codec = `av01.${prof}.${String(lvl).padStart(2, '0')}${tierChar}.${String(bitDepth).padStart(2, '0')}`;
    return { codec, profile: prof, level: lvl, tier, bitDepth, width, height, chromaSamplePos, chromaSampleBitPos };
  } catch { return null; }
}

// Strip non-frame OBUs (TD type 2, SeqHdr type 1), keeping only Frame/TileGroup/FrameHeader OBUs
export function stripAv1NonFrameObus(data) {
  const out = [];
  let offset = 0;
  while (offset < data.length) {
    const hdr = data[offset];
    const obuType = (hdr >> 3) & 0x0F;
    const hasExtension = (hdr >> 2) & 1;
    const hasSize = (hdr >> 1) & 1;
    let headerSize = 1 + (hasExtension ? 1 : 0);
    if (!hasSize) {
      // Keep frame-type OBUs (3=FrameHeader, 4=TileGroup, 6=Frame)
      if (obuType >= 3 && obuType <= 6) out.push(data.subarray(offset));
      break;
    }
    const [obuSize, lebBytes] = readLeb128(data, offset + headerSize);
    headerSize += lebBytes;
    const totalSize = headerSize + obuSize;
    const end = Math.min(offset + totalSize, data.length);
    if (obuType >= 3 && obuType <= 6) out.push(data.subarray(offset, end));
    offset = end;
  }
  if (out.length === 0) return new Uint8Array(0);
  let len = 0;
  for (const chunk of out) len += chunk.length;
  const result = new Uint8Array(len);
  let pos = 0;
  for (const chunk of out) { result.set(chunk, pos); pos += chunk.length; }
  return result;
}

export function stripAv1TemporalDelimiters(data) {
  const out = [];
  let offset = 0, stripped = false;
  while (offset < data.length) {
    const hdr = data[offset];
    const obuType = (hdr >> 3) & 0x0F;
    const hasExtension = (hdr >> 2) & 1;
    const hasSize = (hdr >> 1) & 1;
    let headerSize = 1 + (hasExtension ? 1 : 0);
    if (!hasSize) {
      // No size field — rest of data is this OBU
      if (obuType !== OBU_TEMPORAL_DELIMITER) out.push(data.subarray(offset));
      else stripped = true;
      break;
    }
    const [obuSize, lebBytes] = readLeb128(data, offset + headerSize);
    headerSize += lebBytes;
    const totalSize = headerSize + obuSize;
    const end = Math.min(offset + totalSize, data.length);
    if (obuType !== OBU_TEMPORAL_DELIMITER) {
      out.push(data.subarray(offset, end));
    } else {
      stripped = true;
    }
    offset = end;
  }
  if (!stripped) return data; // nothing to strip, return original
  let len = 0;
  for (const chunk of out) len += chunk.length;
  const result = new Uint8Array(len);
  let pos = 0;
  for (const chunk of out) { result.set(chunk, pos); pos += chunk.length; }
  return result;
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

// ─── DepthDecoder (WebCodecs with auto-fallback to MSE) ────────
const HAS_WEBCODECS = typeof VideoDecoder !== 'undefined';
const HAS_MSE = typeof MediaSource !== 'undefined';
export { HAS_WEBCODECS };

export class DepthDecoder {
  constructor() {
    this.decoder = null; this.configured = false; this.configuredCodec = null;
    this.latestY = null;
    this.is10bit = false; this.width = 0; this.height = 0;
    this.frameCount = 0; this.copyBuf = null;
    this.disabled = !HAS_WEBCODECS && !HAS_MSE;
    this._seqHdrObu = null; // Saved Sequence Header OBU for WebKit prepend
    // MSE fallback state
    this._useMse = false;
    this._msePlayer = null;
    this._mseVideo = null;
    this._mseCanvas = null;
    this._mseCtx = null;
    this._mseRafId = null;
    this._initSegment = null; // saved for MSE fallback replay
    if (this.disabled && !DepthDecoder._warned) {
      DepthDecoder._warned = true;
      log('Depth not available (no WebCodecs or MSE)', 'info');
    }
  }
  onData(data) {
    if (this.disabled) return;
    if (this._autoConfiguring) {
      // Buffer segments arriving during async auto-config (e.g. replay bulk-feeding)
      if (!this._pendingData) this._pendingData = [];
      this._pendingData.push(new Uint8Array(data));
      return;
    }
    const d = new Uint8Array(data);

    // MSE fallback path
    if (this._useMse) {
      if (this._msePlayer) this._msePlayer.onData(d);
      return;
    }

    const isInit = hasFtyp(d);
    if (isInit) {
      this._initSegment = d.slice(); // save for potential MSE fallback
      if (!this.configured) {
        const av1c = extractAv1C(d);
        const codec = detectCodec(d);
        if (av1c) this._savedAv1c = av1c;
        if (codec) this._savedCodec = codec;
        const moofOff = findBoxOffset(d, 'moof');
        const firstSeg = moofOff >= 0 ? d.subarray(moofOff) : null;
        if (av1c && codec && firstSeg) {
          this._startAutoConfig(codec, av1c, firstSeg);
          return;
        }
        // Init-only segment (no moof) — save and wait for first media segment
        return;
      }
      if (this.configured) {
        const moofOff = findBoxOffset(d, 'moof');
        if (moofOff >= 0) this.decodeSample(d.subarray(moofOff));
      }
    } else if (!this.configured && this._savedAv1c && this._savedCodec) {
      // First media segment after init-only — use it to trigger auto-config
      this._startAutoConfig(this._savedCodec, this._savedAv1c, d);
    } else if (this.configured) {
      this.decodeSample(d);
    }
  }
  _startAutoConfig(codec, av1c, firstSeg) {
    // Capture for xoqRetry() debug tool
    if (typeof window !== 'undefined') {
      const mdat = findMdatContent(firstSeg);
      if (mdat) window._xoqCapturedFrame = mdat.slice();
      window._xoqCapturedAv1C = av1c.slice();
    }
    this._autoConfiguring = true;
    this._autoConfig(codec, av1c, firstSeg).catch(e => {
      log(`Depth auto-config error: ${e.message}`, 'error');
      this._switchToMse(codec);
    }).finally(() => {
      this._autoConfiguring = false;
      // Flush any segments that arrived during auto-config
      if (this._pendingData) {
        const pending = this._pendingData;
        this._pendingData = null;
        for (const d of pending) this.onData(d);
      }
    });
  }
  // Test-decode a single keyframe with a given configuration. Returns [success, reason].
  _testDecode(cfg, chunkData) {
    return new Promise(resolve => {
      let resolved = false;
      const done = (v, reason) => { if (!resolved) { resolved = true; clearTimeout(timer); resolve([v, reason]); } };
      const timer = setTimeout(() => { try { dec.close(); } catch {} done(false, 'timeout'); }, 3000);
      let dec;
      try {
        dec = new VideoDecoder({
          output: (f) => { const fmt = f.format; f.close(); try { dec.close(); } catch {} done(true, `fmt=${fmt} ${f.displayWidth}x${f.displayHeight}`); },
          error: (e) => { try { dec.close(); } catch {} done(false, e.message); },
        });
        dec.configure(cfg);
        dec.decode(new EncodedVideoChunk({ type: 'key', timestamp: 0, data: chunkData }));
        dec.flush().catch(() => {});
      } catch (e) { done(false, `threw: ${e.message}`); }
    });
  }
  // Try multiple WebCodecs configurations, pick the first that works, then set up the main decoder.
  async _autoConfig(codec, av1c, firstSegData) {
    if (!HAS_WEBCODECS) { this._switchToMse(codec); return; }

    // Suppress Safari-specific unhandled rejection from VideoDecoder internals
    const suppress = (e) => {
      if (e.reason?.message?.includes('ReadableStreamDefaultController')) e.preventDefault();
    };
    if (typeof window !== 'undefined') window.addEventListener('unhandledrejection', suppress);

    try {
      await this._autoConfigInner(codec, av1c, firstSegData);
    } finally {
      if (typeof window !== 'undefined') window.removeEventListener('unhandledrejection', suppress);
    }
  }
  async _autoConfigInner(codec, av1c, firstSegData) {
    // Save SeqHdr OBU from av1C for prepending to chunks
    if (av1c.length > 4) this._seqHdrObu = av1c.slice(4);

    // Parse actual codec info from SeqHdr (av1C header may have wrong level)
    const info = this._seqHdrObu ? parseSeqHdrInfo(this._seqHdrObu) : null;
    const correctCodec = info ? info.codec : codec;
    if (info && info.codec !== codec) {
      log(`Depth: av1C says ${codec}, SeqHdr says ${info.codec}`, 'data', { toast: false });
    }

    // Build fixed av1C with correct level/tier/bitdepth
    const fixedAv1c = av1c.slice();
    if (info && fixedAv1c.length >= 4) {
      fixedAv1c[1] = (info.profile << 5) | (info.level & 0x1F);
      const hbd = info.bitDepth > 8 ? 1 : 0;
      const tb = info.bitDepth === 12 ? 1 : 0;
      fixedAv1c[2] = ((info.tier & 1) << 7) | ((hbd & 1) << 6) | ((tb & 1) << 5)
        | (fixedAv1c[2] & 0x1F);
    }
    const fixedDesc = fixedAv1c.buffer.slice(fixedAv1c.byteOffset, fixedAv1c.byteOffset + fixedAv1c.byteLength);

    // Prepare test data variants from first keyframe mdat
    const mdat = findMdatContent(firstSegData);
    if (!mdat || !mdat.length) { this._switchToMse(codec); return; }
    const frameObus = stripAv1NonFrameObus(mdat);
    const tdStripped = stripAv1TemporalDelimiters(mdat);

    // SeqHdr prepended + frame-only OBUs (what decodeSample produces)
    let dataWithSeqHdr;
    if (this._seqHdrObu) {
      dataWithSeqHdr = new Uint8Array(this._seqHdrObu.length + frameObus.length);
      dataWithSeqHdr.set(this._seqHdrObu, 0);
      dataWithSeqHdr.set(frameObus, this._seqHdrObu.length);
    } else {
      dataWithSeqHdr = tdStripped;
    }

    // Build trials — Chrome needs prefer-software + description
    const dims = info ? { codedWidth: info.width, codedHeight: info.height } : {};
    const trials = [];
    trials.push([{ codec: correctCodec, description: fixedDesc, hardwareAcceleration: 'prefer-software' }, dataWithSeqHdr, this._seqHdrObu, 'desc+seqhdr']);
    trials.push([{ codec: correctCodec, description: fixedDesc, hardwareAcceleration: 'prefer-software', ...dims }, dataWithSeqHdr, this._seqHdrObu, 'desc+seqhdr+dims']);
    trials.push([{ codec: correctCodec, hardwareAcceleration: 'no-preference', ...dims }, tdStripped, null, 'nodesc+inline']);
    trials.push([{ codec: correctCodec, description: fixedDesc, hardwareAcceleration: 'no-preference' }, dataWithSeqHdr, this._seqHdrObu, 'nopref+desc+seqhdr']);

    for (let i = 0; i < trials.length; i++) {
      const [cfg, data, seqHdr, dLabel] = trials[i];
      const label = `${cfg.codec} hw=${cfg.hardwareAcceleration} ${dLabel}`;
      const [ok, reason] = await this._testDecode(cfg, data);
      if (ok) {
        log(`Depth: config #${i} works (${label}) ${reason}`, 'success');
        // Set up the real decoder with this configuration
        if (this.decoder) try { this.decoder.close(); } catch {}
        this.decoder = new VideoDecoder({
          output: (frame) => this.processFrame(frame).catch(e => console.error('Depth frame error:', e)),
          error: (e) => {
            log(`Depth VideoDecoder error: ${e.message} — switching to MSE`, 'info');
            this._switchToMse(this.configuredCodec);
          },
        });
        this.decoder.configure(cfg);
        this.configured = true;
        this.configuredCodec = cfg.codec;
        // Store winning data variant so decodeSample uses the same format
        if (seqHdr) {
          this._seqHdrObu = seqHdr; // prepend this SeqHdr + frame-only
        } else if (dLabel.includes('frameonly')) {
          this._seqHdrObu = null; // don't prepend any SeqHdr
          this._useFrameOnly = true;
        } else if (dLabel.includes('raw')) {
          this._seqHdrObu = null;
          this._useRawMdat = true; // pass mdat as-is
        } else if (dLabel.includes('inline')) {
          this._seqHdrObu = null;
          this._useTdStrip = true; // just strip TDs
        }
        log(`Depth WebCodecs: ${cfg.codec} ${info ? info.width + 'x' + info.height : ''} (${dLabel})`, 'data', { toast: false });
        // Decode the first keyframe
        this.decodeSample(firstSegData);
        return;
      }
      log(`Depth: #${i} FAIL ${label} — ${reason}`, 'data', { toast: false });
    }

    log(`Depth: all ${trials.length} WebCodecs configs failed — using MSE fallback`, 'info');
    this._switchToMse(codec);
  }
  _switchToMse(codec) {
    // Clean up WebCodecs decoder
    if (this.decoder) try { this.decoder.close(); } catch {}
    this.decoder = null;
    this.configured = false;

    if (!HAS_MSE) { this.disabled = true; log('Depth: MSE not available either', 'error'); return; }

    this._useMse = true;
    this.configured = true;
    this.configuredCodec = codec;

    this._mseVideo = document.createElement('video');
    this._mseVideo.muted = true;
    this._mseVideo.playsInline = true;
    this._mseVideo.style.cssText = 'position:absolute;width:1px;height:1px;opacity:0;pointer-events:none;';
    document.body.appendChild(this._mseVideo);

    this._msePlayer = new MsePlayer(this._mseVideo, 'Depth MSE');
    // Replay the saved init segment so MSE gets the ftyp+moov+first keyframe
    if (this._initSegment) this._msePlayer.onData(this._initSegment);

    // Extract pixels — try VideoFrame from <video> first (may preserve native 10-bit),
    // fall back to canvas drawImage + getImageData (8-bit only).
    const scheduleNext = () => {
      if (!this._useMse) return;
      if ('requestVideoFrameCallback' in this._mseVideo) {
        this._mseVideo.requestVideoFrameCallback(() => extractFrame());
      } else {
        this._mseRafId = requestAnimationFrame(() => extractFrame());
      }
    };
    const extractFrame = async () => {
      if (!this._useMse || this._mseExtracting) { scheduleNext(); return; }
      const v = this._mseVideo;
      if (!v || !v.videoWidth || v.readyState < 2) { scheduleNext(); return; }

      // Try native VideoFrame for potential 10-bit access from VideoToolbox
      if (typeof VideoFrame !== 'undefined' && this._mseCanvasOnly !== true) {
        try {
          this._mseExtracting = true;
          const frame = new VideoFrame(v, { timestamp: v.currentTime * 1e6 });
          if (!this._mseFmtLogged) {
            const fmt = frame.format;
            const native10 = fmt && (fmt.includes('10') || fmt.includes('12'));
            log(`Depth MSE: VideoFrame fmt=${fmt} ${frame.displayWidth}x${frame.displayHeight}${native10 ? ' — true 10-bit!' : ''}`, 'data');
            this._mseFmtLogged = true;
          }
          await this.processFrame(frame); // handles all formats, closes frame
          this._mseExtracting = false;
          scheduleNext();
          return;
        } catch (e) {
          this._mseExtracting = false;
          this._mseCanvasOnly = true;
          log(`Depth MSE: VideoFrame from <video> failed (${e.message}), using canvas fallback`, 'data', { toast: false });
        }
      }

      // Canvas fallback (8-bit, BT.709 reversal for 10-bit sources)
      const w = v.videoWidth, h = v.videoHeight;
      if (!this._mseCanvas || this._mseCanvas.width !== w || this._mseCanvas.height !== h) {
        this._mseCanvas = document.createElement('canvas');
        this._mseCanvas.width = w; this._mseCanvas.height = h;
        this._mseCtx = this._mseCanvas.getContext('2d', { willReadFrequently: true });
      }
      this._mseCtx.drawImage(v, 0, 0, w, h);
      const rgba = this._mseCtx.getImageData(0, 0, w, h).data;
      const ySize = w * h;
      const source10bit = this.configuredCodec && this.configuredCodec.endsWith('.10');
      if (source10bit) {
        if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint16Array(ySize);
        for (let i = 0; i < ySize; i++) {
          const g = rgba[i * 4];
          this.latestY[i] = g < 2 ? 0 : Math.min(1023, Math.round(g * 3.435 + 64));
        }
        this.is10bit = true;
      } else {
        if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint8Array(ySize);
        for (let i = 0; i < ySize; i++) this.latestY[i] = rgba[i * 4];
        this.is10bit = false;
      }
      this.width = w; this.height = h;
      scheduleNext();
    };
    scheduleNext();
    log(`Depth: MSE fallback active`, 'success');
  }
  decodeSample(segData) {
    const mdat = findMdatContent(segData);
    if (!mdat || !mdat.length) return;
    if (!this.decoder || this.decoder.state !== 'configured') return;
    const ts = this.frameCount * (1000000 / 30);
    // Detect keyframe from raw mdat: NVENC keyframes contain a Sequence Header OBU (type 1)
    const isKey = hasSeqHdrObu(mdat);
    // Capture first keyframe for xoqRetry() debug tool
    if (isKey && typeof window !== 'undefined' && !window._xoqCapturedFrame) {
      window._xoqCapturedFrame = mdat.slice();
    }
    // Format chunk data to match what worked during auto-config
    let data;
    if (this._useRawMdat) {
      data = mdat; // pass as-is (TDs included)
    } else if (this._useFrameOnly) {
      data = stripAv1NonFrameObus(mdat); // frame OBUs only, no SeqHdr
    } else if (this._useTdStrip) {
      data = stripAv1TemporalDelimiters(mdat); // strip TDs, keep NVENC inline SeqHdr
    } else if (this._seqHdrObu) {
      // Default: strip all non-frame OBUs, prepend saved SeqHdr
      const frameObus = stripAv1NonFrameObus(mdat);
      data = new Uint8Array(this._seqHdrObu.length + frameObus.length);
      data.set(this._seqHdrObu, 0);
      data.set(frameObus, this._seqHdrObu.length);
    } else {
      data = stripAv1TemporalDelimiters(mdat);
    }
    // Diagnostic: log first 2 frames' OBU structure
    if (this.frameCount < 2) {
      const hex = Array.from(data.subarray(0, Math.min(32, data.length)), b => b.toString(16).padStart(2, '0')).join(' ');
      const obuTypes = [];
      let off = 0;
      while (off < data.length) {
        const h = data[off], t = (h >> 3) & 0xF, ext = (h >> 2) & 1, sz = (h >> 1) & 1;
        let hs = 1 + (ext ? 1 : 0);
        if (!sz) { obuTypes.push(`t${t}`); break; }
        const [v, lb] = readLeb128(data, off + hs); hs += lb;
        obuTypes.push(`t${t}(${v})`);
        off += hs + v;
      }
      log(`Depth chunk#${this.frameCount}: ${isKey ? 'KEY' : 'delta'} ${data.length}b OBUs=[${obuTypes.join(',')}] hex=${hex}`, 'data', { toast: false });
    }
    try {
      this.decoder.decode(new EncodedVideoChunk({ type: isKey ? 'key' : 'delta', timestamp: ts, data }));
    } catch (e) {
      log(`Depth decode() threw: ${e.message}`, 'error');
    }
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
          const nativeBits = fmt.includes('10') || fmt.includes('12');
          const source10bit = this.configuredCodec && this.configuredCodec.endsWith('.10');
          if (nativeBits) {
            // Native 10/12-bit planar (e.g. I010) — copy Y plane directly
            if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint16Array(ySize);
            const stride16 = yStride / 2;
            const src = new Uint16Array(this.copyBuf);
            if (stride16 === w) {
              this.latestY.set(new Uint16Array(this.copyBuf, yOff, ySize));
            } else {
              for (let r = 0; r < h; r++) this.latestY.set(src.subarray(yOff/2 + r*stride16, yOff/2 + r*stride16 + w), r*w);
            }
            is10bit = true;
          } else if (source10bit) {
            // 8-bit planar (e.g. I420) but source is 10-bit — upscale Y << 2
            if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint16Array(ySize);
            const src = new Uint8Array(this.copyBuf);
            for (let r = 0; r < h; r++) {
              for (let c = 0; c < w; c++) this.latestY[r * w + c] = src[yOff + r * yStride + c] << 2;
            }
            is10bit = true;
            if (!this._upscaleLogged) { log(`Depth: upscaling 8-bit ${fmt} → 10-bit (source is 10-bit AV1)`, 'info'); this._upscaleLogged = true; }
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
        // fmt=null means GPU-opaque frame; use canvas to extract pixels
        const source10bit = this.configuredCodec && this.configuredCodec.endsWith('.10');
        if (!this.fmtLogged) {
          log(`Depth frame: fmt=null, ${w}x${h}, canvas fallback${source10bit ? ' (10-bit via BT.709 reversal)' : ''}`, "info");
          this.fmtLogged = true;
        }
        let rgba;
        try {
          if (!this.offCanvas || this.offCanvas.width !== w || this.offCanvas.height !== h) {
            this.offCanvas = (typeof OffscreenCanvas !== 'undefined') ? new OffscreenCanvas(w, h) : document.createElement('canvas');
            if (!(this.offCanvas instanceof OffscreenCanvas)) { this.offCanvas.width = w; this.offCanvas.height = h; }
            this.offCtx = this.offCanvas.getContext('2d', { willReadFrequently: true });
          }
          const bmp = await createImageBitmap(frame);
          this.offCtx.drawImage(bmp, 0, 0);
          bmp.close();
          rgba = this.offCtx.getImageData(0, 0, w, h).data;
        } catch (e) {
          try {
            this.offCtx.drawImage(frame, 0, 0);
            rgba = this.offCtx.getImageData(0, 0, w, h).data;
          } catch (e2) {
            if (!this._canvasWarnLogged) { log(`Depth canvas fallback failed: ${e2.message}`, 'error'); this._canvasWarnLogged = true; }
            return;
          }
        }
        if (source10bit) {
          // BT.709 reversal: canvas gives display-space R (monochrome), convert back to 10-bit Y
          if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint16Array(ySize);
          for (let i = 0; i < ySize; i++) {
            const g = rgba[i * 4]; // R channel (monochrome → R=G=B)
            this.latestY[i] = g < 2 ? 0 : Math.min(1023, Math.round(g * 3.435 + 64));
          }
          is10bit = true;
        } else {
          if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint8Array(ySize);
          for (let i = 0; i < ySize; i++) this.latestY[i] = rgba[i * 4];
          is10bit = false;
        }
      }

      this.is10bit = is10bit; this.width = w; this.height = h;
    } finally { frame.close(); }
  }
  destroy() {
    if (this.decoder) try { this.decoder.close(); } catch {}
    this.decoder = null; this.configured = false; this.latestY = null;
    this._initSegment = null;
    if (this._mseRafId) cancelAnimationFrame(this._mseRafId);
    if (this._msePlayer) this._msePlayer.destroy();
    if (this._mseVideo && this._mseVideo.parentElement) this._mseVideo.remove();
    this._msePlayer = null; this._mseVideo = null; this._mseCanvas = null; this._mseCtx = null;
    this._useMse = false;
  }
}

// ─── MsePlayer (MSE video playback) ─────────────────
export class MsePlayer {
  constructor(videoEl, label, { liveMode = true } = {}) {
    this.video = videoEl; this.label = label;
    this.ms = null; this.sb = null; this.queue = []; this.ready = false;
    this.frames = 0; this.seekIv = null; this.liveMode = liveMode;
  }
  onData(data) {
    this.frames++;
    if (!this.ready) {
      if (hasFtyp(data)) {
        const codec = detectCodec(data);
        if (!codec) { log(`${this.label}: cannot detect codec`, "error"); return; }
        this.initMse(codec, data); return;
      }
      // Buffer data arriving before sourceopen (e.g. replay bulk-feeding segments)
      this.queue.push(data);
      return;
    }
    this.enqueue(data);
  }
  initMse(codec, initData) {
    const mime = `video/mp4; codecs="${codec}"`;
    log(`${this.label}: ${mime}`, "data");
    if (!MediaSource.isTypeSupported(mime)) {
      const isAv1 = codec.startsWith('av01');
      const isSafari = /Safari/.test(navigator.userAgent) && !/Chrome/.test(navigator.userAgent);
      if (isAv1 && isSafari) {
        log(`${this.label}: AV1 not supported in MSE on this Safari/device. AV1 requires M3+ chip or newer. Server needs H.264 encoding for Safari compatibility.`, "error");
      } else {
        log(`${this.label}: codec unsupported: ${codec}`, "error");
      }
      return;
    }
    this.ms = new MediaSource();
    this.video.src = URL.createObjectURL(this.ms);
    this.ms.addEventListener('sourceopen', () => {
      try {
        this.sb = this.ms.addSourceBuffer(mime);
        this.sb.mode = 'segments';
        this.sb.addEventListener('updateend', () => this.flush());
        this.ready = true;
        // Prepend init before any pre-buffered segments (from replay bulk-feeding)
        this.queue.unshift(initData);
        this.flush();
        if (this.liveMode) {
          this.video.play().catch(() => {});
          this.seekIv = setInterval(() => {
            if (this.video.buffered.length > 0) {
              const start = this.video.buffered.start(0);
              const end = this.video.buffered.end(this.video.buffered.length - 1);
              if (this.video.currentTime < start || end - this.video.currentTime > 0.15) {
                this.video.currentTime = Math.max(start, end - 0.03);
              }
              if (end - start > 4 && !this.sb.updating) try { this.sb.remove(start, end - 2); } catch {}
            }
          }, 100);
        }
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

// ─── DepthVideoExtractor (replay: extract Y plane from MSE <video>) ──
// Same interface as DepthDecoder (latestY, width, height, is10bit) so the
// render loop's updatePointCloudGeneric() works unchanged.
export class DepthVideoExtractor {
  constructor(videoEl, codec) {
    this.video = videoEl;
    this.latestY = null;
    this.width = 0;
    this.height = 0;
    this.is10bit = !!(codec && codec.endsWith('.10'));
    this._canvas = null;
    this._ctx = null;
    this._rafId = null;
    this._lastTime = -1;
    this._startLoop();
  }
  _startLoop() {
    const tick = () => {
      this._rafId = requestAnimationFrame(tick);
      this._extract();
    };
    this._rafId = requestAnimationFrame(tick);
  }
  _extract() {
    const v = this.video;
    if (!v || !v.videoWidth || v.readyState < 2) return;
    // Skip if video time hasn't changed (same frame)
    if (v.currentTime === this._lastTime) return;
    this._lastTime = v.currentTime;

    const w = v.videoWidth, h = v.videoHeight;
    if (!this._canvas || this._canvas.width !== w || this._canvas.height !== h) {
      this._canvas = document.createElement('canvas');
      this._canvas.width = w; this._canvas.height = h;
      this._ctx = this._canvas.getContext('2d', { willReadFrequently: true });
    }
    this._ctx.drawImage(v, 0, 0, w, h);
    const rgba = this._ctx.getImageData(0, 0, w, h).data;
    const ySize = w * h;
    if (this.is10bit) {
      if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint16Array(ySize);
      for (let i = 0; i < ySize; i++) {
        const g = rgba[i * 4];
        this.latestY[i] = g < 2 ? 0 : Math.min(1023, Math.round(g * 3.435 + 64));
      }
    } else {
      if (!this.latestY || this.latestY.length !== ySize) this.latestY = new Uint8Array(ySize);
      for (let i = 0; i < ySize; i++) this.latestY[i] = rgba[i * 4];
    }
    this.width = w;
    this.height = h;
  }
  // Force extraction even if currentTime hasn't changed (after seek)
  forceExtract() {
    this._lastTime = -1;
    this._extract();
  }
  destroy() {
    if (this._rafId) cancelAnimationFrame(this._rafId);
    this._rafId = null;
    this.video = null;
    this._canvas = null;
    this._ctx = null;
    this.latestY = null;
  }
}

// ─── Default intrinsics (approximate D435I at 1280x720, used until metadata arrives) ──
const DEFAULT_INTRINSICS = { fx: 920, fy: 920, ppx: 640, ppy: 360, width: 1280, height: 720 };

// ─── Point cloud update ─────────────────────────────
export function updatePointCloudGeneric(videoEl, decoder, cCtx, pArr, cArr, geom, intrinsics) {
  if (!videoEl.videoWidth) return;
  if (!decoder || !decoder.latestY) return;

  const dW = decoder.width, dH = decoder.height;
  const step = 1;
  const intr = intrinsics || DEFAULT_INTRINSICS;
  const fx = intr.fx * dW / intr.width, fy = intr.fy * dH / intr.height;
  const cx = intr.ppx * dW / intr.width, cy = intr.ppy * dH / intr.height;

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
