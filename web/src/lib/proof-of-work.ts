// The browser's half of the anti-bot challenge asked before a login code is sent
// (crates/core/src/challenge.rs): find the number n, at most `maxNumber`, whose SHA-256 of
// `salt + n` (n in decimal) is `challenge`.
//
// SHA-256 is written out here rather than taken from `crypto.subtle`: that one only exists on
// HTTPS pages, and an installation reached at http://<address>:8480 must work too. It is also
// several times faster for this job, since only the last bytes change from one try to the next.

const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
  0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
  0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
  0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
  0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
  0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
  0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
  0xc67178f2,
]);
const IV = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

/** SHA-256 of `message` (already padded to whole 64-byte blocks) into `out`. */
function compress(message: Uint8Array, out: Uint32Array, w: Uint32Array): void {
  out.set(IV);
  for (let offset = 0; offset < message.length; offset += 64) {
    for (let i = 0; i < 16; i++) {
      const j = offset + i * 4;
      w[i] = (message[j] << 24) | (message[j + 1] << 16) | (message[j + 2] << 8) | message[j + 3];
    }
    for (let i = 16; i < 64; i++) {
      const x = w[i - 15];
      const y = w[i - 2];
      const s0 = ((x >>> 7) | (x << 25)) ^ ((x >>> 18) | (x << 14)) ^ (x >>> 3);
      const s1 = ((y >>> 17) | (y << 15)) ^ ((y >>> 19) | (y << 13)) ^ (y >>> 10);
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) | 0;
    }
    let a = out[0], b = out[1], c = out[2], d = out[3], e = out[4], f = out[5], g = out[6], h = out[7]; // prettier-ignore
    for (let i = 0; i < 64; i++) {
      const s1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
      const t1 = (h + s1 + ((e & f) ^ (~e & g)) + K[i] + w[i]) | 0;
      const s0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
      const t2 = (s0 + ((a & b) ^ (a & c) ^ (b & c))) | 0;
      h = g;
      g = f;
      f = e;
      e = (d + t1) | 0;
      d = c;
      c = b;
      b = a;
      a = (t1 + t2) | 0;
    }
    out[0] += a;
    out[1] += b;
    out[2] += c;
    out[3] += d;
    out[4] += e;
    out[5] += f;
    out[6] += g;
    out[7] += h;
  }
}

/** `prefix` followed by room for `extra` bytes and SHA-256's padding. */
function padded(prefix: Uint8Array, extra: number): Uint8Array {
  const length = prefix.length + extra;
  const message = new Uint8Array(((length + 9 + 63) >> 6) << 6);
  message.set(prefix);
  message[length] = 0x80;
  const bits = length * 8;
  const end = message.length;
  message[end - 4] = bits >>> 24;
  message[end - 3] = (bits >>> 16) & 0xff;
  message[end - 2] = (bits >>> 8) & 0xff;
  message[end - 1] = bits & 0xff;
  return message;
}

/** Lowercase hex SHA-256 of an ASCII string. */
export function sha256Hex(text: string): string {
  const out = new Uint32Array(8);
  compress(padded(new TextEncoder().encode(text), 0), out, new Uint32Array(64));
  return Array.from(out, (word) => word.toString(16).padStart(8, "0")).join("");
}

/** The number the challenge hides, or `null` when none up to `maxNumber` fits. */
export function solveChallenge(salt: string, challenge: string, maxNumber: number): number | null {
  if (!/^[0-9a-f]{64}$/.test(challenge)) return null;
  const target = new Uint32Array(8);
  for (let i = 0; i < 8; i++) target[i] = parseInt(challenge.slice(i * 8, i * 8 + 8), 16);
  const prefix = new TextEncoder().encode(salt);
  const out = new Uint32Array(8);
  const w = new Uint32Array(64);
  const digits = new Uint8Array(20);
  let message = padded(prefix, 1);
  let width = 1;
  for (let n = 0; n <= maxNumber; n++) {
    let count = 0;
    let rest = n;
    do {
      digits[count++] = 48 + (rest % 10);
      rest = Math.floor(rest / 10);
    } while (rest > 0);
    if (count !== width) {
      width = count;
      message = padded(prefix, width);
    }
    for (let i = 0; i < count; i++) message[prefix.length + i] = digits[count - 1 - i];
    compress(message, out, w);
    if (
      out[0] === target[0] &&
      out[1] === target[1] &&
      out[2] === target[2] &&
      out[3] === target[3] &&
      out[4] === target[4] &&
      out[5] === target[5] &&
      out[6] === target[6] &&
      out[7] === target[7]
    ) {
      return n;
    }
  }
  return null;
}
