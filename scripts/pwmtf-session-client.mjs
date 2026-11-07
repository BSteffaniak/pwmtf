// Minimal raw WebSocket client for the PWMTF version-2 session protocol.
// Shared by native smoke scripts; encodes ClientMessage and decodes
// ServerMessage exactly per the binary wire tables (big-endian, tag first),
// failing closed on unknown tags, truncation, or trailing bytes.
import crypto from "node:crypto";
import net from "node:net";

export const SESSION_VERSION = 2;
export const REJECT_REASONS = [
  null,
  "malformed",
  "stale-revision",
  "wrong-turn",
  "match-completed",
  "illegal-move",
  "idempotency-conflict",
  "unavailable",
  "not-subscribed",
];

const U128_MAX = (1n << 128n) - 1n;

function u128(value) {
  const number = BigInt(value);
  if (number < 0n || number > U128_MAX) throw new Error(`u128 out of range: ${value}`);
  const bytes = Buffer.alloc(16);
  bytes.writeBigUInt64BE(number >> 64n, 0);
  bytes.writeBigUInt64BE(number & 0xffff_ffff_ffff_ffffn, 8);
  return bytes;
}

function tagged(tag, ...parts) {
  return Buffer.concat([Buffer.from([tag]), ...parts]);
}

/** `Hello { versions }`: u8 count (1..=8) + u16 each. */
export function hello(versions = [SESSION_VERSION]) {
  const body = Buffer.alloc(1 + versions.length * 2);
  body[0] = versions.length;
  versions.forEach((version, index) => body.writeUInt16BE(version, 1 + index * 2));
  return tagged(0x01, body);
}
export const subscribeMatch = (matchId) => tagged(0x02, u128(matchId));
export const unsubscribeMatch = (matchId) => tagged(0x03, u128(matchId));
export const subscribeLobby = (lobbyId) => tagged(0x04, u128(lobbyId));
export const unsubscribeLobby = (lobbyId) => tagged(0x05, u128(lobbyId));
/** `Command`: u128 match + u16 length + unchanged `CommandEnvelope::to_bytes()`. */
export function command(matchId, envelope) {
  const length = Buffer.alloc(2);
  length.writeUInt16BE(envelope.length);
  return tagged(0x06, u128(matchId), length, Buffer.from(envelope));
}
/** `Preview`: u128 match + the fixed 32-byte `ShotPreview::to_bytes()`. */
export function preview(matchId, previewBytes) {
  if (previewBytes.length !== 32) throw new Error("preview payload must be 32 bytes");
  return tagged(0x07, u128(matchId), Buffer.from(previewBytes));
}
export function ping(nonce) {
  const body = Buffer.alloc(8);
  body.writeBigUInt64BE(BigInt(nonce));
  return tagged(0x08, body);
}

class Reader {
  constructor(bytes) {
    this.bytes = bytes;
    this.offset = 0;
  }
  take(length) {
    if (this.offset + length > this.bytes.length) throw new Error("truncated server message");
    const slice = this.bytes.subarray(this.offset, this.offset + length);
    this.offset += length;
    return slice;
  }
  u8() { return this.take(1)[0]; }
  u16() { return this.take(2).readUInt16BE(0); }
  u32() { return this.take(4).readUInt32BE(0); }
  u64() { return this.take(8).readBigUInt64BE(0); }
  u128() {
    const bytes = this.take(16);
    return ((bytes.readBigUInt64BE(0) << 64n) | bytes.readBigUInt64BE(8)).toString();
  }
  player() {
    const value = this.u8();
    if (value !== 1 && value !== 2) throw new Error(`invalid player ${value}`);
    return value;
  }
  flag() {
    const value = this.u8();
    if (value > 1) throw new Error(`invalid option flag ${value}`);
    return value === 1;
  }
  finish() {
    if (this.offset !== this.bytes.length) {
      throw new Error(`server message has ${this.bytes.length - this.offset} trailing bytes`);
    }
  }
}

function decodeCommand(reader) {
  const kind = reader.u8();
  if (kind === 1) {
    return {
      kind: "play-shot",
      shotVersion: reader.u16(),
      aim: reader.u16(),
      power: reader.u16(),
      spinSide: reader.take(2).readInt16BE(0),
      spinVertical: reader.take(2).readInt16BE(0),
      calledPocket: reader.u8(),
    };
  }
  if (kind === 2) return { kind: "timeout" };
  if (kind === 3) return { kind: "concede", player: reader.player() };
  if (kind === 4) {
    return {
      kind: "place-cue-ball",
      x: reader.take(8).readBigInt64BE(0),
      y: reader.take(8).readBigInt64BE(0),
    };
  }
  throw new Error(`invalid command tag ${kind}`);
}

