import assert from "node:assert";
import { JamRoom } from "./src/index.js";

// A minimal stand-in for the Durable Object runtime.
class FakeSocket {
  constructor(room) { this.room = room; this.sent = []; this.attachment = null; this.closed = null; }
  send(data) { if (this.closed) throw new Error("closed"); this.sent.push(JSON.parse(data)); }
  serializeAttachment(v) { this.attachment = structuredClone(v); }
  deserializeAttachment() { return this.attachment; }
  close(code) { this.closed = code; this.room._sockets = this.room._sockets.filter((s) => s !== this); }
  last(type) { return [...this.sent].reverse().find((m) => m.type === type); }
}
const store = new Map();
const ctx = {
  _sockets: [],
  acceptWebSocket(ws) { this._sockets.push(ws); },
  getWebSockets() { return this._sockets; },
  storage: {
    async get(k) { return structuredClone(store.get(k)); },
    async put(k, v) { if (typeof k === "object") { for (const [a, b] of Object.entries(k)) store.set(a, structuredClone(b)); } else store.set(k, structuredClone(v)); },
    async deleteAll() { store.clear(); },
  },
};
const room = new JamRoom(ctx, {});
function connect() { const ws = new FakeSocket(ctx); ctx.acceptWebSocket(ws); ws.serializeAttachment({ id: null, name: null, host: false }); return ws; }
const say = (ws, m) => room.webSocketMessage(ws, JSON.stringify(m));

// Joining before a host fails.
const early = connect();
await say(early, { type: "hello", name: "Early", host: false });
assert.equal(early.last("error").code, "missing");

const host = connect();
await say(host, { type: "hello", name: "Nick", id: "h", host: true });
assert.equal(host.last("welcome").host, true);

const second = connect();
await say(second, { type: "hello", name: "Other", host: true });
assert.equal(second.last("error").code, "taken", "one host per code");

const guest = connect();
await say(guest, { type: "hello", name: "Sam", id: "g" });
const welcome = guest.last("welcome");
assert.equal(welcome.members.length, 2);
assert.equal(host.last("members").members.length, 2, "host hears who joined");

await say(host, { type: "state", track: { uri: "spotify:track:A", title: "A", artist: "X", duration_ms: 1000 }, position_ms: 500, playing: true });
const st = guest.last("state");
assert.equal(st.track.uri, "spotify:track:A");
assert.ok(st.server_ms > 0 && st.seq === 1);
await say(guest, { type: "state", track: { uri: "spotify:track:EVIL" }, playing: true });
assert.equal(guest.last("state").seq, 1, "only the host sets the state");

// A late joiner gets the state and queue.
await say(guest, { type: "queue_add", uri: "spotify:track:B", title: "B", artist: "Y" });
assert.equal(host.last("queued").item.by, "Sam");
const late = connect();
await say(late, { type: "hello", name: "Late" });
assert.equal(late.last("welcome").state.track.uri, "spotify:track:A");
assert.equal(late.last("welcome").queue.length, 1);

// Guests' commands reach the host only.
await say(guest, { type: "command", action: { kind: "next" } });
assert.deepEqual(host.last("command"), { type: "command", from: "Sam", action: { kind: "next" } });
assert.equal(late.last("command"), undefined);

await say(host, { type: "queue_done", id: host.last("queued").item.id });
assert.equal(guest.last("queue").items.length, 0);

await say(guest, { type: "ping", t: 7 });
assert.equal(guest.last("pong").t, 7);

// The guest leaves; then the host ends the Jam for everyone.
await room.webSocketClose(guest);
ctx._sockets = ctx._sockets.filter((s) => s !== guest);
assert.equal(host.last("members").members.length, 2);
await room.webSocketClose(host);
assert.equal(late.last("ended").type, "ended");
assert.equal(late.closed, 4000);
assert.equal(store.size, 0);
console.log("relay ok");
