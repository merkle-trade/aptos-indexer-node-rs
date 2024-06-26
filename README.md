# @merkletrade/aptos-indexer-node-rs

A tiny Aptos GRPC indexer native binding for Nodejs. The GRPC receive stream is handled in Rust for performance, resulting in up to 8x better throughput than using grpc-js (vanilla JS).

It also provides basic transaction filtering and auto-reconnection on intermittent network failures.

### Basic Usage

```ts
import {
  startFetchTransactions,
  nextTransactions,
} from "@merkletrade/aptos-indexer-node-rs";

const ch = await startFetchTransactions(
  "https://grpc.mainnet.aptoslabs.com",
  "...", // authKey (optional)
  200_000_000n, // startVersion
  300_000_000n, // endVersion (optional)
  { focusContractAddresses: ["0xabc...def"] } // address to filter
);

while (true) {
  const resp = await nextTransactions(ch).then((v) => v && JSON.parse(v));
  if (!resp) return;

  // process txs
  console.log(resp.transactions);
}
```

### Usage with Node Stream

```ts
import { Readable } from "node:stream";
import {
  startFetchTransactions,
  nextTransactions,
} from "@merkletrade/aptos-indexer-node-rs";

const BUFFER_SIZE = 10;

const fetchTxs = async function* () {
  const ch = await startFetchTransactions(
    "https://grpc.mainnet.aptoslabs.com",
    "...", // authKey (optional)
    200_000_000n, // startVersion
    300_000_000n, // endVersion (optional)
    { focusContractAddresses: ["0xabc...def"] } // address to filter
  );

  while (true) {
    const resp = await nextTransactions(ch).then((v) => v && JSON.parse(v));
    if (!resp) return;
    yield resp;
  }
};

for await (const resp of Readable.from(fetchTxs(), { highWaterMark: BUFFER_SIZE })) {
  // process txs
  console.log(resp.transactions);
}
```