/** Decodes one binary server frame; throws on any malformed input. */
export function decodeServerMessage(bytes) {
  const reader = new Reader(bytes);
  const tag = reader.u8();
  let message;
  switch (tag) {
    case 0x81:
      message = { type: "welcome", version: reader.u16(), serverNowMs: reader.u64() };
      break;
    case 0x82: {
      const matchId = reader.u128();
      const seat = reader.player();
      const revision = Number(reader.u64());
      const checksum = reader.u64();
      const deadlineAtMs = reader.flag() ? Number(reader.u64()) : null;
      const serverNowMs = Number(reader.u64());
      let lastCommand = null;
      if (reader.flag()) {
        const commandId = reader.take(16).toString("hex");
        const actor = reader.player();
        const version = reader.u16();
        const decoded = decodeCommand(reader);
        lastCommand = { commandId, actor, version, command: decoded, playbackTicks: reader.u32() };
      }
      const size = reader.u32();
      if (size > 4096) throw new Error(`snapshot length ${size} exceeds its bound`);
      const snapshot = Buffer.from(reader.take(size));
      message = {
        type: "match-state", matchId, seat, revision, checksum, deadlineAtMs, serverNowMs, lastCommand, snapshot,
      };
      break;
    }
    case 0x83: {
      const matchId = reader.u128();
      const commandId = reader.take(16).toString("hex");
      const code = reader.u8();
      const reason = REJECT_REASONS[code];
      if (!reason) throw new Error(`invalid reject reason ${code}`);
      message = { type: "rejected", matchId, commandId, reason };
      break;
    }
    case 0x84:
      message = { type: "preview", matchId: reader.u128(), preview: Buffer.from(reader.take(32)) };
      break;
    case 0x85:
      message = { type: "pong", nonce: reader.u64(), serverNowMs: reader.u64() };
      break;
    case 0x86:
      message = { type: "lobby-changed", lobbyId: reader.u128() };
      break;
    case 0x87:
      message = { type: "match-started", lobbyId: reader.u128(), matchId: reader.u128() };
      break;
    case 0x88:
      message = { type: "social-changed" };
      break;
    case 0x89: {
      const code = reader.u8();
      if (code !== 1 && code !== 2) throw new Error(`invalid subscription kind ${code}`);
      message = { type: "subscription-denied", kind: code === 1 ? "match" : "lobby", id: reader.u128() };
      break;
    }
    default:
      throw new Error(`unknown server message tag 0x${tag.toString(16)}`);
  }
  reader.finish();
  return message;
}

