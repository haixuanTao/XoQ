// openarm-replay.js — fMP4 demuxer + replay controller for recorded multi-track fMP4 files

import { parseAllCanFrames, parseDamiaoState, canIdToJointIdx } from "./openarm-can.js";
import { MsePlayer, DepthDecoder, DepthVideoExtractor, detectCodec } from "./openarm-depth.js";
import { log } from "./openarm-log.js";

// ─── fMP4 box parsing ──────────────────────────────────

function readU32(d, off) { return (d[off] << 24 | d[off+1] << 16 | d[off+2] << 8 | d[off+3]) >>> 0; }
function readU16(d, off) { return (d[off] << 8 | d[off+1]) >>> 0; }
function readU64(d, off) { return readU32(d, off) * 0x100000000 + readU32(d, off + 4); }
function boxType(d, off) { return String.fromCharCode(d[off], d[off+1], d[off+2], d[off+3]); }

function* iterBoxes(data, start, end) {
  let pos = start;
  while (pos + 8 <= end) {
    let size = readU32(data, pos);
    const type = boxType(data, pos + 4);
    if (size === 0) { size = end - pos; }
    else if (size === 1 && pos + 16 <= end) { size = readU64(data, pos + 8); }
    if (size < 8 || pos + size > end) break;
    yield { type, offset: pos, size };
    pos += size;
  }
}

function findBox(data, type, start, end) {
  for (const box of iterBoxes(data, start, end)) {
    if (box.type === type) return box;
  }
  return null;
}

function findBoxContent(data, type, start, end) {
  const box = findBox(data, type, start, end);
  if (!box) return null;
  const hdrLen = box.size > 0xFFFFFFFF ? 16 : 8;
  return { offset: box.offset + hdrLen, size: box.size - hdrLen, boxOffset: box.offset, boxSize: box.size };
}

// ─── moov parsing ──────────────────────────────────────

export function parseMoov(data) {
  const moovBox = findBox(data, 'moov', 0, data.length);
  if (!moovBox) throw new Error('No moov box found');

  const moovStart = moovBox.offset + 8;
  const moovEnd = moovBox.offset + moovBox.size;

  const tracks = [];

  for (const trakBox of iterBoxes(data, moovStart, moovEnd)) {
    if (trakBox.type !== 'trak') continue;
    const trakStart = trakBox.offset + 8;
    const trakEnd = trakBox.offset + trakBox.size;

    // tkhd → track_id, width, height
    const tkhd = findBoxContent(data, 'tkhd', trakStart, trakEnd);
    if (!tkhd) continue;
    const tkhdData = data.subarray(tkhd.offset, tkhd.offset + tkhd.size);
    const version = tkhdData[0];
    let trackId, width, height;
    if (version === 0) {
      trackId = readU32(tkhdData, 12);
      width = readU32(tkhdData, 76) >>> 16;
      height = readU32(tkhdData, 80) >>> 16;
    } else {
      trackId = readU32(tkhdData, 20);
      width = readU32(tkhdData, 88) >>> 16;
      height = readU32(tkhdData, 92) >>> 16;
    }

    // mdia → mdhd (timescale), hdlr (handler), stsd (codec info)
    const mdia = findBoxContent(data, 'mdia', trakStart, trakEnd);
    if (!mdia) continue;

    // mdhd → timescale
    const mdhd = findBoxContent(data, 'mdhd', mdia.offset, mdia.offset + mdia.size);
    let timescale = 90000;
    if (mdhd) {
      const mdhdData = data.subarray(mdhd.offset, mdhd.offset + mdhd.size);
      const mdhdVer = mdhdData[0];
      timescale = mdhdVer === 0 ? readU32(mdhdData, 12) : readU32(mdhdData, 20);
    }

    // hdlr → handler type
    const hdlr = findBoxContent(data, 'hdlr', mdia.offset, mdia.offset + mdia.size);
    let handler = 'unkn';
    if (hdlr) {
      handler = boxType(data, hdlr.offset + 8);
    }

    // minf → stbl → stsd → av01 → av1C
    let av1cConfig = null;
    let highBitdepth = false;
    let sampleEntryType = null;
    const minf = findBoxContent(data, 'minf', mdia.offset, mdia.offset + mdia.size);
    if (minf) {
      const stbl = findBoxContent(data, 'stbl', minf.offset, minf.offset + minf.size);
      if (stbl) {
        const stsd = findBoxContent(data, 'stsd', stbl.offset, stbl.offset + stbl.size);
        if (stsd) {
          // stsd: version(1) + flags(3) + entry_count(4) then sample entries
          const entryStart = stsd.offset + 8;
          const entryEnd = stsd.offset + stsd.size;
          for (const entryBox of iterBoxes(data, entryStart, entryEnd)) {
            sampleEntryType = entryBox.type;
            if (entryBox.type === 'av01') {
              // Find av1C inside av01
              // av01 sample entry: 8 (box hdr) + 6 (reserved) + 2 (data_ref_idx) + 2+2+12 (pre_def) + 2+2 (w,h) + 4+4+4+2+32+2+2 = 78 bytes of fixed data, then child boxes
              const av01ContentStart = entryBox.offset + 8 + 78;
              const av01ContentEnd = entryBox.offset + entryBox.size;
              const av1c = findBoxContent(data, 'av1C', av01ContentStart, av01ContentEnd);
              if (av1c) {
                av1cConfig = data.slice(av1c.offset, av1c.offset + av1c.size);
                // Parse high_bitdepth from av1C
                if (av1cConfig.length >= 2) {
                  highBitdepth = !!((av1cConfig[1] >> 6) & 1);
                }
              }
            }
          }
        }
      }
    }

    tracks.push({ trackId, handler, timescale, width, height, av1cConfig, highBitdepth, sampleEntryType });
  }

  return { tracks, moovEnd: moovBox.offset + moovBox.size };
}

// ─── Fragment parsing ──────────────────────────────────

