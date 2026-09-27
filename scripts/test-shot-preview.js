// End-to-end two-participant ephemeral preview relay check against seeded native smoke match.
import net from "node:net";
import crypto from "node:crypto";

const [port, matchId, oneCookie, twoCookie] = process.argv.slice(2);
function frame(opcode, payload) {
  const mask = crypto.randomBytes(4);
  const bytes = Buffer.alloc(6 + payload.length);
  bytes[0] = 0x80 | opcode;
  bytes[1] = 0x80 | payload.length;
  mask.copy(bytes, 2);
  for (let i = 0; i < payload.length; i += 1) bytes[i + 6] = payload[i] ^ mask[i % 4];
  return bytes;
}
function preview(sequence, angle, power, cancel = false, revision = 0) {
  const bytes = Buffer.alloc(32);
  bytes[0] = 0xff;
  bytes.writeUInt16BE(2, 1);
  bytes.writeBigUInt64BE(BigInt(revision), 3);
  bytes.writeBigUInt64BE(987n, 11);
  bytes.writeUInt32BE(sequence, 19);
  bytes.writeUInt16BE(angle, 23);
  bytes.writeUInt16BE(power, 25);
  bytes[27] = Number(cancel);
  // The remaining four bytes encode the bounded canonical spin contact point.
  return bytes;
}
async function connect(cookie) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(Number(port), "127.0.0.1");
    const timer = setTimeout(() => reject(new Error("preview handshake timed out")), 4000);
    let data = Buffer.alloc(0);
    let stage = 0;
    const listeners = [];
    const client = {
      send: (payload) => socket.write(frame(2, payload)),
      wait: (predicate, delay = 800) => new Promise((yes, no) => {
        const timeout = setTimeout(() => { listeners.splice(listeners.indexOf(watch), 1); no(new Error("expected preview not relayed")); }, delay);
        function watch(packet) {
          if (!predicate(packet)) return;
          clearTimeout(timeout);
          listeners.splice(listeners.indexOf(watch), 1);
          yes(packet);
        }
        listeners.push(watch);
      }),
      close: () => socket.destroy(),
    };
    socket.on("error", reject);
    socket.on("connect", () => socket.write([
      `GET /ws?match_id=${matchId} HTTP/1.1`, `Host: 127.0.0.1:${port}`,
      "Upgrade: websocket", "Connection: Upgrade",
      `Sec-WebSocket-Key: ${crypto.randomBytes(16).toString("base64")}`,
      "Sec-WebSocket-Version: 13", "Origin: https://pwmtf.hyperchad.dev",
      `Cookie: ${cookie}`, "", "",
    ].join("\r\n")));
    socket.on("data", (chunk) => {
      data = Buffer.concat([data, chunk]);
      if (stage === 0) {
        const end = data.indexOf("\r\n\r\n");
        if (end < 0) return;
        if (!data.toString("utf8", 0, end).startsWith("HTTP/1.1 101")) { reject(new Error("upgrade rejected")); return; }
        data = data.subarray(end + 4);
        stage = 1;
        socket.write(frame(1, Buffer.from("1")));
      }
      while (data.length >= 2) {
        const length = data[1] & 0x7f;
        const header = length === 126 ? 4 : 2;
        if (data.length < header) break;
        const size = length === 126 ? data.readUInt16BE(2) : length;
        if (data.length < header + size) break;
        const opcode = data[0] & 15;
        const payload = data.subarray(header, header + size);
        data = data.subarray(header + size);
        if (stage === 1 && opcode === 1 && payload.toString() === "1") stage = 2;
        else if (stage === 2 && opcode === 2) {
          if (payload[0] !== 0xff) { clearTimeout(timer); stage = 3; resolve(client); }
        } else if (stage === 3 && opcode === 2) for (const listener of [...listeners]) listener(payload);
      }
    });
  });
}
const one = await connect(oneCookie);
const two = await connect(twoCookie);
try {
  const first = preview(0, 200, 300);
  const received = two.wait((packet) => packet.equals(first));
  one.send(first);
  await received;
  // An opponent cannot inject a planning update, and a stale revision cannot replace one.
  two.send(preview(1, 600, 700));
  one.send(preview(1, 600, 700, false, 5));
  const second = preview(1, 800, 900);
  const changed = two.wait((packet) => packet.equals(second));
  await new Promise((resolve) => setTimeout(resolve, 55));
  one.send(second);
  await changed;
  const throttled = preview(2, 400, 500);
  const noBurst = two.wait((packet) => packet.equals(throttled), 120).then(
    () => { throw new Error("unthrottled preview relayed"); },
    () => {},
  );
  one.send(throttled);
  await noBurst;
  const invalid = preview(3, 800, 900);
  invalid[2] = 3;
  one.send(invalid);
  const clear = preview(3, 800, 900, true);
  const cleared = two.wait((packet) => packet.equals(clear));
  one.send(clear);
  await cleared;
  const cancelFlood = preview(4, 800, 900, true);
  const noCancelFlood = two.wait((packet) => packet.equals(cancelFlood), 120).then(
    () => { throw new Error("repeated cancel bypassed preview throttle"); },
    () => {},
  );
  one.send(cancelFlood);
  await noCancelFlood;
  const stale = two.wait((packet) => packet[0] === 0xff, 120).then(() => { throw new Error("stale preview relayed"); }, () => {});
  one.send(first);
  await stale;
  const next = preview(5, 125, 400);
  const nextSeen = two.wait((packet) => packet.equals(next));
  one.send(next);
  await nextSeen;
  // A reconnect receives only canonical snapshot, not ephemeral planning history.
  two.close();
  const reopened = await connect(twoCookie);
  try {
    const noReplay = reopened.wait((packet) => packet[0] === 0xff, 125).then(() => { throw new Error("preview replayed on reconnect"); }, () => {});
    await noReplay;
    const resumed = preview(6, 222, 555);
    const resumedSeen = reopened.wait((packet) => packet.equals(resumed));
    await new Promise((resolve) => setTimeout(resolve, 55));
    one.send(resumed);
    await resumedSeen;
  } finally { reopened.close(); }
} finally {
  one.close();
  two.close();
}
console.log("two-participant preview relay, authorization, order and cancel passed");
