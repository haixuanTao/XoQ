// openarm-recorder.js — Browser-side recording of live MoQ streams as separate files per track
//
// Video/depth: raw fMP4 concatenation (.mp4)
// CAN: candump log format (.log)
// Metadata: raw binary with timestamps (.bin)
//   Format per entry: [u64_le timestamp_ms][u32_le data_len][data bytes]

import { log } from "./openarm-log.js";

// ─── candump log formatting ─────────────────────────────
const CANFD_FRAME_SIZE = 72;

function formatCandumpBatch(timestampMs, interfaceName, data) {
  const lines = [];
  const secs = Math.floor(timestampMs / 1000);
  const usecs = Math.floor((timestampMs % 1000) * 1000);
  const tsStr = `(${secs}.${String(usecs).padStart(6, '0')})`;
  let offset = 0;

  while (offset + CANFD_FRAME_SIZE <= data.length) {
    const view = new DataView(data.buffer, data.byteOffset + offset, CANFD_FRAME_SIZE);
    const rawId = view.getUint32(0, true);
    const len = Math.min(data[offset + 4], 64);
    const fdFlags = data[offset + 5];
    const eff = (rawId & 0x80000000) !== 0;
    const canId = rawId & 0x1FFFFFFF;

    const idStr = eff ? canId.toString(16).toUpperCase().padStart(8, '0')
                      : canId.toString(16).toUpperCase().padStart(3, '0');

    let hex = '';
    for (let i = 0; i < len; i++) hex += data[offset + 8 + i].toString(16).toUpperCase().padStart(2, '0');

    if (fdFlags !== 0 || len > 8) {
      lines.push(`${tsStr} ${interfaceName} ${idStr}##${fdFlags.toString(16).toUpperCase()}${hex}`);
    } else {
      lines.push(`${tsStr} ${interfaceName} ${idStr}#${hex}`);
    }
    offset += CANFD_FRAME_SIZE;
  }
  return lines.join('\n') + (lines.length ? '\n' : '');
}

// Detect fMP4 init segment (starts with 'ftyp' box)
function isFmp4Init(d) {
  return d.length >= 8 && d[4] === 0x66 && d[5] === 0x74 && d[6] === 0x79 && d[7] === 0x70;
}

// Read a big-endian u32 box size at offset
function boxSize(d, off) {
  return (d[off] << 24 | d[off+1] << 16 | d[off+2] << 8 | d[off+3]) >>> 0;
}
function boxType(d, off) {
  return String.fromCharCode(d[off+4], d[off+5], d[off+6], d[off+7]);
}

// Strip init (ftyp+moov) from front of a chunk that starts with ftyp.
// Returns the media portion (moof+mdat...) or null if no media found.
function stripInit(d) {
  let off = 0;
  while (off + 8 <= d.length) {
    const size = boxSize(d, off);
    if (size < 8 || off + size > d.length) break;
    const type = boxType(d, off);
    if (type === 'moof' || type === 'styp') {
      // Found the start of media — return everything from here
      return d.subarray(off);
    }
    off += size;
  }
  return null; // no media portion found
}

// ─── TrackBuffer ────────────────────────────────────────

class TrackBuffer {
  constructor(key, type) {
    this.key = key;       // e.g. "RS1_color", "can0"
    this.type = type;     // "fmp4" | "can" | "metadata"
    this.init = null;     // fmp4 only: latest init segment (ftyp+moov)
    this.chunks = [];     // fmp4: [{data}], can/metadata: [{timeMs, data}]
  }
}

// ─── RecordingController ────────────────────────────────
// Created at connect time. Always receives data via onData() so it can
// capture fMP4 init segments. Only stores media data when recording.

export class RecordingController {
  constructor() {
    this.recording = false;
    this.startTime = 0;
    this.tracks = new Map(); // key -> TrackBuffer
  }

  start() {
    this.recording = true;
    this.startTime = performance.now();
    // Clear media chunks but keep init segments
    for (const track of this.tracks.values()) {
      track.chunks = [];
    }
    log('Recording started', 'success');
  }

  stop() {
    this.recording = false;
    const elapsed = performance.now() - this.startTime;
    log(`Recording stopped (${(elapsed / 1000).toFixed(1)}s)`, 'success');

    const files = this._buildFiles();
    this._downloadAll(files);
    // Clear chunks after download
    for (const track of this.tracks.values()) {
      track.chunks = [];
    }
    return files;
  }