export function parseFragments(data, moovEnd) {
  const fragments = [];

  for (const box of iterBoxes(data, moovEnd, data.length)) {
    if (box.type !== 'moof') continue;

    const moofStart = box.offset;
    const moofEnd = box.offset + box.size;
    const moofContentStart = box.offset + 8;

    // Find the mdat that follows this moof
    let mdatOffset = moofEnd;
    let mdatSize = 0;
    if (mdatOffset + 8 <= data.length && boxType(data, mdatOffset + 4) === 'mdat') {
      mdatSize = readU32(data, mdatOffset);
      if (mdatSize === 1 && mdatOffset + 16 <= data.length) {
        mdatSize = readU64(data, mdatOffset + 8);
      }
    }
    const mdatDataStart = mdatOffset + 8;

    // Parse mfhd
    let seqNum = 0;
    const mfhd = findBoxContent(data, 'mfhd', moofContentStart, moofEnd);
    if (mfhd) {
      seqNum = readU32(data, mfhd.offset + 4);
    }

    // Parse each traf
    for (const trafBox of iterBoxes(data, moofContentStart, moofEnd)) {
      if (trafBox.type !== 'traf') continue;
      const trafStart = trafBox.offset + 8;
      const trafEnd = trafBox.offset + trafBox.size;

      // tfhd → track_id
      const tfhd = findBoxContent(data, 'tfhd', trafStart, trafEnd);
      if (!tfhd) continue;
      const tfhdData = data.subarray(tfhd.offset, tfhd.offset + tfhd.size);
      const tfhdFlags = (tfhdData[1] << 16) | (tfhdData[2] << 8) | tfhdData[3];
      const trackId = readU32(tfhdData, 4);

      // Parse tfhd optional fields
      let tfhdOff = 8;
      let defaultDuration = 0, defaultSize = 0, defaultFlags = 0;
      if (tfhdFlags & 0x000008) { /* base_data_offset */ tfhdOff += 8; }
      if (tfhdFlags & 0x000010) { /* sample_description_index */ tfhdOff += 4; }
      if (tfhdFlags & 0x000020) { defaultDuration = readU32(tfhdData, tfhdOff); tfhdOff += 4; }
      if (tfhdFlags & 0x000040) { defaultSize = readU32(tfhdData, tfhdOff); tfhdOff += 4; }
      if (tfhdFlags & 0x000080) { defaultFlags = readU32(tfhdData, tfhdOff); tfhdOff += 4; }

      // tfdt → base_decode_time
      const tfdt = findBoxContent(data, 'tfdt', trafStart, trafEnd);
      let baseDecodeTime = 0;
      if (tfdt) {
        const tfdtData = data.subarray(tfdt.offset, tfdt.offset + tfdt.size);
        const tfdtVer = tfdtData[0];
        baseDecodeTime = tfdtVer === 0 ? readU32(tfdtData, 4) : readU64(tfdtData, 4);
      }

      // trun → samples + data_offset
      const trun = findBoxContent(data, 'trun', trafStart, trafEnd);
      if (!trun) continue;
      const trunData = data.subarray(trun.offset, trun.offset + trun.size);
      const trunFlags = (trunData[1] << 16) | (trunData[2] << 8) | trunData[3];
      const sampleCount = readU32(trunData, 4);

      let trunOff = 8;
      let dataOffset = 0;
      if (trunFlags & 0x000001) { dataOffset = readU32(trunData, trunOff) | 0; trunOff += 4; }
      if (trunFlags & 0x000004) { /* first_sample_flags */ trunOff += 4; }

      const hasDuration = !!(trunFlags & 0x000100);
      const hasSize = !!(trunFlags & 0x000200);
      const hasFlags = !!(trunFlags & 0x000400);
      const hasCts = !!(trunFlags & 0x000800);

      const samples = [];
      for (let s = 0; s < sampleCount; s++) {
        const duration = hasDuration ? readU32(trunData, trunOff) : defaultDuration; if (hasDuration) trunOff += 4;
        const size = hasSize ? readU32(trunData, trunOff) : defaultSize; if (hasSize) trunOff += 4;
        const flags = hasFlags ? readU32(trunData, trunOff) : defaultFlags; if (hasFlags) trunOff += 4;
        if (hasCts) trunOff += 4;
        samples.push({ duration, size, flags });
      }

      // Calculate absolute data position: data_offset is relative to moof start
      const absDataStart = moofStart + dataOffset;
      const totalDataSize = samples.reduce((a, s) => a + s.size, 0);

      fragments.push({
        seqNum,
        trackId,
        baseDecodeTime,
        samples,
        dataOffset: absDataStart,
        dataSize: totalDataSize,
      });
    }
  }

  return fragments;
}

// ─── Single-track init segment builder ─────────────────

export function writeU32(buf, val) { buf.push((val >>> 24) & 0xFF, (val >>> 16) & 0xFF, (val >>> 8) & 0xFF, val & 0xFF); }
export function writeU16(buf, val) { buf.push((val >>> 8) & 0xFF, val & 0xFF); }
export function writeBox(buf, type, content) {
  const size = 8 + content.length;
  writeU32(buf, size);
  for (let i = 0; i < 4; i++) buf.push(type.charCodeAt(i));
  for (let i = 0; i < content.length; i++) buf.push(content[i]);
}

