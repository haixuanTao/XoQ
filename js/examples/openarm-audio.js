// openarm-audio.js — Audio MoQ subscription, PCM decode, WebAudio playback, level meter

import * as Moq from "@moq/lite";
import { log } from "./openarm-log.js";
import { buildConnectOpts, withTimeout, STALE_MS, RECONNECT_DELAY } from "./openarm-moq.js";

export function createAudioState() {
  return {
    conn: null,
    audioCtx: null,
    nextPlayTime: 0,
    running: false,
    enabled: false,
    framesReceived: 0,
  };
}

// 20-byte header: [u32 sample_rate][u16 channels][u16 sample_format][u32 frame_count][u32 timestamp_us][u32 data_length]
function decodeAudioMoqHeader(buf) {
  if (buf.byteLength < 20) return null;
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  return {
    sampleRate:   view.getUint32(0, true),
    channels:     view.getUint16(4, true),
    sampleFormat: view.getUint16(6, true),  // 0=I16, 1=F32
    frameCount:   view.getUint32(8, true),
    timestampUs:  view.getUint32(12, true),
    dataLength:   view.getUint32(16, true),
  };
}

function pcmToFloat32(pcmBytes, sampleFormat, frameCount, channels) {
  const totalSamples = frameCount * channels;
  if (sampleFormat === 1) {
    return new Float32Array(pcmBytes.buffer, pcmBytes.byteOffset, totalSamples);
  }
  const i16 = new Int16Array(pcmBytes.buffer, pcmBytes.byteOffset, totalSamples);
  const f32 = new Float32Array(totalSamples);
  for (let i = 0; i < totalSamples; i++) f32[i] = i16[i] / 32768;
  return f32;
}

function playAudioChunk(audioState, header, pcmData) {
  const ctx = audioState.audioCtx;
  if (!ctx) return;

  const f32 = pcmToFloat32(pcmData, header.sampleFormat, header.frameCount, header.channels);
  const audioBuf = ctx.createBuffer(header.channels, header.frameCount, header.sampleRate);

  if (header.channels === 1) {
    audioBuf.copyToChannel(f32, 0);
  } else {
    for (let ch = 0; ch < header.channels; ch++) {
      const chData = new Float32Array(header.frameCount);
      for (let i = 0; i < header.frameCount; i++) chData[i] = f32[i * header.channels + ch];
      audioBuf.copyToChannel(chData, ch);
    }
  }

  const src = ctx.createBufferSource();
  src.buffer = audioBuf;
  src.connect(ctx.destination);

  if (audioState.nextPlayTime < ctx.currentTime) {
    audioState.nextPlayTime = ctx.currentTime;
  }
  src.start(audioState.nextPlayTime);
  audioState.nextPlayTime += audioBuf.duration;
}

async function connectAudioOnce(config, audioState) {
  const relay = config.general.relay;
  const audioPath = (config.audio.path || "").trim();
  if (!audioPath) return;

  const fullUrl = `${relay}/${audioPath}`;
  const connectOpts = buildConnectOpts(config);
  log(`[audio] Connecting to ${fullUrl}...`, 'info', { toast: false });

  audioState.conn = await Moq.Connection.connect(new URL(fullUrl), connectOpts);
  log(`[audio] Connected`, 'success');

  const broadcast = audioState.conn.consume(Moq.Path.from(""));
  const track = broadcast.subscribe("mic", 0);
  log(`[audio] Subscribed to "mic" track`, 'success', { toast: false });

  while (audioState.running) {
    const group = await withTimeout(track.nextGroup(), STALE_MS);
    if (!group) { log(`[audio] Track ended`); break; }
    while (audioState.running) {
      const frame = await withTimeout(group.readFrame(), STALE_MS);
      if (!frame) break;
      const bytes = new Uint8Array(frame);
      const header = decodeAudioMoqHeader(bytes);
      if (!header || bytes.byteLength < 20 + header.dataLength) continue;

      if (!audioState.audioCtx) {
        audioState.audioCtx = new AudioContext({ sampleRate: header.sampleRate });
        audioState.nextPlayTime = 0;
        log(`[audio] AudioContext created: ${header.sampleRate}Hz, ${header.channels}ch`, 'data', { toast: false });
      }
      if (audioState.audioCtx.state === 'suspended') {
        await audioState.audioCtx.resume();
      }

      const pcmData = bytes.subarray(20, 20 + header.dataLength);
      playAudioChunk(audioState, header, pcmData);
      audioState.framesReceived++;

      // Update audio level indicator
      const f32 = pcmToFloat32(pcmData, header.sampleFormat, header.frameCount, header.channels);
      let sum = 0, peak = 0;
      for (let s = 0; s < f32.length; s++) {
        sum += f32[s] * f32[s];
        const a = Math.abs(f32[s]);
        if (a > peak) peak = a;
      }
      const rms = Math.sqrt(sum / f32.length);
      const rmsDb = rms > 0 ? Math.max(-60, 20 * Math.log10(rms)) : -60;
      const barLen = Math.round(((rmsDb + 60) / 60) * 6);
      const bar = '\u2588'.repeat(Math.max(0, barLen)) + '\u2591'.repeat(6 - Math.max(0, barLen));
      const levelEl = document.getElementById('audioLevel');
      if (levelEl) {
        levelEl.textContent = bar;
        levelEl.style.color = rmsDb > -10 ? '#f44' : rmsDb > -30 ? '#4f4' : '#555';
      }
    }
  }
}

function disconnectAudioConn(audioState) {
  if (audioState.conn) { try { audioState.conn.close(); } catch {} audioState.conn = null; }
}

export async function connectAudio(config, audioState) {
  audioState.running = true;
  let lastError = null;
  while (audioState.running) {
    try {
      await connectAudioOnce(config, audioState);
      if (!audioState.running) break;
      lastError = null;
      log(`[audio] Stream ended`, 'info');
    } catch (e) {
      if (!audioState.running) break;
      log(`[audio] ${e.message}`, 'error');
      lastError = e;
    }
    disconnectAudioConn(audioState);
    if (!audioState.running) break;
    await new Promise(r => setTimeout(r, RECONNECT_DELAY));
  }
}

export function disconnectAudio(audioState) {
  audioState.running = false;
  disconnectAudioConn(audioState);
  if (audioState.audioCtx) { try { audioState.audioCtx.close(); } catch {} audioState.audioCtx = null; }
  audioState.nextPlayTime = 0;
  audioState.framesReceived = 0;
  const levelEl = document.getElementById('audioLevel');
  if (levelEl) { levelEl.textContent = '--'; levelEl.style.color = '#555'; }
}
