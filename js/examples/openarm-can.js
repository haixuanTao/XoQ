// openarm-can.js — CAN protocol definitions and parsing (pure functions, zero deps)

export const JOINTS = [
  { name: "J1", desc: "Shoulder pan",  canId: 0x11, color: 0xff6b35 },
  { name: "J2", desc: "Shoulder lift", canId: 0x12, color: 0xff8c42 },
  { name: "J3", desc: "Shoulder rot",  canId: 0x13, color: 0xffa94d },
  { name: "J4", desc: "Elbow flex",    canId: 0x14, color: 0xffd166 },
  { name: "J5", desc: "Wrist roll",    canId: 0x15, color: 0x06d6a0 },
  { name: "J6", desc: "Wrist pitch",   canId: 0x16, color: 0x118ab2 },
  { name: "J7", desc: "Wrist rot",     canId: 0x17, color: 0x073b4c },
  { name: "Grip", desc: "Gripper",     canId: 0x18, color: 0x8338ec },
];

export const URDF_PREFIXES = ["L_", "R_"];

// Map CAN ID to joint index (0-7).
// Supports both command range (0x01-0x08) and response range (0x11-0x18).
export function canIdToJointIdx(canId) {
  const lo = canId & 0x0F;
  return (lo >= 1 && lo <= 8) ? lo - 1 : -1;
}

export function makeJointState() {
  return new Array(8).fill(null).map(() => ({
    angle: 0, targetAngle: 0, velocity: 0, torque: 0, tempMos: 0, tempRotor: 0, updated: false,
  }));
}

// Format: [1B flags][4B can_id LE][1B data_len][0-64B data]
export function parseCanFrame(buf) {
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  if (buf.length < 6) return null;

  const flags = buf[0];
  const canId = view.getUint32(1, true);
  const dataLen = buf[5];

  if (buf.length < 6 + dataLen) return null;

  const data = buf.slice(6, 6 + dataLen);
  return { flags, canId, dataLen, data };
}

// Parse ALL CAN frames from a buffer (handles batched MoQ groups)
export function parseAllCanFrames(buf) {
  const frames = [];
  let offset = 0;
  while (offset + 6 <= buf.length) {
    const slice = buf.subarray(offset);
    const dataLen = slice[5];
    if (offset + 6 + dataLen > buf.length) break;
    const view = new DataView(buf.buffer, buf.byteOffset + offset, 6 + dataLen);
    frames.push({
      flags: slice[0],
      canId: view.getUint32(1, true),
      dataLen,
      data: buf.slice(offset + 6, offset + 6 + dataLen),
    });
    offset += 6 + dataLen;
  }
  return frames;
}

// Damiao motor state response parsing
// 8 bytes: [id][pos_h][pos_l][vel_h][vel_l|tau_h][tau_l][t_mos][t_rotor]
export function parseDamiaoState(data) {
  if (data.length < 8) return null;

  const qRaw = (data[1] << 8) | data[2];
  const velRaw = (data[3] << 4) | (data[4] >> 4);
  const tauRaw = ((data[4] & 0x0F) << 8) | data[5];

  const Q_MAX = 12.5;
  const qRad = (qRaw / 65535.0) * (2 * Q_MAX) - Q_MAX;

  const V_MAX = 45.0;
  const vel = (velRaw / 4095.0) * (2 * V_MAX) - V_MAX;

  const T_MAX = 18.0;
  const tau = (tauRaw / 4095.0) * (2 * T_MAX) - T_MAX;

  const tempMos = data[6];
  const tempRotor = data[7];

  return { qRad, vel, tau, tempMos, tempRotor };
}