export function buildSingleTrackInit(trackInfo) {
  const buf = [];

  // ftyp
  const ftypContent = [];
  // major brand
  for (const c of 'isom') ftypContent.push(c.charCodeAt(0));
  writeU32(ftypContent, 0); // minor version
  for (const brand of ['isom', 'iso6', 'cmfc', 'av01', 'mp41']) {
    for (const c of brand) ftypContent.push(c.charCodeAt(0));
  }
  writeBox(buf, 'ftyp', ftypContent);

  // moov
  const moovContent = [];

  // mvhd
  {
    const c = [];
    c.push(0, 0, 0, 0); // version + flags
    writeU32(c, 0); // creation_time
    writeU32(c, 0); // modification_time
    writeU32(c, trackInfo.timescale);
    writeU32(c, 0); // duration
    writeU32(c, 0x00010000); // rate 1.0
    writeU16(c, 0x0100); // volume
    for (let i = 0; i < 10; i++) c.push(0); // reserved
    // matrix
    const matrix = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];
    for (const m of matrix) writeU32(c, m);
    for (let i = 0; i < 24; i++) c.push(0); // pre_defined
    writeU32(c, 2); // next_track_id
    writeBox(moovContent, 'mvhd', c);
  }

  // trak
  {
    const trakContent = [];

    // tkhd
    {
      const c = [];
      c.push(0, 0, 0, 3); // version + flags (enabled, in movie)
      writeU32(c, 0); // creation_time
      writeU32(c, 0); // modification_time
      writeU32(c, 1); // track_id = 1
      writeU32(c, 0); // reserved
      writeU32(c, 0); // duration
      for (let i = 0; i < 8; i++) c.push(0); // reserved
      writeU16(c, 0); // layer
      writeU16(c, 0); // alternate_group
      writeU16(c, 0); // volume
      writeU16(c, 0); // reserved
      const matrix = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];
      for (const m of matrix) writeU32(c, m);
      writeU32(c, trackInfo.width << 16);
      writeU32(c, trackInfo.height << 16);
      writeBox(trakContent, 'tkhd', c);
    }

    // mdia
    {
      const mdiaContent = [];

      // mdhd
      {
        const c = [];
        c.push(0, 0, 0, 0); // version + flags
        writeU32(c, 0); writeU32(c, 0); // creation/modification
        writeU32(c, trackInfo.timescale);
        writeU32(c, 0); // duration
        writeU16(c, 0x55c4); // language: und
        writeU16(c, 0);
        writeBox(mdiaContent, 'mdhd', c);
      }

      // hdlr
      {
        const c = [];
        c.push(0, 0, 0, 0); // version + flags
        writeU32(c, 0); // pre_defined
        const h = trackInfo.handler === 'vide' ? 'vide' : 'meta';
        for (const ch of h) c.push(ch.charCodeAt(0));
        for (let i = 0; i < 12; i++) c.push(0); // reserved
        const name = trackInfo.handler === 'vide' ? 'VideoHandler\0' : 'MetaHandler\0\0';
        for (const ch of name) c.push(ch.charCodeAt(0));
        writeBox(mdiaContent, 'hdlr', c);
      }

      // minf
      {
        const minfContent = [];

        // vmhd or nmhd
        if (trackInfo.handler === 'vide') {
          const c = [];
          c.push(0, 0, 0, 1); // version + flags
          writeU16(c, 0); // graphicsmode
          for (let i = 0; i < 6; i++) c.push(0); // opcolor
          writeBox(minfContent, 'vmhd', c);
        } else {
          writeBox(minfContent, 'nmhd', [0, 0, 0, 0]);
        }

        // dinf → dref
        {
          const dinfContent = [];
          const drefContent = [];
          drefContent.push(0, 0, 0, 0); // version + flags
          writeU32(drefContent, 1); // entry_count
          // url entry (self-contained)
          writeBox(drefContent, 'url ', [0, 0, 0, 1]);
          writeBox(dinfContent, 'dref', drefContent);
          writeBox(minfContent, 'dinf', dinfContent);
        }

        // stbl
        {
          const stblContent = [];

          // stsd
          {
            const stsdContent = [];
            stsdContent.push(0, 0, 0, 0); // version + flags
            writeU32(stsdContent, 1); // entry_count

            if (trackInfo.handler === 'vide' && trackInfo.av1cConfig) {
              // av01 sample entry
              const av01Content = [];
              for (let i = 0; i < 6; i++) av01Content.push(0); // reserved
              writeU16(av01Content, 1); // data_reference_index
              writeU16(av01Content, 0); // pre_defined
              writeU16(av01Content, 0); // reserved
              for (let i = 0; i < 12; i++) av01Content.push(0); // pre_defined
              writeU16(av01Content, trackInfo.width);
              writeU16(av01Content, trackInfo.height);
              writeU32(av01Content, 0x00480000); // h res 72dpi
              writeU32(av01Content, 0x00480000); // v res 72dpi
              writeU32(av01Content, 0); // reserved
              writeU16(av01Content, 1); // frame_count
              // compressor name (32 bytes)
              const cname = 'xoq-replay';
              av01Content.push(cname.length);
              for (const ch of cname) av01Content.push(ch.charCodeAt(0));
              for (let i = 0; i < 31 - cname.length; i++) av01Content.push(0);
              writeU16(av01Content, 0x0018); // depth
              av01Content.push(0xFF, 0xFF); // pre_defined = -1

              // av1C box
              const av1cContent = [];
              for (let i = 0; i < trackInfo.av1cConfig.length; i++) av1cContent.push(trackInfo.av1cConfig[i]);
              writeBox(av01Content, 'av1C', av1cContent);

              writeBox(stsdContent, 'av01', av01Content);
            } else {
              // mett (text metadata sample entry)
              const mettContent = [];
              for (let i = 0; i < 6; i++) mettContent.push(0); // reserved
              writeU16(mettContent, 1); // data_reference_index
              const mime = 'application/octet-stream\0';
              for (const ch of mime) mettContent.push(ch.charCodeAt(0));
              writeBox(stsdContent, 'mett', mettContent);
            }

            writeBox(stblContent, 'stsd', stsdContent);
          }

          // Empty required boxes
          for (const btype of ['stts', 'stsc', 'stsz', 'stco']) {
            const c = [];
            c.push(0, 0, 0, 0); // version + flags
            writeU32(c, 0); // entry_count
            if (btype === 'stsz') writeU32(c, 0); // sample_size (before count for stsz)
            writeBox(stblContent, btype, c);
          }

          writeBox(minfContent, 'stbl', stblContent);
        }

        writeBox(mdiaContent, 'minf', minfContent);
      }

      writeBox(trakContent, 'mdia', mdiaContent);
    }

    writeBox(moovContent, 'trak', trakContent);
  }

  // mvex → trex
  {
    const mvexContent = [];
    const trexContent = [];
    trexContent.push(0, 0, 0, 0); // version + flags
    writeU32(trexContent, 1); // track_id = 1
    writeU32(trexContent, 1); // default_sample_description_index
    writeU32(trexContent, 0); // default_sample_duration
    writeU32(trexContent, 0); // default_sample_size
    writeU32(trexContent, 0); // default_sample_flags
    writeBox(mvexContent, 'trex', trexContent);
    writeBox(moovContent, 'mvex', mvexContent);
  }

  writeBox(buf, 'moov', moovContent);

  return new Uint8Array(buf);
}

// ─── Single-track media segment builder ────────────────

