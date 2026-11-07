// End-to-end two-participant ephemeral preview relay check against seeded native smoke match,
// speaking the version-2 binary session protocol on the single `/ws` endpoint.
import {
  openSession,
  preview as previewMessage,
  subscribeToMatch,
} from "./pwmtf-session-client.mjs";

const [port, matchId, oneCookie, twoCookie] = process.argv.slice(2);
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
async function connect(cookie, label) {
  const session = await openSession({ port, cookie, label });
  await subscribeToMatch(session, matchId);
  return {
    session,
    send: (payload) => session.send(previewMessage(matchId, payload)),
    wait: (predicate, delay = 800) => session.next(
      (message) => message.type === "preview" && message.matchId === matchId && predicate(message.preview),
      delay,
      "relayed preview",
    ),
    close: () => session.close(),
  };
}
const one = await connect(oneCookie, "player one");
const two = await connect(twoCookie, "player two");
try {
  const first = preview(0, 200, 300);
  const received = two.wait((packet) => packet.equals(first));
  one.send(first);
  await received;
  // An opponent cannot inject a planning update, and a stale revision cannot replace one.
  const injected = one.session.absent((message) => message.type === "preview", 150, "opponent preview echo");
  two.send(preview(1, 600, 700));
  one.send(preview(1, 600, 700, false, 5));
  await injected;
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
  // A preview that fails ShotPreview validation makes the whole session frame
  // malformed: the server closes that socket and relays nothing.
  const malformed = await connect(oneCookie, "player one malformed preview");
  const invalid = preview(3, 800, 900);
  invalid[2] = 3;
  const noInvalid = two.wait((packet) => packet.equals(invalid), 150).then(
    () => { throw new Error("invalid preview relayed"); },
    () => {},
  );
  malformed.send(invalid);
  const closeReason = await Promise.race([
    malformed.session.closed,
    new Promise((resolve) => setTimeout(() => resolve(null), 2_000)),
  ]);
  if (closeReason === null) throw new Error("server kept a socket open after a malformed preview frame");
  await noInvalid;
  await new Promise((resolve) => setTimeout(resolve, 55));
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
  const stale = two.wait(() => true, 120).then(() => { throw new Error("stale preview relayed"); }, () => {});
  one.send(first);
  await stale;
  const next = preview(5, 125, 400);
  const nextSeen = two.wait((packet) => packet.equals(next));
  one.send(next);
  await nextSeen;
  // A reconnect receives only canonical snapshot, not ephemeral planning history.
  two.close();
  const reopened = await connect(twoCookie, "player two reconnect");
  try {
    await reopened.wait(() => true, 125).then(() => { throw new Error("preview replayed on reconnect"); }, () => {});
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
console.log("two-participant preview relay, authorization, order, malformed-frame close and cancel passed");
