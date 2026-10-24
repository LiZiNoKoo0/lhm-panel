// A server that accepts, reads, and says nothing. Its only job is to make the
// firmware's 15 s silence watchdog (SPEC L-1) fire, which a real daemon never
// does because it sends `data` every second.
//
// Run:  node firmware/mute_server.js 7804
//
// Expected board lines, in order:
//   LINK up
//   LINK tx n=5            (the bench-echo build's PING)
//   ... 15 s of nothing ...
//   LINK silent ms=15000
//   LINK lost idle=15000
//   LINK backoff ms=...
const net = require("net");

const port = Number(process.argv[2] || 7804);
let connections = 0;
const t0 = Date.now();
const stamp = () => ((Date.now() - t0) / 1000).toFixed(2).padStart(7) + "s";

net
  .createServer((socket) => {
    connections += 1;
    const id = connections;
    console.log(`${stamp()} connection ${id} from ${socket.remoteAddress}:${socket.remotePort}`);
    // Read and drop. Never write: a single byte would reset the watchdog.
    socket.on("data", (buf) => {
      console.log(`${stamp()} rx ${buf.length} bytes: ${JSON.stringify(buf.toString("utf8").slice(0, 40))}`);
    });
    socket.on("error", (err) => console.log(`${stamp()} connection ${id} error ${err.code}`));
    socket.on("close", () => console.log(`${stamp()} connection ${id} closed`));
  })
  .listen(port, "0.0.0.0", () => console.log(`${stamp()} mute listening on 0.0.0.0:${port}`));