export function buildSingleTrackSegment(frag, data, sourceData, seqNum, timeOffsetUnits = 0) {
  // frag: { baseDecodeTime, samples, dataOffset, dataSize }
  // data: full file ArrayBuffer
  // sourceData: Uint8Array view of full file
  // timeOffsetUnits: units to subtract from baseDecodeTime (in track timescale units)

  const sampleData = sourceData.subarray(frag.dataOffset, frag.dataOffset + frag.dataSize);

  const buf = [];

  // moof
  const moofContent = [];

  // mfhd
  {
    const c = [];
    c.push(0, 0, 0, 0); // version + flags
    writeU32(c, seqNum);
    writeBox(moofContent, 'mfhd', c);
  }

  // traf
  {
    const trafContent = [];

    // tfhd (track_id=1, default-base-is-moof)
    {
      const c = [];
      c.push(0, 0x02, 0x00, 0x00); // version + flags (default-base-is-moof)
      writeU32(c, 1); // track_id = 1
      writeBox(trafContent, 'tfhd', c);
    }

    // tfdt (version 1 for 64-bit base_decode_time)
    {
      const c = [];
      c.push(1, 0, 0, 0); // version 1 + flags
      // 64-bit base_decode_time (rebased by timeOffsetUnits)
      const rebasedTime = Math.max(0, frag.baseDecodeTime - timeOffsetUnits);
      writeU32(c, Math.floor(rebasedTime / 0x100000000));
      writeU32(c, rebasedTime >>> 0);
      writeBox(trafContent, 'tfdt', c);
    }

    // trun (with data_offset, duration, size, flags, cts)
    {
      const c = [];
      c.push(0, 0x00, 0x0F, 0x01); // version + flags: data-offset + duration + size + flags + cts
      writeU32(c, frag.samples.length); // sample_count
      writeU32(c, 0); // data_offset placeholder (will be patched)

      for (const sample of frag.samples) {
        writeU32(c, sample.duration);
        writeU32(c, sample.size);
        writeU32(c, sample.flags);
        writeU32(c, 0); // composition_time_offset
      }

      writeBox(trafContent, 'trun', c);
    }

    writeBox(moofContent, 'traf', trafContent);
  }

  writeBox(buf, 'moof', moofContent);

  // Build moof as Uint8Array
  const moofBytes = new Uint8Array(buf);
  const moofSize = moofBytes.length;

  // Patch data_offset in trun: relative to start of moof
  const dataOffset = moofSize + 8; // +8 for mdat header
  for (let i = 8; i < moofBytes.length - 8; i++) {
    if (moofBytes[i+4] === 0x74 && moofBytes[i+5] === 0x72 && moofBytes[i+6] === 0x75 && moofBytes[i+7] === 0x6E) { // 'trun'
      const doPos = i + 8 + 4 + 4; // box_hdr(8) + version_flags(4) + sample_count(4)
      moofBytes[doPos]   = (dataOffset >>> 24) & 0xFF;
      moofBytes[doPos+1] = (dataOffset >>> 16) & 0xFF;
      moofBytes[doPos+2] = (dataOffset >>> 8) & 0xFF;
      moofBytes[doPos+3] = dataOffset & 0xFF;
      break;
    }
  }

  // Build mdat header
  const mdatHeader = new Uint8Array(8);
  const mdatSize = 8 + sampleData.length;
  mdatHeader[0] = (mdatSize >>> 24) & 0xFF;
  mdatHeader[1] = (mdatSize >>> 16) & 0xFF;
  mdatHeader[2] = (mdatSize >>> 8) & 0xFF;
  mdatHeader[3] = mdatSize & 0xFF;
  mdatHeader[4] = 0x6D; mdatHeader[5] = 0x64; mdatHeader[6] = 0x61; mdatHeader[7] = 0x74; // 'mdat'

  // Concatenate moof + mdat header + sample data
  const result = new Uint8Array(moofSize + 8 + sampleData.length);
  result.set(moofBytes, 0);
  result.set(mdatHeader, moofSize);
  result.set(sampleData, moofSize + 8);
  return result;
}

// ─── Track identification ──────────────────────────────

export function classifyTracks(tracks, fragments, fileData) {
  // Returns: { videoTracks, depthTracks, metaTracks, canTracks }
  // Group by RS source: tracks appear in order [video, depth, metadata] per source, then CAN last
  const videoTracks = [];
  const depthTracks = [];
  const metaTracks = [];
  const canTracks = [];

  for (const track of tracks) {
    if (track.handler === 'vide') {
      if (track.highBitdepth) {
        depthTracks.push(track);
      } else {
        videoTracks.push(track);
      }
    } else if (track.handler === 'meta') {
      // Disambiguate CAN vs metadata by checking first sample content
      const firstFrag = fragments.find(f => f.trackId === track.trackId);
      if (firstFrag && firstFrag.dataSize > 0) {
        const firstByte = fileData[firstFrag.dataOffset];
        if (firstByte === 0x7B) { // '{' = JSON
          metaTracks.push(track);
        } else {
          canTracks.push(track);
        }
      } else {
        // No data — guess based on position (last meta track without data is likely CAN)
        canTracks.push(track);
      }
    }
  }

  return { videoTracks, depthTracks, metaTracks, canTracks };
}

// ─── candump log parsing ────────────────────────────────
// Parses candump log format back into canfd_frame bytes + timestamps.
// Format: (seconds.microseconds) interface CAN_ID#DATA        (classic)
//         (seconds.microseconds) interface CAN_ID##FDATA      (CAN FD)
// Returns [{timeMs, data: Uint8Array}] where data is concatenated 72-byte canfd_frames.

const CANFD_FRAME_SIZE = 72;

function parseCandumpLog(text) {
  const events = [];
  const lines = text.split('\n');
  let currentMs = -1;
  let currentFrames = [];

  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed) continue;

    // (secs.usecs) iface ID#DATA  or  ID##FDATA
    const m = trimmed.match(/^\((\d+)\.(\d+)\)\s+\S+\s+([0-9A-Fa-f]+)(##([0-9A-Fa-f])([0-9A-Fa-f]*)|#([0-9A-Fa-f]*))$/);
    if (!m) continue;

    const secs = parseInt(m[1]);
    const usecs = parseInt(m[2]);
    const timeMs = secs * 1000 + usecs / 1000;

    const idStr = m[3];
    const canId = parseInt(idStr, 16);
    const eff = idStr.length > 3;

    let fdFlags = 0;
    let hexData = '';
    if (m[5] !== undefined) {
      fdFlags = parseInt(m[5], 16);
      hexData = m[6] || '';
    } else {
      hexData = m[7] || '';
    }

    const frame = new Uint8Array(CANFD_FRAME_SIZE);
    const view = new DataView(frame.buffer);
    let rawId = canId;
    if (eff) rawId |= 0x80000000;
    view.setUint32(0, rawId, true);

    const dataLen = hexData.length >>> 1;
    frame[4] = dataLen;
    frame[5] = fdFlags;
    for (let i = 0; i < dataLen; i++) {
      frame[8 + i] = parseInt(hexData.substr(i * 2, 2), 16);
    }

    // Group consecutive frames with the same timestamp
    if (timeMs !== currentMs && currentFrames.length > 0) {
      const data = new Uint8Array(currentFrames.length * CANFD_FRAME_SIZE);
      for (let i = 0; i < currentFrames.length; i++) data.set(currentFrames[i], i * CANFD_FRAME_SIZE);
      events.push({ timeMs: currentMs, data });
      currentFrames = [];
    }
    currentMs = timeMs;
    currentFrames.push(frame);
  }

  if (currentFrames.length > 0) {
    const data = new Uint8Array(currentFrames.length * CANFD_FRAME_SIZE);
    for (let i = 0; i < currentFrames.length; i++) data.set(currentFrames[i], i * CANFD_FRAME_SIZE);
    events.push({ timeMs: currentMs, data });
  }

  return events;
}

// ─── Time formatting ───────────────────────────────────

