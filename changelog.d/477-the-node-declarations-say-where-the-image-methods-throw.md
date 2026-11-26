- The Node client's TypeScript declarations say where a subscription's `imageCount`
  answers −1 and where `imageAtIndex` and `images` throw `CavalryError`: from inside a
  handler of that subscription's own poll, and after its own `close()`. `index.js` said
  so since the three became a refusal there, and `index.d.ts`, which a TypeScript
  reader's editor shows, declared them with no comment. No method changes. The
  quickstart runner of each of the five clients now also asks a subscription for its
  images after its close, which until now only the generators' tests held
  ([the Node client's README](clients/node/README.md#what-it-must-never-do)).
