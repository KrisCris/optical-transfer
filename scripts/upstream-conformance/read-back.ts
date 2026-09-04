// Can the ACTUAL upstream v0.5.3 read what the Rust port packed?
import { unpackFile, verifyFile } from "./shared/protocol.ts";
import { isSnippet, snippetText } from "./shared/snippet.ts";
import { readFileSync } from "node:fs";

const text = readFileSync(process.argv[2], "utf8");
const cases: Record<string, string>[] = [];
let current: Record<string, string> | null = null;
for (const raw of text.split("\n")) {
  const line = raw.trim();
  if (!line || line.startsWith("#")) continue;
  const i = line.indexOf(" ");
  const key = i < 0 ? line : line.slice(0, i);
  const value = i < 0 ? "" : line.slice(i + 1);
  if (key === "case") current = { id: value };
  else if (key === "end") { cases.push(current!); current = null; }
  else current![key] = value;
}

let failures = 0;
for (const c of cases) {
  const container = Uint8Array.from(Buffer.from(c.container, "hex"));
  try {
    const file = await unpackFile(container);
    const verified = await verifyFile(file);
    const bytesHex = Buffer.from(file.bytes).toString("hex");
    const checks = {
      sha256Verified: verified,
      name: file.name === c.name ? "ok" : `${file.name} != ${c.name}`,
      type: file.type === c.type ? "ok" : `${file.type} != ${c.type}`,
      bytes: bytesHex === c.original ? "ok" : `differ (${file.bytes.length} vs ${c.original.length / 2} bytes)`,
      snippetReadable: isSnippet(file) ? (snippetText(file).length > 0 ? "ok" : "empty") : "n/a",
    };
    const bad = Object.entries(checks).filter(([, v]) => v !== "ok" && v !== "n/a" && v !== true);
    if (bad.length) failures++;
    console.log(`${bad.length ? "FAIL" : "PASS"} ${c.id} ${JSON.stringify(checks)}`);
  } catch (e) {
    failures++;
    console.log(`FAIL ${c.id} threw ${e}`);
  }
}
console.log(failures === 0 ? "ALL PASS" : `${failures} FAILURES`);
process.exit(failures === 0 ? 0 : 1);
