#!/usr/bin/env node
// Parse an AV1 Sequence Header OBU to find chroma_sample_position

// NVENC depth SeqHdr OBU from the user's real Safari output:
// 0a 0b 00 00 00 2d 4c ff b3 c0 21 9c 84
const nvencSeqHdr = new Uint8Array([0x0a, 0x0b, 0x00, 0x00, 0x00, 0x2d, 0x4c, 0xff, 0xb3, 0xc0, 0x21, 0x9c, 0x84]);

// Software encoder SeqHdr (640x480, worked on WebKit):
// 0a 0a 00 00 00 24 c4 ff df 00 68 02
const swSeqHdr = new Uint8Array([0x0a, 0x0a, 0x00, 0x00, 0x00, 0x24, 0xc4, 0xff, 0xdf, 0x00, 0x68, 0x02]);

function parseSeqHdr(obuData, label) {
  console.log(`\n=== ${label} (${obuData.length} bytes) ===`);
  console.log('Hex:', Array.from(obuData, b => b.toString(16).padStart(2,'0')).join(' '));

  // Skip OBU header
  const hasExt = (obuData[0] >> 2) & 1;
  const hasSize = (obuData[0] >> 1) & 1;
  let payloadStart = 1 + (hasExt ? 1 : 0);
  if (hasSize) {
    for (let i = 0; payloadStart + i < obuData.length; i++) {
      if (!(obuData[payloadStart + i] & 0x80)) { payloadStart += i + 1; break; }
    }
  }
  console.log(`Payload starts at byte ${payloadStart}`);

  let bp = payloadStart * 8;
  function read(n, name) {
    let val = 0;
    for (let i = 0; i < n; i++) {
      val = (val << 1) | ((obuData[bp >> 3] >> (7 - (bp & 7))) & 1);
      bp++;
    }
    if (name) console.log(`  [bit ${bp-n}-${bp-1}] ${name} = ${val}`);
    return val;
  }

  const seqProfile = read(3, 'seq_profile');
  read(1, 'still_picture');
  const rsh = read(1, 'reduced_still_picture_header');

  if (rsh) {
    read(5, 'seq_level_idx[0]');
  } else {
    const tip = read(1, 'timing_info_present_flag');
    if (tip) { console.log('  timing_info present — skipping rest'); return; }
    const iddp = read(1, 'initial_display_delay_present_flag');
    const opCnt = read(5, 'operating_points_cnt_minus_1');
    for (let i = 0; i <= opCnt; i++) {
      read(12, `operating_point_idc[${i}]`);
      const lvl = read(5, `seq_level_idx[${i}]`);
      if (lvl > 7) read(1, `seq_tier[${i}]`);
      if (iddp) { if (read(1, `display_delay_present[${i}]`)) read(4, `display_delay_minus1[${i}]`); }
    }
  }

  const fwb = read(4, 'frame_width_bits_minus_1');
  const fhb = read(4, 'frame_height_bits_minus_1');
  const mfw = read(fwb + 1, 'max_frame_width_minus_1');
  const mfh = read(fhb + 1, 'max_frame_height_minus_1');
  console.log(`  → Resolution: ${mfw+1}x${mfh+1}`);

  if (!rsh) {
    if (read(1, 'frame_id_numbers_present')) { read(4, 'delta_frame_id_length_minus_2'); read(3, 'additional_frame_id_length_minus_1'); }
  }

  read(1, 'use_128x128_superblock');
  read(1, 'enable_filter_intra');
  read(1, 'enable_intra_edge_filter');

  if (!rsh) {
    read(1, 'enable_interintra_compound');
    read(1, 'enable_masked_compound');
    read(1, 'enable_warped_motion');
    read(1, 'enable_dual_filter');
    const eoh = read(1, 'enable_order_hint');
    if (eoh) { read(1, 'enable_jnt_comp'); read(1, 'enable_ref_frame_mvs'); }
    const scsct = read(1, 'seq_choose_screen_content_tools');
    const sfsct = scsct ? 2 : read(1, 'seq_force_screen_content_tools');
    console.log(`  → seq_force_screen_content_tools = ${sfsct}`);
    if (sfsct > 0) {
      const scim = read(1, 'seq_choose_integer_mv');
      if (!scim) read(1, 'seq_force_integer_mv');
    }
    if (eoh) read(3, 'order_hint_bits_minus_1');
  }

  read(1, 'enable_superres');
  read(1, 'enable_cdef');
  read(1, 'enable_restoration');

  console.log('  --- color_config ---');
  const hbd = read(1, 'high_bitdepth');
  let twelveBit = 0;
  if (seqProfile === 2 && hbd) twelveBit = read(1, 'twelve_bit');
  const bitDepth = twelveBit ? 12 : (hbd ? 10 : 8);
  console.log(`  → BitDepth = ${bitDepth}`);

  let mono = 0;
  if (seqProfile !== 1) mono = read(1, 'mono_chrome');
  console.log(`  → NumPlanes = ${mono ? 1 : 3}`);

  const cdp = read(1, 'color_description_present_flag');
  let mc = 2;
  if (cdp) {
    read(8, 'color_primaries');
    read(8, 'transfer_characteristics');
    mc = read(8, 'matrix_coefficients');
  }

  if (mono) {
    read(1, 'color_range');
    console.log('  (monochrome — no chroma_sample_position)');
    return;
  }

  if (mc === 0) {
    console.log('  MC_IDENTITY — color_range=1, subsampling=0,0');
    return;
  }

  read(1, 'color_range');

  let sx, sy;
  if (seqProfile === 0) { sx = 1; sy = 1; }
  else if (seqProfile === 1) { sx = 0; sy = 0; }
  else {
    if (bitDepth === 12) { sx = read(1, 'subsampling_x'); sy = sx ? read(1, 'subsampling_y') : 0; }
    else { sx = 1; sy = 0; }
  }
  console.log(`  → subsampling: ${sx},${sy} (${sx&&sy?'4:2:0':sx?'4:2:2':'4:4:4'})`);

  if (sx === 1 && sy === 1) {
    const cspBitPos = bp;
    const csp = read(2, 'chroma_sample_position');
    const cspNames = ['CSP_UNKNOWN', 'CSP_VERTICAL', 'CSP_COLOCATED', 'RESERVED(invalid!)'];
    console.log(`  → chroma_sample_position = ${csp} (${cspNames[csp]}) at bit ${cspBitPos}`);
    if (csp === 3) {
      console.log(`  *** THIS IS THE BUG! chroma_sample_position=3 is RESERVED in AV1 spec.`);
      console.log(`  *** Chrome tolerates it, but Safari/WebKit rejects it.`);
      console.log(`  *** Fix: patch bits ${cspBitPos}-${cspBitPos+1} from 11 to 00 (CSP_UNKNOWN)`);
    }
  }

  read(1, 'separate_uv_delta_q');
  read(1, 'film_grain_params_present');
  console.log(`  Remaining bits in payload: ${(obuData.length * 8) - bp}`);
}

parseSeqHdr(nvencSeqHdr, 'NVENC Depth (1280x720 10-bit)');
parseSeqHdr(swSeqHdr, 'Software Encoder (640x480 8-bit)');
