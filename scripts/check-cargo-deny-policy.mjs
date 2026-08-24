#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("../", import.meta.url));
const exception = {
  name: "bsv-sdk",
  version: "0.1.75",
  checksum: "9ec6257d7be0db319c39d6304c8bedc4a90dcbd5cdae204754b2d493da9eaadc",
  licensePath: "licenses/Open-BSV-License-v5.txt",
  licenseSha256:
    "27135868feaeb18396c2bd7ffda5eeee58058c1a80db0a85aaead9e40f4e44a3",
};

const fail = (message, output = "") => {
  if (output) process.stderr.write(output);
  console.error(`dependency-policy: ${message}`);
  process.exit(1);
};

const cargoLock = readFileSync(`${repoRoot}/Cargo.lock`, "utf8");
const packageBlocks = cargoLock.split(/\n(?=\[\[package\]\]\n)/u);
const sdkBlock = packageBlocks.find(
  (block) =>
    block.includes(`name = "${exception.name}"`) &&
    block.includes(`version = "${exception.version}"`),
);
if (!sdkBlock || !sdkBlock.includes(`checksum = "${exception.checksum}"`)) {
  fail(
    `${exception.name}@${exception.version} must remain locked to the reviewed crate checksum`,
  );
}

const licenseBytes = readFileSync(`${repoRoot}/${exception.licensePath}`);
const licenseSha256 = createHash("sha256").update(licenseBytes).digest("hex");
if (licenseSha256 !== exception.licenseSha256) {
  fail(`${exception.licensePath} no longer matches the reviewed Open BSV v5 text`);
}

// cargo-deny cannot express non-SPDX Open BSV v5 in its allow list. Downgrade
// only the generic `unlicensed` lint, then fail below unless every such
// diagnostic names the one exact, checksum-pinned SDK reviewed above. All
// ordinary license rejection, advisory, ban, and source lints remain enforced.
const result = spawnSync(
  "cargo-deny",
  ["--format", "json", "--locked", "check", "-W", "unlicensed"],
  { cwd: repoRoot, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
);
const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
const observedUnlicensed = new Set();

for (const line of output.split("\n")) {
  if (!line.startsWith("{")) continue;
  let record;
  try {
    record = JSON.parse(line);
  } catch {
    continue;
  }
  if (record?.type !== "diagnostic" || record?.fields?.code !== "unlicensed") {
    continue;
  }
  for (const graph of record.fields.graphs ?? []) {
    const crate = graph.Krate;
    if (crate?.name && crate?.version) {
      observedUnlicensed.add(`${crate.name}@${crate.version}`);
    }
  }
}

const allowedKey = `${exception.name}@${exception.version}`;
const unexpected = [...observedUnlicensed].filter((key) => key !== allowedKey);
if (result.status !== 0) {
  fail(`cargo-deny exited with status ${result.status}`, output);
}
if (unexpected.length > 0) {
  fail(`unexpected unlicensed crates: ${unexpected.join(", ")}`, output);
}
if (!observedUnlicensed.has(allowedKey)) {
  fail(`reviewed exception ${allowedKey} was not reported; remove or update the exception`);
}

console.log(
  `dependency policy passed; reviewed Open BSV exception: ${allowedKey} (${exception.checksum})`,
);
