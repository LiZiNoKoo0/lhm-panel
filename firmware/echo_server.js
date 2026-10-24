// Same contract as echo_server.py, in Node, because on this PC two inbound
// Block rules for python.exe override every port allow rule (Windows: block
// beats allow). Nothing wrong with the Python tool -- it simply cannot accept
// connections here. Keep both so the test survives either kind of machine.
//
// Usage: node firmware/echo_server.js [port]

const net = require("net");

const port = Number(process.argv[2] || 7804);

const server = net.createServer((sock) => {
  const peer = `${sock.remoteAddress}:${sock.remotePort}`;
  console.log(`${peer} connected`);
  let buf = Buffer.alloc(0);
  sock.on("data", (chunk) => {
    buf = Buffer.concat([buf, chunk]);
    let i;
    while ((i = buf.indexOf(0x0a)) >= 0) {
      const line = buf.subarray(0, i);
      buf = buf.subarray(i + 1);
      console.log(`rx ${line.length} bytes: ${JSON.stringify(line.toString("latin1"))}`);
      sock.write(Buffer.concat([line, Buffer.from([0x0a])]));
    }
  });
  sock.on("close", () => console.log(`${peer} closed`));
  sock.on("error", (e) => console.log(`${peer} error: ${e.message}`));
});

server.listen(port, "0.0.0.0", () => {
  console.log(`echo listening on 0.0.0.0:${port} (TCP, newline framed)`);
});