function formatTime(seconds) {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s.toString().padStart(2, '0')}`;
}

// ─── ReplayController ──────────────────────────────────

export class ReplayController {
  constructor({ armStates, rsCams, rsVideoEls, pointClouds, appState, config }) {
    this.armStates = armStates;
    this.rsCams = rsCams;
    this.rsVideoEls = rsVideoEls;
    this.pointClouds = pointClouds;
    this.appState = appState;
    this.config = config;

    this.fileData = null;
    this.tracks = null;
    this.fragments = null;
    this.classified = null;
    this.durationMs = 0;
    this._timeOffsetMs = 0;

    // Per-RS-source players
    this.colorPlayers = [];
    this.depthDecoders = [];
    this.colorVideoEls = [];
    this.depthVideoEls = [];

    // CAN + metadata pre-parsed events
    this.canEvents = []; // [{timeMs, data}]
    this.metaEvents = []; // [{timeMs, json, sourceIdx}]

    // Playback state
    this.playing = false;
    this.speed = 1;
    this.masterVideo = null;
    this._rafId = null;
    this._destroyed = false;

    // Callbacks
    this.onTimeUpdate = null;
    this.onEnded = null;
  }

  async loadFile(file) {
    log(`Loading recording: ${file.name} (${(file.size / 1024 / 1024).toFixed(1)} MB)`, 'info');

    const arrayBuf = await file.arrayBuffer();
    this.fileData = new Uint8Array(arrayBuf);

    // Parse moov
    const { tracks, moovEnd } = parseMoov(this.fileData);
    this.tracks = tracks;
    log(`Found ${tracks.length} tracks`, 'data');

    // Parse all fragments
    this.fragments = parseFragments(this.fileData, moovEnd);
    log(`Found ${this.fragments.length} fragments`, 'data');

    // Classify tracks
    this.classified = classifyTracks(tracks, this.fragments, this.fileData);
    const { videoTracks, depthTracks, metaTracks, canTracks } = this.classified;
    log(`Tracks: ${videoTracks.length} video, ${depthTracks.length} depth, ${metaTracks.length} metadata, ${canTracks.length} CAN`, 'data');

    // Compute duration using relative timestamps (max_end - min_start)
    // fMP4 segments may use absolute wall-clock baseMediaDecodeTime
    let minStartMs = Infinity, maxEndMs = 0;
    for (const trk of [...videoTracks, ...depthTracks, ...canTracks]) {
      const frags = this.fragments.filter(f => f.trackId === trk.trackId);
      if (frags.length === 0) continue;
      const first = frags[0];
      const last = frags[frags.length - 1];
      const startMs = (first.baseDecodeTime / trk.timescale) * 1000;
      const endMs = ((last.baseDecodeTime + last.samples.reduce((a, s) => a + s.duration, 0)) / trk.timescale) * 1000;
      if (startMs < minStartMs) minStartMs = startMs;
      if (endMs > maxEndMs) maxEndMs = endMs;
    }
    this._timeOffsetMs = isFinite(minStartMs) ? minStartMs : 0;
    this.durationMs = maxEndMs > minStartMs ? maxEndMs - minStartMs : 0;

    log(`Duration: ${formatTime(this.durationMs / 1000)}`, 'data');

    // ── Set up video/depth MSE players ──
    const numSources = videoTracks.length;

    for (let i = 0; i < numSources; i++) {
      const vTrack = videoTracks[i];
      const dTrack = depthTracks[i]; // may be undefined if no depth

      // Build single-track init segments
      const videoInit = buildSingleTrackInit(vTrack);
      const videoFrags = this.fragments.filter(f => f.trackId === vTrack.trackId);

      // Use existing rsVideoEl if available, otherwise log warning
      const videoEl = this.rsVideoEls[i];
      if (!videoEl) {
        log(`No video element for source ${i}`, 'error');
        continue;
      }

      // Create MsePlayer for color (replay mode: no auto-play, no buffer trimming)
      const colorPlayer = new MsePlayer(videoEl, `Replay Color ${i}`, { liveMode: false });
      this.colorPlayers.push(colorPlayer);
      this.colorVideoEls.push(videoEl);

      // Feed init segment (includes ftyp + moov)
      colorPlayer.onData(videoInit);

      // Feed all video fragments
      let seqNum = 1;
      for (const frag of videoFrags) {
        const seg = buildSingleTrackSegment(frag, arrayBuf, this.fileData, seqNum++);
        colorPlayer.onData(seg);
      }

      // Use first video element as master clock
      if (i === 0) this.masterVideo = videoEl;

      // Depth — use MSE + DepthVideoExtractor (plays in sync with video timeline)
      if (dTrack) {
        const depthInit = buildSingleTrackInit(dTrack);
        const depthFrags = this.fragments.filter(f => f.trackId === dTrack.trackId);

        const depthVideoEl = document.createElement('video');
        depthVideoEl.muted = true;
        depthVideoEl.playsInline = true;
        depthVideoEl.style.cssText = 'position:absolute;width:1px;height:1px;opacity:0;pointer-events:none;';
        document.body.appendChild(depthVideoEl);
        this.depthVideoEls.push(depthVideoEl);

        const depthPlayer = new MsePlayer(depthVideoEl, `Replay Depth ${i}`, { liveMode: false });
        this.depthDecoders.push(depthPlayer);

        depthPlayer.onData(depthInit);
        let depthSeq = 1;
        for (const frag of depthFrags) {
          const seg = buildSingleTrackSegment(frag, arrayBuf, this.fileData, depthSeq++);
          depthPlayer.onData(seg);
        }

        const codec = detectCodec(depthInit);
        const extractor = new DepthVideoExtractor(depthVideoEl, codec);
        if (this.rsCams[i]) this.rsCams[i].depthDecoder = extractor;
      }
    }

    // ── Pre-parse CAN events ──
    for (const ct of canTracks) {
      const frags = this.fragments.filter(f => f.trackId === ct.trackId);
      const timescale = ct.timescale || 1000;
      for (const frag of frags) {
        let sampleOffset = frag.dataOffset;
        let timeUnits = frag.baseDecodeTime;
        for (const sample of frag.samples) {
          const sampleData = this.fileData.subarray(sampleOffset, sampleOffset + sample.size);
          const timeMs = (timeUnits / timescale) * 1000 - this._timeOffsetMs;
          this.canEvents.push({ timeMs, data: sampleData });
          timeUnits += sample.duration;
          sampleOffset += sample.size;
        }
      }
    }
    this.canEvents.sort((a, b) => a.timeMs - b.timeMs);
    if (this.canEvents.length > 0) log(`CAN: ${this.canEvents.length} events`, 'data');

    // ── Pre-parse metadata events ──
    for (let mi = 0; mi < metaTracks.length; mi++) {
      const mt = metaTracks[mi];
      const frags = this.fragments.filter(f => f.trackId === mt.trackId);
      const timescale = mt.timescale || 1000;
      for (const frag of frags) {
        let sampleOffset = frag.dataOffset;
        let timeUnits = frag.baseDecodeTime;
        for (const sample of frag.samples) {
          const sampleData = this.fileData.subarray(sampleOffset, sampleOffset + sample.size);
          const timeMs = (timeUnits / timescale) * 1000 - this._timeOffsetMs;
          try {
            const json = JSON.parse(new TextDecoder().decode(sampleData));
            this.metaEvents.push({ timeMs, json, sourceIdx: mi });
          } catch (e) { /* skip non-JSON */ }
          timeUnits += sample.duration;
          sampleOffset += sample.size;
        }
      }
    }
    this.metaEvents.sort((a, b) => a.timeMs - b.timeMs);
    if (this.metaEvents.length > 0) log(`Metadata: ${this.metaEvents.length} events`, 'data');

    // Wait for MSE to be ready before allowing play
    await this._waitForMseReady();

    log('Replay loaded', 'success');
  }

  async loadFiles(files) {
    log(`Loading ${files.length} recording files`, 'info');

    // Separate files by extension
    const mp4Files = [];
    const binFiles = [];
    const logFiles = [];
    for (const file of files) {
      if (file.name.endsWith('.bin')) {
        binFiles.push(file);
      } else if (file.name.endsWith('.log')) {
        logFiles.push(file);
      } else {
        mp4Files.push(file);
      }
    }

    // Parse .mp4 files: extract tracks, fragments, and raw data per file
    const parsed = [];
    for (const file of mp4Files) {
      const arrayBuf = await file.arrayBuffer();
      const fileData = new Uint8Array(arrayBuf);
      const { tracks, moovEnd } = parseMoov(fileData);
      const fragments = parseFragments(fileData, moovEnd);
      const classified = classifyTracks(tracks, fragments, fileData);
      parsed.push({ file, fileData, arrayBuf, tracks, fragments, classified });
      log(`  ${file.name}: ${tracks.length} tracks, ${fragments.length} fragments`, 'data');
    }

    // Parse .bin files: timestamped binary [u64_le ts][u32_le len][data]
    // Classify by filename: *_can.bin or *can*.bin → CAN, *_meta.bin → metadata
    const binParsed = [];
    for (const file of binFiles) {
      const arrayBuf = await file.arrayBuffer();
      const fileData = new Uint8Array(arrayBuf);
      const view = new DataView(arrayBuf);
      const events = [];
      let off = 0;
      while (off + 12 <= fileData.length) {
        const tsLow = view.getUint32(off, true);
        const tsHigh = view.getUint32(off + 4, true);
        const tsMs = tsHigh * 0x100000000 + tsLow;
        const dataLen = view.getUint32(off + 8, true);
        off += 12;
        if (off + dataLen > fileData.length) break;
        const data = fileData.subarray(off, off + dataLen);
        events.push({ timeMs: tsMs, data });
        off += dataLen;
      }
      const isCan = /can/i.test(file.name) && !/meta/i.test(file.name);
      binParsed.push({ file, events, isCan });
      log(`  ${file.name}: ${events.length} ${isCan ? 'CAN' : 'metadata'} events (.bin)`, 'data');
    }

    // Parse .log files: candump log format → canfd_frame bytes + timestamps
    const logParsed = [];
    for (const file of logFiles) {
      const text = await file.text();
      const events = parseCandumpLog(text);
      logParsed.push({ file, events });
      const totalFrames = events.reduce((a, e) => a + e.data.length / CANFD_FRAME_SIZE, 0);
      log(`  ${file.name}: ${events.length} batches, ${totalFrames} CAN frames (.log)`, 'data');
    }

    // Compute per-file time offset BEFORE classification (each mp4 file may use a different clock)
    for (const p of parsed) {
      let minMs = Infinity;
      for (const trk of p.tracks) {
        const frags = p.fragments.filter(f => f.trackId === trk.trackId);
        if (frags.length > 0) {
          const startMs = (frags[0].baseDecodeTime / trk.timescale) * 1000;
          if (startMs < minMs) minMs = startMs;
        }
      }
      p._fileOffsetMs = isFinite(minMs) ? minMs : 0;
    }
    this._timeOffsetMs = 0; // All times are now relative (per-file rebased)

    // Collect all classified tracks across .mp4 files.
    // For separate-file recordings, use filename hints to override classification
    // since both color and depth are AV1 and classifyTracks can't distinguish them.
    const allVideo = [];
    const allDepth = [];
    const allMeta = [];
    const allCan = [];
    for (const p of parsed) {
      const name = p.file.name.toLowerCase();
      const isDepthFile = /depth/i.test(name);
      const isColorFile = /color/i.test(name);
      if (isDepthFile) {
        // All video-classified tracks in a depth file → depth
        for (const vt of p.classified.videoTracks) allDepth.push({ track: vt, ...p });
        for (const dt of p.classified.depthTracks) allDepth.push({ track: dt, ...p });
      } else if (isColorFile) {
        // All tracks in a color file → video
        for (const vt of p.classified.videoTracks) allVideo.push({ track: vt, ...p });
        for (const dt of p.classified.depthTracks) allVideo.push({ track: dt, ...p });
      } else {
        // No filename hint — use classifyTracks result as-is
        for (const vt of p.classified.videoTracks) allVideo.push({ track: vt, ...p });
        for (const dt of p.classified.depthTracks) allDepth.push({ track: dt, ...p });
      }
      for (const mt of p.classified.metaTracks) allMeta.push({ track: mt, ...p });
      for (const ct of p.classified.canTracks) allCan.push({ track: ct, ...p });
    }

    log(`Total: ${allVideo.length} video, ${allDepth.length} depth, ${allMeta.length + binParsed.filter(b=>!b.isCan).length} metadata, ${allCan.length + binParsed.filter(b=>b.isCan).length + logParsed.length} CAN`, 'data');

    // Store combined references (use first file's data as primary)
    this.tracks = parsed.flatMap(p => p.tracks);
    this.fragments = parsed.flatMap(p => p.fragments);
    if (parsed.length > 0) this.fileData = parsed[0].fileData;

    // Compute duration across all sources (now relative per-file)
    let maxRelMs = 0;
    for (const { track, fragments: frags, _fileOffsetMs } of [...allVideo, ...allDepth, ...allCan]) {
      const trackFrags = frags.filter(f => f.trackId === track.trackId);
      if (trackFrags.length === 0) continue;
      const last = trackFrags[trackFrags.length - 1];
      const endMs = ((last.baseDecodeTime + last.samples.reduce((a, s) => a + s.duration, 0)) / track.timescale) * 1000 - (_fileOffsetMs || 0);
      if (endMs > maxRelMs) maxRelMs = endMs;
    }
    // .bin and .log files use relative timestamps already
    for (const bp of binParsed) {
      if (bp.events.length > 0) {
        const lastMs = bp.events[bp.events.length - 1].timeMs;
        if (lastMs > maxRelMs) maxRelMs = lastMs;
      }
    }
    for (const lp of logParsed) {
      if (lp.events.length > 0) {
        const lastMs = lp.events[lp.events.length - 1].timeMs;
        if (lastMs > maxRelMs) maxRelMs = lastMs;
      }
    }
    this.durationMs = maxRelMs;

    log(`Duration: ${formatTime(this.durationMs / 1000)}`, 'data');

    // Set up video/depth MSE players (pair video[i] with depth[i])
    const numSources = allVideo.length;
    for (let i = 0; i < numSources; i++) {
      const { track: vTrack, fileData: vFileData, arrayBuf: vArrayBuf, fragments: vAllFrags, _fileOffsetMs } = allVideo[i];

      const videoEl = this.rsVideoEls[i];
      if (!videoEl) { log(`No video element for source ${i}`, 'error'); continue; }

      const videoInit = buildSingleTrackInit(vTrack);
      const videoFrags = vAllFrags.filter(f => f.trackId === vTrack.trackId);
      // Convert ms offset to timescale units for rebasing
      const vOffsetUnits = Math.round((_fileOffsetMs || 0) / 1000 * vTrack.timescale);

      const colorPlayer = new MsePlayer(videoEl, `Replay Color ${i}`, { liveMode: false });
      this.colorPlayers.push(colorPlayer);
      this.colorVideoEls.push(videoEl);

      colorPlayer.onData(videoInit);
      let seqNum = 1;
      for (const frag of videoFrags) {
        const seg = buildSingleTrackSegment(frag, vArrayBuf, vFileData, seqNum++, vOffsetUnits);
        colorPlayer.onData(seg);
      }

      if (i === 0) this.masterVideo = videoEl;

      // Depth — use MSE + DepthVideoExtractor (not WebCodecs DepthDecoder)
      // DepthDecoder decodes all frames at once so latestY holds only the last frame.
      // MSE plays frames in sync with the video timeline, so depth updates with playback.
      if (i < allDepth.length) {
        const { track: dTrack, fileData: dFileData, arrayBuf: dArrayBuf, fragments: dAllFrags, _fileOffsetMs: dFileOffsetMs } = allDepth[i];

        const depthInit = buildSingleTrackInit(dTrack);
        const depthFrags = dAllFrags.filter(f => f.trackId === dTrack.trackId);
        const dOffsetUnits = Math.round((dFileOffsetMs || 0) / 1000 * dTrack.timescale);

        // Create hidden video element for depth MSE playback
        const depthVideoEl = document.createElement('video');
        depthVideoEl.muted = true;
        depthVideoEl.playsInline = true;
        depthVideoEl.style.cssText = 'position:absolute;width:1px;height:1px;opacity:0;pointer-events:none;';
        document.body.appendChild(depthVideoEl);
        this.depthVideoEls.push(depthVideoEl);

        const depthPlayer = new MsePlayer(depthVideoEl, `Replay Depth ${i}`, { liveMode: false });
        this.depthDecoders.push(depthPlayer); // store for _waitForMseReady + destroy

        depthPlayer.onData(depthInit);
        let depthSeq = 1;
        for (const frag of depthFrags) {
          const seg = buildSingleTrackSegment(frag, dArrayBuf, dFileData, depthSeq++, dOffsetUnits);
          depthPlayer.onData(seg);
        }

        // Detect codec for 10-bit BT.709 reversal
        const codec = detectCodec(depthInit);

        // DepthVideoExtractor provides latestY/width/height/is10bit for the render loop
        const extractor = new DepthVideoExtractor(depthVideoEl, codec);
        if (this.rsCams[i]) this.rsCams[i].depthDecoder = extractor;
      }
    }

    // Pre-parse CAN events (from .mp4 fMP4 tracks)
    for (const { track: ct, fileData: cFileData, fragments: cAllFrags, _fileOffsetMs: cOffsetMs } of allCan) {
      const frags = cAllFrags.filter(f => f.trackId === ct.trackId);
      const timescale = ct.timescale || 1000;
      for (const frag of frags) {
        let sampleOffset = frag.dataOffset;
        let timeUnits = frag.baseDecodeTime;
        for (const sample of frag.samples) {
          const sampleData = cFileData.subarray(sampleOffset, sampleOffset + sample.size);
          const timeMs = (timeUnits / timescale) * 1000 - (cOffsetMs || 0);
          this.canEvents.push({ timeMs, data: sampleData });
          timeUnits += sample.duration;
          sampleOffset += sample.size;
        }
      }
    }
    // CAN events from .bin files
    for (const bp of binParsed) {
      if (bp.isCan) {
        for (const evt of bp.events) {
          this.canEvents.push({ timeMs: evt.timeMs, data: evt.data });
        }
      }
    }
    // CAN events from .log files (candump format)
    for (const lp of logParsed) {
      for (const evt of lp.events) {
        this.canEvents.push({ timeMs: evt.timeMs, data: evt.data });
      }
    }
    this.canEvents.sort((a, b) => a.timeMs - b.timeMs);
    if (this.canEvents.length > 0) log(`CAN: ${this.canEvents.length} events`, 'data');

    // Pre-parse metadata events (from .mp4 fMP4 tracks)
    for (let mi = 0; mi < allMeta.length; mi++) {
      const { track: mt, fileData: mFileData, fragments: mAllFrags, _fileOffsetMs: mOffsetMs } = allMeta[mi];
      const frags = mAllFrags.filter(f => f.trackId === mt.trackId);
      const timescale = mt.timescale || 1000;
      for (const frag of frags) {
        let sampleOffset = frag.dataOffset;
        let timeUnits = frag.baseDecodeTime;
        for (const sample of frag.samples) {
          const sampleData = mFileData.subarray(sampleOffset, sampleOffset + sample.size);
          const timeMs = (timeUnits / timescale) * 1000 - (mOffsetMs || 0);
          try {
            const json = JSON.parse(new TextDecoder().decode(sampleData));
            this.metaEvents.push({ timeMs, json, sourceIdx: mi });
          } catch (e) { /* skip non-JSON */ }
          timeUnits += sample.duration;
          sampleOffset += sample.size;
        }
      }
    }
    // Metadata events from .bin files
    let binMetaIdx = allMeta.length;
    for (const bp of binParsed) {
      if (!bp.isCan) {
        for (const evt of bp.events) {
          try {
            const json = JSON.parse(new TextDecoder().decode(evt.data));
            this.metaEvents.push({ timeMs: evt.timeMs, json, sourceIdx: binMetaIdx });
          } catch (e) { /* skip non-JSON */ }
        }
        binMetaIdx++;
      }
    }
    this.metaEvents.sort((a, b) => a.timeMs - b.timeMs);
    if (this.metaEvents.length > 0) log(`Metadata: ${this.metaEvents.length} events`, 'data');

    await this._waitForMseReady();
    log('Replay loaded', 'success');
  }

  _waitForMseReady() {
    return new Promise(resolve => {
      const allPlayers = [...this.colorPlayers, ...this.depthDecoders.filter(d => d instanceof MsePlayer)];
      const check = () => {
        // Wait for all MSE players (color + depth) to be initialized AND have their queues drained
        const allReady = allPlayers.every(p =>
          (p.ready && p.queue.length === 0 && !p.sb?.updating) || p.frames === 0
        );
        if (allReady) {
          // Signal end of stream so browser knows media is complete
          for (const p of allPlayers) {
            if (p.ms && p.ms.readyState === 'open') {
              try { p.ms.endOfStream(); } catch {}
            }
          }
          // Pause all videos and seek to start (user will click Play)
          for (const el of [...this.colorVideoEls, ...this.depthVideoEls]) {
            el.pause();
            if (el.buffered.length > 0) el.currentTime = el.buffered.start(0);
          }
          resolve();
        } else {
          setTimeout(check, 50);
        }
      };
      check();
    });
  }

  play() {
    if (this._destroyed) return;
    this.playing = true;

    for (const el of [...this.colorVideoEls, ...this.depthVideoEls]) {
      el.playbackRate = this.speed;
      el.play().catch(() => {});
    }

    this._startAnimationLoop();
  }

  pause() {
    this.playing = false;
    for (const el of [...this.colorVideoEls, ...this.depthVideoEls]) {
      el.pause();
    }
    if (this._rafId) {
      cancelAnimationFrame(this._rafId);
      this._rafId = null;
    }
  }

  seek(timeMs) {
    // Convert relative timeMs to absolute video time (fMP4 may use wall-clock timestamps)
    const absMs = timeMs + (this._timeOffsetMs || 0);
    const timeSec = absMs / 1000;
    for (const el of [...this.colorVideoEls, ...this.depthVideoEls]) {
      if (el.buffered.length > 0) {
        const start = el.buffered.start(0);
        const end = el.buffered.end(el.buffered.length - 1);
        el.currentTime = Math.max(start, Math.min(end, timeSec));
      }
    }
    // Force depth extractors to re-extract after seek
    for (let i = 0; i < this.rsCams.length; i++) {
      const dec = this.rsCams[i]?.depthDecoder;
      if (dec && dec.forceExtract) dec.forceExtract();
    }
    // Apply CAN and metadata at this time
    this._applyCan(timeMs);
    this._applyMetadata(timeMs);
  }

  setSpeed(rate) {
    this.speed = rate;
    for (const el of [...this.colorVideoEls, ...this.depthVideoEls]) {
      el.playbackRate = rate;
    }
  }

  get currentTimeMs() {
    if (this.masterVideo && this.masterVideo.currentTime > 0) {
      return this.masterVideo.currentTime * 1000 - this._timeOffsetMs;
    }
    return 0;
  }

  _startAnimationLoop() {
    if (this._rafId) return;
    const loop = () => {
      if (this._destroyed || !this.playing) return;
      this._rafId = requestAnimationFrame(loop);

      const timeMs = this.currentTimeMs;

      // Apply CAN state at current time
      this._applyCan(timeMs);

      // Apply metadata at current time
      this._applyMetadata(timeMs);

      // Check if ended
      if (this.masterVideo && this.masterVideo.ended) {
        this.playing = false;
        if (this.onEnded) this.onEnded();
        return;
      }

      if (this.onTimeUpdate) this.onTimeUpdate(timeMs);
    };
    this._rafId = requestAnimationFrame(loop);
  }

  _applyCan(timeMs) {
    if (this.canEvents.length === 0) return;

    // Find latest CAN events up to timeMs
    // For each arm, apply the latest state
    let lo = 0, hi = this.canEvents.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >>> 1;
      if (this.canEvents[mid].timeMs <= timeMs) lo = mid;
      else hi = mid - 1;
    }
    if (this.canEvents[lo].timeMs > timeMs) return;

    // Apply events from slightly before to current time (last ~100ms window for smooth updates)
    const windowStart = Math.max(0, timeMs - 100);
    let startIdx = lo;
    while (startIdx > 0 && this.canEvents[startIdx - 1].timeMs >= windowStart) startIdx--;

    for (let i = startIdx; i <= lo; i++) {
      const evt = this.canEvents[i];
      const canFrames = parseAllCanFrames(evt.data);
      for (const frame of canFrames) {
        const jointIdx = canIdToJointIdx(frame.canId);
        if (jointIdx < 0) continue;
        const state = parseDamiaoState(frame.data);
        if (!state) continue;
        // Apply to all arm states — CAN events from recording may cover multiple arms
        // The CAN track is merged across all CAN sources, so we apply to the first arm pair
        for (const armState of this.armStates) {
          if (jointIdx < armState.length) {
            armState[jointIdx].targetAngle = state.qRad;
            armState[jointIdx].velocity = state.vel;
            armState[jointIdx].torque = state.tau;
            armState[jointIdx].tempMos = state.tempMos;
            armState[jointIdx].tempRotor = state.tempRotor;
            armState[jointIdx].updated = true;
          }
        }
      }
    }
  }

  _applyMetadata(timeMs) {
    if (this.metaEvents.length === 0) return;

    // For each source, find the latest metadata event ≤ timeMs
    const sourceLatest = {};
    for (const evt of this.metaEvents) {
      if (evt.timeMs > timeMs) break;
      sourceLatest[evt.sourceIdx] = evt;
    }

    for (const [idx, evt] of Object.entries(sourceLatest)) {
      const i = parseInt(idx);
      if (!this.rsCams[i]) continue;
      const meta = evt.json;
      if (meta.fx && meta.fy) {
        this.rsCams[i].intrinsics = meta;
      }
      if (meta.gravity) {
        this.rsCams[i].gravity = meta.gravity;
      }
    }
  }

  destroy() {
    this._destroyed = true;
    this.pause();

    for (const player of this.colorPlayers) {
      player.destroy();
    }
    for (const decoder of this.depthDecoders) {
      decoder.destroy();
    }

    // Destroy depth extractors stored on rsCams
    for (let i = 0; i < this.rsCams.length; i++) {
      const dec = this.rsCams[i]?.depthDecoder;
      if (dec && dec instanceof DepthVideoExtractor) dec.destroy();
      this.rsCams[i].depthDecoder = null;
      this.rsCams[i].intrinsics = null;
      this.rsCams[i].gravity = null;
      this.rsCams[i]._frustumUpdated = false;
    }

    // Remove hidden depth video elements from DOM
    for (const el of this.depthVideoEls) {
      el.pause();
      el.src = '';
      if (el.parentNode) el.parentNode.removeChild(el);
    }

    // Reset arm states
    for (const armState of this.armStates) {
      for (const joint of armState) {
        joint.targetAngle = 0;
        joint.velocity = 0;
        joint.torque = 0;
        joint.tempMos = 0;
        joint.tempRotor = 0;
      }
    }

    // Clear point cloud draw ranges
    if (this.pointClouds) {
      for (const pc of this.pointClouds) {
        pc.geometry.setDrawRange(0, 0);
      }
    }

    this.colorPlayers = [];
    this.depthDecoders = [];
    this.colorVideoEls = [];
    this.depthVideoEls = [];
    this.fileData = null;
    this.fragments = null;
    this.canEvents = [];
    this.metaEvents = [];

    log('Replay closed', 'info');
  }
}

export function createReplayController(opts) {
  return new ReplayController(opts);
}