  // Called from MoQ data handlers — always, regardless of recording state.
  // Init segments are always captured; media data only when recording.
  onData(key, data, type) {
    let track = this.tracks.get(key);
    if (!track) {
      track = new TrackBuffer(key, type || 'fmp4');
      this.tracks.set(key, track);
    }

    if (track.type === 'fmp4') {
      if (isFmp4Init(data)) {
        track.init = new Uint8Array(data);
      }
      if (!this.recording) return;
      track.chunks.push({ data: new Uint8Array(data) });
    } else {
      if (!this.recording) return;
      const timeMs = performance.now() - this.startTime;
      track.chunks.push({ timeMs, data: new Uint8Array(data) });
    }
  }

  _buildFiles() {
    const now = new Date();
    const timeTag = String(now.getHours()).padStart(2, '0') +
                    String(now.getMinutes()).padStart(2, '0') +
                    String(now.getSeconds()).padStart(2, '0');

    const files = [];

    for (const [key, track] of this.tracks) {
      if (track.chunks.length === 0 && !track.init) continue;

      const sanitizedKey = key.replace(/[^a-zA-Z0-9_-]/g, '_');

      if (track.type === 'fmp4') {
        if (track.chunks.length === 0) continue;
        // Build: one init (ftyp+moov) + media segments (moof+mdat) only.
        // Each MoQ frame may contain init+media concatenated, so strip
        // duplicate inits — keep only the first, extract media from the rest.
        const parts = [];
        let initUsed = false;
        for (const chunk of track.chunks) {
          if (isFmp4Init(chunk.data)) {
            if (!initUsed) {
              // First chunk with init: keep it whole (init + media)
              parts.push(chunk.data);
              initUsed = true;
            } else {
              // Subsequent: strip init, keep only media (moof+mdat)
              const media = stripInit(chunk.data);
              if (media) parts.push(media);
            }
          } else {
            // Pure media segment (no init prefix)
            if (!initUsed && track.init) {
              // Prepend stored init before first media chunk
              parts.push(track.init);
              initUsed = true;
            }
            parts.push(chunk.data);
          }
        }
        if (parts.length === 0) continue;
        const totalSize = parts.reduce((a, d) => a + d.length, 0);
        const merged = new Uint8Array(totalSize);
        let off = 0;
        for (const d of parts) {
          merged.set(d, off);
          off += d.length;
        }
        files.push({
          blob: new Blob([merged], { type: 'video/mp4' }),
          filename: `rec_${timeTag}_${sanitizedKey}.mp4`,
        });
        log(`Built fmp4 track: ${key} (${track.chunks.length} segments, ${(totalSize/1024).toFixed(0)} KB)`, 'data');
      } else if (track.type === 'can') {
        const iface = key.replace(/^can_/, '') || 'can0';
        const parts = [];
        for (const chunk of track.chunks) {
          parts.push(formatCandumpBatch(chunk.timeMs, iface, chunk.data));
        }
        const text = parts.join('');
        files.push({
          blob: new Blob([text], { type: 'text/plain' }),
          filename: `rec_${timeTag}_${sanitizedKey}.log`,
        });
        log(`Built can track: ${key} (${track.chunks.length} batches)`, 'data');
      } else {
        let totalSize = 0;
        for (const chunk of track.chunks) {
          totalSize += 8 + 4 + chunk.data.length;
        }
        const merged = new Uint8Array(totalSize);
        const view = new DataView(merged.buffer);
        let off = 0;
        for (const chunk of track.chunks) {
          const tsMs = Math.round(chunk.timeMs);
          view.setUint32(off, tsMs >>> 0, true);
          view.setUint32(off + 4, (tsMs / 0x100000000) >>> 0, true);
          off += 8;
          view.setUint32(off, chunk.data.length, true);
          off += 4;
          merged.set(chunk.data, off);
          off += chunk.data.length;
        }
        files.push({
          blob: new Blob([merged], { type: 'application/octet-stream' }),
          filename: `rec_${timeTag}_${sanitizedKey}.bin`,
        });
        log(`Built metadata track: ${key} (${track.chunks.length} entries)`, 'data');
      }
    }

    return files;
  }

  _downloadAll(files) {
    for (const { blob, filename } of files) {
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = filename;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      setTimeout(() => URL.revokeObjectURL(url), 5000);
      log(`Downloaded: ${filename} (${(blob.size / 1024).toFixed(0)} KB)`, 'data');
    }
  }
}
