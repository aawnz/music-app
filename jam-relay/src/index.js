// Spotifast Jam relay.
//
// Each Jam is one Durable Object, named by its code. Apps connect to
// wss://<relay>/jam/<CODE> and exchange small JSON messages; the room keeps
// what a newcomer needs (the host's last state, the shared queue, who is
// here) and passes everything else on. No audio and no Spotify credentials
// ever pass through here: each app streams from its own account.

const CODE = /^[A-Z0-9]{6}$/;
const MAX_MEMBERS = 32;
const MAX_MESSAGE = 4096;
const MAX_QUEUE = 200;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/" || url.pathname === "/health") {
      return new Response("Spotifast Jam relay\n", { headers: { "content-type": "text/plain" } });
    }
    const match = url.pathname.match(/^\/jam\/([A-Za-z0-9]{6})$/);
    if (!match) return new Response("Not found", { status: 404 });
    if (request.headers.get("Upgrade") !== "websocket") {
      return new Response("Expected a WebSocket", { status: 426 });
    }
    const code = match[1].toUpperCase();
    if (!CODE.test(code)) return new Response("Bad code", { status: 400 });
    const room = env.ROOMS.get(env.ROOMS.idFromName(code));
    return room.fetch(request);
  },
};

function text(value, limit) {
  return typeof value === "string" ? value.slice(0, limit) : "";
}

function number(value) {
  return Number.isFinite(value) ? Math.max(0, Math.floor(value)) : 0;
}

export class JamRoom {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
  }

  async fetch(request) {
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    // Hibernation keeps idle Jams free: the room sleeps between messages.
    this.ctx.acceptWebSocket(server);
    server.serializeAttachment({ id: null, name: null, host: false });
    return new Response(null, { status: 101, webSocket: client });
  }

  sockets() {
    return this.ctx.getWebSockets();
  }

  members() {
    const list = [];
    for (const ws of this.sockets()) {
      const who = ws.deserializeAttachment();
      if (who && who.id) list.push({ id: who.id, name: who.name, host: who.host });
    }
    return list;
  }

  hostSocket() {
    return this.sockets().find((ws) => ws.deserializeAttachment()?.host);
  }

  send(ws, message) {
    try {
      ws.send(JSON.stringify(message));
    } catch {
      // A socket closing as we send: its close event tidies up.
    }
  }

  broadcast(message, except) {
    const data = JSON.stringify(message);
    for (const ws of this.sockets()) {
      if (ws === except || !ws.deserializeAttachment()?.id) continue;
      try {
        ws.send(data);
      } catch {}
    }
  }

  async queue() {
    return (await this.ctx.storage.get("queue")) || [];
  }

  async webSocketMessage(ws, raw) {
    if (typeof raw !== "string" || raw.length > MAX_MESSAGE) return;
    let message;
    try {
      message = JSON.parse(raw);
    } catch {
      return;
    }
    const who = ws.deserializeAttachment() || {};
    const now = Date.now();

    if (message.type === "ping") {
      this.send(ws, { type: "pong", t: message.t, server_ms: now });
      return;
    }

    if (message.type === "hello") {
      if (who.id) return;
      const id = text(message.id, 64) || crypto.randomUUID();
      const name = text(message.name, 40) || "Listener";
      const wantsHost = message.host === true;
      const host = this.hostSocket();
      if (wantsHost && host) {
        this.send(ws, { type: "error", code: "taken", message: "That Jam code is in use." });
        ws.close(4001, "taken");
        return;
      }
      if (!wantsHost && !host) {
        this.send(ws, { type: "error", code: "missing", message: "No Jam with that code is running." });
        ws.close(4004, "missing");
        return;
      }
      if (this.members().length >= MAX_MEMBERS) {
        this.send(ws, { type: "error", code: "full", message: "That Jam is full." });
        ws.close(4003, "full");
        return;
      }
      if (wantsHost) {
        // A new Jam starts clean.
        await this.ctx.storage.deleteAll();
      }
      ws.serializeAttachment({ id, name, host: wantsHost });
      this.send(ws, {
        type: "welcome",
        id,
        server_ms: now,
        host: wantsHost,
        state: (await this.ctx.storage.get("state")) || null,
        queue: await this.queue(),
        members: this.members(),
      });
      this.broadcast({ type: "members", members: this.members() }, ws);
      return;
    }

    if (!who.id) return;

    if (message.type === "state" && who.host) {
      const track = message.track && typeof message.track === "object" ? message.track : null;
      const seq = number((await this.ctx.storage.get("seq")) || 0) + 1;
      const state = {
        track: track && {
          uri: text(track.uri, 512),
          title: text(track.title, 200),
          artist: text(track.artist, 200),
          duration_ms: number(track.duration_ms),
        },
        position_ms: number(message.position_ms),
        playing: message.playing === true,
        server_ms: now,
        seq,
      };
      await this.ctx.storage.put({ state, seq });
      this.broadcast({ type: "state", ...state }, ws);
      return;
    }

    if (message.type === "command" && !who.host) {
      const host = this.hostSocket();
      const action = message.action && typeof message.action === "object" ? message.action : null;
      if (host && action) this.send(host, { type: "command", from: who.name, action });
      return;
    }

    if (message.type === "queue_add") {
      const uri = text(message.uri, 512);
      if (!uri) return;
      const queue = await this.queue();
      if (queue.length >= MAX_QUEUE) return;
      const item = {
        id: crypto.randomUUID(),
        uri,
        title: text(message.title, 200),
        artist: text(message.artist, 200),
        by: who.name,
      };
      queue.push(item);
      await this.ctx.storage.put("queue", queue);
      this.broadcast({ type: "queued", item });
      this.broadcast({ type: "queue", items: queue });
      return;
    }

    if (message.type === "queue_done" && who.host) {
      const id = text(message.id, 64);
      const queue = (await this.queue()).filter((item) => item.id !== id);
      await this.ctx.storage.put("queue", queue);
      this.broadcast({ type: "queue", items: queue });
      return;
    }
  }

  async webSocketClose(ws) {
    await this.left(ws);
  }

  async webSocketError(ws) {
    await this.left(ws);
  }

  async left(ws) {
    const who = ws.deserializeAttachment();
    if (!who || !who.id) return;
    ws.serializeAttachment({ id: null, name: null, host: false });
    if (who.host) {
      // The Jam ends with its host, as in Spotify.
      this.broadcast({ type: "ended", message: `${who.name} ended the Jam.` });
      for (const other of this.sockets()) {
        if (other !== ws) {
          try {
            other.close(4000, "ended");
          } catch {}
        }
      }
      await this.ctx.storage.deleteAll();
      return;
    }
    this.broadcast({ type: "members", members: this.members() });
  }
}