/** Masks one client WebSocket frame with any opcode and payload size. */
export function clientFrame(opcode, payload) {
  const body = Buffer.from(payload);
  const mask = crypto.randomBytes(4);
  let header;
  if (body.length < 126) {
    header = Buffer.from([0x80 | opcode, 0x80 | body.length]);
  } else if (body.length <= 0xffff) {
    header = Buffer.alloc(4);
    header[0] = 0x80 | opcode;
    header[1] = 0x80 | 126;
    header.writeUInt16BE(body.length, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x80 | opcode;
    header[1] = 0x80 | 127;
    header.writeBigUInt64BE(BigInt(body.length), 2);
  }
  const masked = Buffer.alloc(body.length);
  for (let index = 0; index < body.length; index += 1) masked[index] = body[index] ^ mask[index % 4];
  return Buffer.concat([header, mask, masked]);
}

function parseFrames(buffer) {
  const frames = [];
  let offset = 0;
  while (buffer.length - offset >= 2) {
    const opcode = buffer[offset] & 0x0f;
    let length = buffer[offset + 1] & 0x7f;
    let header = 2;
    if (length === 126) {
      if (buffer.length - offset < 4) break;
      length = buffer.readUInt16BE(offset + 2);
      header = 4;
    } else if (length === 127) {
      if (buffer.length - offset < 10) break;
      const large = buffer.readBigUInt64BE(offset + 2);
      if (large > 1_048_576n) throw new Error("oversized WebSocket frame");
      length = Number(large);
      header = 10;
    }
    if (buffer.length - offset < header + length) break;
    frames.push({ opcode, payload: buffer.subarray(offset + header, offset + header + length) });
    offset += header + length;
  }
  return { frames, remaining: buffer.subarray(offset) };
}

/**
 * Opens a raw upgrade to `/ws`. Resolves `{ status, body }` for a refused
 * upgrade, or `{ status: 101, client }` with a message-queue client.
 */
export function connect({ port, cookie = null, origin = "https://pwmtf.hyperchad.dev", path = "/ws", label = "session" }) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(Number(port), "127.0.0.1");
    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error(`${label} upgrade timed out`));
    }, 5_000);
    let buffer = Buffer.alloc(0);
    let upgraded = false;
    const queue = [];
    const waiters = [];
    let closedResolve;
    const closed = new Promise((done) => { closedResolve = done; });
    let isClosed = false;
    const markClosed = (reason) => {
      if (isClosed) return;
      isClosed = true;
      closedResolve(reason);
      for (const waiter of waiters.splice(0)) waiter.fail(new Error(`${label} closed while waiting: ${reason}`));
    };
    const deliver = (message) => {
      const index = waiters.findIndex((waiter) => waiter.predicate(message));
      if (index >= 0) {
        const [waiter] = waiters.splice(index, 1);
        waiter.done(message);
      } else {
        queue.push(message);
      }
    };
    const client = {
      label,
      closed,
      get isClosed() { return isClosed; },
      send(bytes) { socket.write(clientFrame(2, bytes)); },
      sendText(text) { socket.write(clientFrame(1, Buffer.from(text))); },
      sendRaw(bytes) { socket.write(bytes); },
      /** Resolves with the next queued or future message matching `predicate`. */
      next(predicate = () => true, timeout = 5_000, what = "server message") {
        const index = queue.findIndex(predicate);
        if (index >= 0) return Promise.resolve(queue.splice(index, 1)[0]);
        if (isClosed) return Promise.reject(new Error(`${label} is closed; expected ${what}`));
        return new Promise((done, fail) => {
          const waiter = {
            predicate,
            done: (message) => { clearTimeout(waiter.timer); done(message); },
            fail: (error) => { clearTimeout(waiter.timer); fail(error); },
          };
          waiter.timer = setTimeout(() => {
            waiters.splice(waiters.indexOf(waiter), 1);
            fail(new Error(`${label} timed out waiting for ${what}`));
          }, timeout);
          waiters.push(waiter);
        });
      },
      /** Resolves true if no matching message arrives within `timeout`. */
      async absent(predicate, timeout, what) {
        try {
          await client.next(predicate, timeout, what);
        } catch {
          return true;
        }
        throw new Error(`${label} unexpectedly received ${what}`);
      },
      drain() { return queue.splice(0); },
      close() { socket.destroy(); markClosed("closed locally"); },
    };
    socket.on("connect", () => {
      const headers = [
        `GET ${path} HTTP/1.1`,
        `Host: 127.0.0.1:${port}`,
        "Upgrade: websocket",
        "Connection: Upgrade",
        `Sec-WebSocket-Key: ${crypto.randomBytes(16).toString("base64")}`,
        "Sec-WebSocket-Version: 13",
        `Origin: ${origin}`,
        "User-Agent: pwmtf-native-smoke",
      ];
      if (cookie !== null) headers.push(`Cookie: ${cookie}`);
      socket.write([...headers, "", ""].join("\r\n"));
    });
    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      if (!upgraded) {
        const end = buffer.indexOf("\r\n\r\n");
        if (end < 0) return;
        const response = buffer.subarray(0, end).toString();
        const status = Number(response.split(" ")[1]);
        if (status !== 101) {
          const contentLength = Number(response.match(/content-length: (\d+)/iu)?.[1] ?? 0);
          if (buffer.length < end + 4 + contentLength) return;
          clearTimeout(timer);
          const body = buffer.subarray(end + 4, end + 4 + contentLength).toString();
          socket.destroy();
          resolve({ status, body });
          return;
        }
        upgraded = true;
        buffer = buffer.subarray(end + 4);
        clearTimeout(timer);
        resolve({ status, client });
      }
      let parsed;
      try {
        parsed = parseFrames(buffer);
      } catch (error) {
        socket.destroy();
        markClosed(error.message);
        return;
      }
      buffer = parsed.remaining;
      for (const frame of parsed.frames) {
        if (frame.opcode === 8) {
          markClosed("close frame");
          socket.end();
        } else if (frame.opcode === 9) {
          socket.write(clientFrame(10, frame.payload));
        } else if (frame.opcode === 2) {
          try {
            deliver(decodeServerMessage(frame.payload));
          } catch (error) {
            socket.destroy();
            markClosed(`invalid server message: ${error.message}`);
            reject(error);
          }
        } else if (frame.opcode === 1) {
          socket.destroy();
          markClosed("server sent a text frame");
        }
      }
    });
    socket.on("close", () => markClosed("socket closed"));
    socket.on("error", (error) => {
      clearTimeout(timer);
      markClosed(error.message);
      if (!upgraded) reject(error);
    });
  });
}

/** Connects, sends `Hello`, and requires a version-2 `Welcome`. */
export async function openSession(options) {
  const result = await connect(options);
  if (result.status !== 101) {
    throw new Error(`${options.label ?? "session"} upgrade failed with ${result.status}: ${result.body}`);
  }
  const { client } = result;
  client.send(hello());
  const welcome = await client.next((message) => message.type === "welcome", 5_000, "Welcome");
  if (welcome.version !== SESSION_VERSION) throw new Error(`negotiated unexpected version ${welcome.version}`);
  return client;
}

/** Subscribes to a match and returns its initial `MatchState`. */
export async function subscribeToMatch(client, matchId) {
  client.send(subscribeMatch(matchId));
  const state = await client.next(
    (message) => message.type === "match-state" && message.matchId === String(matchId),
    5_000,
    `MatchState for ${matchId}`,
  );
  if (state.lastCommand !== null) throw new Error("subscription MatchState carried a last command");
  return state;
}
